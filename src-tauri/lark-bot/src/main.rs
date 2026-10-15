//! 飞书 bot sidecar —— `docs/LARK_BOT.md` P1 的 bot 本体（**先做纯文本回复**）。
//!
//! 它是**独立进程**，只经 webapi 访问本库（§4.1 的数据流），因此：
//!
//! ```text
//! 消费 lark-cli 的 NDJSON 事件
//!   → §7.1 准入判定（`lark-gate` crate：L0 来源 / L2 类型 / L1 白名单）
//!   → /api/lark/seen 去重（按 message_id）
//!   → /api/lark/binding 取/建 chat_id ↔ session_id 绑定
//!   → /api/chat/sessions/{id}?origin=lark 取会话、带上历史
//!   → **回复一张"检索中"卡片** → 消费 `POST /api/ai/conversation/ask/stream`（SSE）
//!     → **节流**更新卡片（§8.1：约束是频率而非次数） → 收尾成最终卡片
//!     →（卡片发不出去就**退回纯文本**：`/api/ai/conversation/ask` + `im +messages-reply --text`）
//!   → 把这一轮落回会话（origin=lark）
//! ```
//!
//! 卡片的渲染/节流/SSE 分帧在 [`card`] 模块（纯逻辑，可单测）。
//!
//! ## 为什么是**独立 crate**（而不是主程序包里的一个 bin）
//!
//! 把 bot 装进安装包要用 `bundle.externalBin`，而 `tauri-build` **在编译期校验**
//! externalBin：那个文件不存在就直接构建失败。若 bot 只是主程序的一个 bin，那么
//! "生成该文件"就必须先编主程序 —— **鸡生蛋**（2026-10-15 实测撞上，见 §5.7）。
//!
//! 拆成独立 crate 且**只依赖 `lark-gate`** 之后，`cargo build -p lark-bot` **完全不触碰
//! 主程序**，环就断了；顺带的好处是不再链 onnxruntime / tantivy / Tauri，
//! 体积从 30 MB 降到几 MB。
//!
//! ## 因此这里**不 import `link_searcher_lib`**
//!
//! 一旦引应用 lib，等于把整个应用链回来（体积、编译时间、鸡生蛋全部回来）。三处替代：
//!
//! - **准入判定** → `lark-gate` crate（策略只有一份实现、一份测试）
//! - **会话 `messages`** → 当**不透明 `serde_json::Value`** 原样透传。不结构化解析：
//!   字段一旦与服务端不一致，结构化解析会**静默把历史丢空**（表现为"它忘了前面说过什么"），
//!   而透传最多被服务端拒绝 —— 那是能看见的错。
//! - **`web_api_token()`** → 本文件内的小函数（读 `config.json`，路径规则与
//!   `config.rs::config_dir` 一致），见 [`web_api_token`]
//!
//! ## 其它约束
//!
//! - **stderr 先就绪、stdout 再解析**：lark-cli 的契约是"stderr 出现 `[event] ready event_key=`
//!   之后才开始读 stdout"（§2 实测）。两个流必须分开读，不能合并。
//! - **自签名 TLS**：webapi 用自签证书，本进程必须显式接受（见 [`insecure_agent`]）。
//!
//! ## 尚未做（后续增量）
//!
//! - 卡片**按钮/交互**（P3；`card.action.trigger` 回调已实测可走长连接，但需要
//!   `custom_action_id` 路由，且那条路的更新有"最多 2 次"限制，见 §8.1）
//! - `lark-cli` 的可执行文件发现应交给**依赖中心**（现在是探测 + `LS_LARK_CLI` 覆盖）

mod card;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use card::{
    render_answer_card, render_error, render_working, split_for_cards, SseAccum, Throttle,
    MAX_BODY_BYTES, MAX_CARDS,
};
use lark_gate::{
    gate, parse_allowed_users, strip_leading_mention, GateVerdict, LarkGateConfig, LarkMessageEvent,
    NON_P2P_NOTICE,
};
use serde_json::{json, Value};

/// webapi 基址（自签名 TLS）。可用 `LS_WEB_API_URL` 覆盖。
const DEFAULT_BASE: &str = "https://127.0.0.1:8443";
/// 等长连接就绪的上限。
const READY_TIMEOUT: Duration = Duration::from_secs(90);
/// 纯文本回复的截断长度（飞书消息有长度上限，且 Windows 命令行也有；§8 的卡片分页是后续增量）。
const MAX_REPLY_CHARS: usize = 3000;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // ── 0. 父进程看门狗（由 GUI 拉起时才启用）──────────────────────────────
    // 应用被**硬杀/崩溃**（任务管理器、OOM、panic 后 abort）时 `RunEvent::Exit`
    // **不会**执行 → 没人收我们。若不看住父进程，就会留下**孤儿 bot**继续占着长连接；
    // 更糟的是应用重启后会**同时跑两个 bot**（重复回答）。2026-10-15 实测确认过这个缺口。
    if let Some(pid) = arg_value("--parent-pid").and_then(|v| v.parse::<u32>().ok()) {
        watch_parent(pid);
    }

    // ── 1. 凭据与基址 ──────────────────────────────────────────────────────
    let token = web_api_token();
    if token.is_empty() {
        log::error!("config.json 里没有 web_api_token —— 请先启动一次主程序（它会生成）");
        std::process::exit(2);
    }
    let base = std::env::var("LS_WEB_API_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
    let http = Arc::new(Http::new(base.clone(), token));
    log::info!("[bot] webapi = {base}");

    // ── 2. 策略（§7.1 ⑤；默认关，必须显式开启）─────────────────────────────
    // 由 GUI 拉起时，webapi 的 bind 可能还在异步进行 → 重试一小会儿再判定失败。
    let cfg = match http.load_gate_config_with_retry(Duration::from_secs(20)) {
        Ok(c) => c,
        Err(e) => {
            log::error!("读取设置失败：{e}（主程序/webapi 是否在运行？）");
            std::process::exit(3);
        }
    };
    if !cfg.enabled {
        log::warn!("lark_bot_enabled != \"true\" —— 机器人未启用，退出");
        return;
    }
    if cfg.gate.allowed_users.is_empty() {
        log::warn!("lark_allowed_users 为空 ⇒ 按 fail-closed **拒绝一切消息**（§7.1 ②）");
    }
    log::info!(
        "[bot] 策略：require_p2p={} 白名单 {} 人",
        cfg.gate.require_p2p,
        cfg.gate.allowed_users.len()
    );

    // ── 3. 起 lark-cli 长连接 ─────────────────────────────────────────────
    let mut child = match spawn_consumer() {
        Ok(c) => c,
        Err(e) => {
            log::error!("{e}");
            std::process::exit(4);
        }
    };
    if let Err(e) = wait_ready(&mut child) {
        log::error!("{e}");
        let _ = child.kill();
        std::process::exit(5);
    }
    log::info!("[bot] 长连接就绪，开始消费事件");

    // ── 4. 逐行消费标准输出 ────────────────────────────────────────────────
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            log::error!("拿不到 lark-cli 的 stdout");
            std::process::exit(6);
        }
    };
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                log::warn!("读 stdout 失败：{e}");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Err(e) = handle_line(line, &http, &cfg.gate) {
            // 单条失败不影响后续事件：飞书会重投，而我们有 message_id 去重兜底。
            log::warn!("[bot] 处理事件失败：{e}");
        }
    }
    log::warn!("[bot] stdout 结束（lark-cli 已退出）");
    let _ = child.wait();
}

// ───────────────────────────── 事件处理 ─────────────────────────────

fn handle_line(line: &str, http: &Http, gate_cfg: &LarkGateConfig) -> Result<(), String> {
    let ev: LarkMessageEvent = serde_json::from_str(line)
        .map_err(|e| format!("事件 JSON 解析失败：{e}（原文前 200 字符：{}）", &line[..line.len().min(200)]))?;
    if ev.event_type != "im.message.receive_v1" {
        return Ok(()); // 只关心新消息
    }

    match gate(&ev, gate_cfg) {
        GateVerdict::DenyNonUser => {
            log::info!("[bot] 忽略非用户来源（sender_type={}）", ev.sender_type);
            return Ok(());
        }
        GateVerdict::DenyNotAllowed => {
            // 静默：不向未知用户确认机器人存在（§7.1 ③）
            log::info!(
                "[bot] 拒绝：不在白名单（sender={} chat_type={}）",
                ev.sender_id,
                ev.chat_type
            );
            return Ok(());
        }
        GateVerdict::DenyNonP2p => {
            // 回提示：群里多半是本人误用，需要反馈（§7.1 ③）
            log::info!("[bot] 拒绝：非私聊（chat_type={}）", ev.chat_type);
            reply(&ev.message_id, NON_P2P_NOTICE);
            return Ok(());
        }
        GateVerdict::Pass => {}
    }

    // 去重（§8：键是 message_id，不是 event_id）
    let seen = http.post("/api/lark/seen", &json!({ "message_id": ev.message_id }))?;
    if !seen.get("first_time").and_then(|v| v.as_bool()).unwrap_or(false) {
        log::info!("[bot] 重投，忽略 message_id={}", ev.message_id);
        return Ok(());
    }

    let question = strip_leading_mention(&ev.content);
    if question.is_empty() {
        reply(&ev.message_id, "请直接说出你的问题。");
        return Ok(());
    }

    // 取/建会话绑定（阶段三）
    let session_id = http.session_for_chat(&ev.chat_id, &ev.sender_id)?;
    let session = http.get(&format!("/api/chat/sessions/{session_id}?origin=lark"))?;
    // 历史**原样透传**（见文件头注释：结构化解析一旦与服务端不一致就会静默丢空历史）
    let mut messages: Vec<Value> = session
        .get("messages")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    messages.push(json!({ "role": "user", "content": question }));
    if messages.len() > 1 {
        log::info!("[bot] 带上 {} 条历史（含本轮问题）", messages.len());
    }

    // 优先走**卡片**（§8：RAG 要几十秒，"一片安静"是最差的体验）。
    // 但**卡片还没发出去之前**的任何失败 → 完全退回纯文本，用户不会看到两套答案。
    // `LS_LARK_CARDS=off` 是逃生阀（排查卡片问题时强制走纯文本）。
    if cards_enabled() {
        match answer_with_card(http, &ev, &question, &session_id, session.clone(), messages.clone()) {
            Ok(()) => return Ok(()),
            Err(e) => log::warn!("[bot] 卡片流程未能启动（{e}）→ 退回纯文本"),
        }
    }
    answer_with_text(http, &ev, &question, &session_id, session, messages)
}

/// 截断过长回答，并说明原因。
///
/// **只用于纯文本兜底路径**（卡片路径有自己的 `card::clip`，且飞书卡片本身能承载更多内容）。
fn truncate_reply(answer: &str) -> String {
    if answer.chars().count() <= MAX_REPLY_CHARS {
        return answer.to_string();
    }
    let head: String = answer.chars().take(MAX_REPLY_CHARS).collect();
    format!("{head}\n\n…（回答过长已截断；完整内容请到 WebUI / 桌面端查看）")
}

// ───────────────────────────── 会话问答：两条路径 ─────────────────────────────

/// 两个会话问答路由**共用**的请求体（省得两处写法漂移）。
///
/// - 非流式 `/api/ai/conversation/ask`：**忽略** `sessionId`，历史只能由我们带上；
/// - 流式 `/api/ai/conversation/ask/stream`：**接收** `sessionId` → 证据按 session 按轮绑定
///   （这正是"追问时「他」指代不明确"的根因，见 §8.1）。
///
/// `scope` 直接发 `{}`：服务端字段全是 `#[serde(default)]`，语义等同全默认。
fn ask_body(session_id: &str, messages: &[Value]) -> Value {
    json!({
        "messages": messages,
        "sourceIds": [],
        "scope": {},
        "sessionRetrievalScope": [],
        "strictDocs": false,
        "fullRecall": false,
        "sessionId": session_id,
    })
}

/// 把这一轮写回会话（`origin=lark`；`origin` 已在会话里，回写时不必带）。
///
/// **失败只告警**：落库失败不该让用户拿不到答案。
fn save_turn(http: &Http, session_id: &str, session: &mut Value, messages: &[Value]) {
    if let Some(obj) = session.as_object_mut() {
        obj.insert("messages".into(), serde_json::to_value(messages).unwrap_or_default());
    }
    if let Err(e) = http.put(
        &format!("/api/chat/sessions/{session_id}?origin=lark"),
        &json!({ "session": session }),
    ) {
        log::warn!("[bot] 会话落库失败（回答照常发出）：{e}");
    }
}

/// 纯文本回答：**卡片功能之前的老路径**，现在作为兜底保留（能力不能因卡片而变差）。
fn answer_with_text(
    http: &Http,
    ev: &LarkMessageEvent,
    question: &str,
    session_id: &str,
    mut session: Value,
    messages: Vec<Value>,
) -> Result<(), String> {
    let asked = http.post("/api/ai/conversation/ask", &ask_body(session_id, &messages))?;
    let answer = asked
        .get("answer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if answer.is_empty() {
        reply(&ev.message_id, "没有拿到回答（主程序日志里应有原因）。");
        return Ok(());
    }
    let mut done = messages;
    done.push(json!({ "role": "assistant", "content": answer }));
    save_turn(http, session_id, &mut session, &done);

    reply(&ev.message_id, &truncate_reply(&answer));
    log::info!(
        "[bot] 已回答（文本）chat={} session={session_id} 问题 {} 字 / 回答 {} 字",
        ev.chat_id,
        question.chars().count(),
        answer.chars().count()
    );
    Ok(())
}

/// 卡片逃生阀：`LS_LARK_CARDS=off` → 强制走纯文本。
fn cards_enabled() -> bool {
    !std::env::var("LS_LARK_CARDS")
        .map(|v| v.eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

/// 卡片回答：回复一张"检索中"卡片 → 消费 SSE → **节流**更新 → 收尾成最终卡片。
///
/// **错误契约**：只在"卡片**还没发出去**"时返回 `Err`（调用方据此退回纯文本）；
/// 卡片一旦发出，之后的错误都在内部消化成一张"出错了"卡片 —— 否则用户会**同时**看到
/// 卡片和文本两份答案。
fn answer_with_card(
    http: &Http,
    ev: &LarkMessageEvent,
    question: &str,
    session_id: &str,
    mut session: Value,
    messages: Vec<Value>,
) -> Result<(), String> {
    let card_id = reply_card(&ev.message_id, &render_working("正在检索…", ""))?;
    log::info!("[bot] 已回卡片 {card_id}（chat={}）", ev.chat_id);

    match stream_turn(http, session_id, &messages, &card_id) {
        Ok(turn) => {
            let mut done = messages;
            done.push(json!({ "role": "assistant", "content": turn.full_text }));
            save_turn(http, session_id, &mut session, &done);

            // 超长回答**分多张卡片**（"过长也要能完成"）：
            // 第 1 段更新那张流式卡片（用户正在看它），第 2..N 段作为**新的回复卡片续发**。
            // 续发只发最终内容、不参与流式更新 → **不额外增加 QPS**。
            let chunks = split_for_cards(&turn.full_text, MAX_BODY_BYTES, MAX_CARDS);
            let pages = chunks.len();
            let mut sent_pages = 0usize;
            let first = render_answer_card(&chunks[0], 1, pages, turn.evidence_count, turn.took_ms);
            if let Err(e) = patch_card(&card_id, &first) {
                // 收尾更新失败：至少把答案给出去（此时卡片里还没有最终内容，不会重复）
                log::warn!("[bot] 收尾卡片更新失败：{e}");
                reply(&ev.message_id, &truncate_reply(&turn.full_text));
            } else {
                sent_pages = 1;
                for (i, chunk) in chunks.iter().enumerate().skip(1) {
                    let card = render_answer_card(chunk, i + 1, pages, turn.evidence_count, turn.took_ms);
                    match reply_card(&ev.message_id, &card) {
                        Ok(_) => {
                            sent_pages += 1;
                            log::info!("[bot] 续发第 {}/{} 段", i + 1, pages);
                        }
                        // 单段失败不该让其余段落不发出
                        Err(e) => log::warn!("[bot] 第 {}/{} 段续发失败：{e}", i + 1, pages),
                    }
                }
            }
            log::info!(
                "[bot] 已回答（卡片）chat={} session={session_id} 问题 {} 字 / 回答 {} 字 / {sent_pages}/{pages} 张卡片 / 中间更新 {} 次{}",
                ev.chat_id,
                question.chars().count(),
                turn.full_text.chars().count(),
                turn.updates,
                if turn.cancelled { "（已取消）" } else { "" }
            );
            Ok(())
        }
        Err(e) => {
            log::warn!("[bot] 流式回答失败：{e}");
            let _ = patch_card(&card_id, &render_error(&e));
            // 卡已发出 → **不**退回纯文本（否则答案会出现两份）
            Ok(())
        }
    }
}

/// 一轮流式回答的产出。
struct Turn {
    full_text: String,
    evidence_count: usize,
    took_ms: u64,
    cancelled: bool,
    updates: usize,
}

/// 消费 SSE 并**节流**更新卡片；返回这一轮的最终内容。
///
/// 事件契约（`webapi/routes/ai.rs`）：具名事件 `ai-progress` / `ai-chunk` / `ai-done`，
/// 各自 data 是 JSON。**只有 `ai-done` 的 `full_text` 是权威结果** —— 中间 chunk 只用于显示，
/// 广播通道在接收端落后时会丢事件（axum `BroadcastStream` 的 `Lagged`），丢了也不影响最终答案。
fn stream_turn(http: &Http, session_id: &str, messages: &[Value], card_id: &str) -> Result<Turn, String> {
    let mut throttle = Throttle::standard();
    let mut acc = SseAccum::default();
    let mut partial = String::new();
    let mut progress = String::from("正在检索…");
    let mut done_payload: Option<Value> = None;

    http.stream("/api/ai/conversation/ask/stream", &ask_body(session_id, messages), |line| {
        let Some((event, data)) = acc.push_line(line) else {
            return Ok(true);
        };
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            log::warn!("[bot] SSE 帧不是 JSON（忽略）：{}", &data[..data.len().min(120)]);
            return Ok(true);
        };
        match event.as_str() {
            "ai-progress" => {
                progress = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("处理中…")
                    .to_string();
            }
            "ai-chunk" => match v.get("delta").and_then(|d| d.as_str()) {
                Some(d) => partial.push_str(d),
                None => return Ok(true),
            },
            "ai-done" => {
                done_payload = Some(v);
                return Ok(false); // 收尾由调用方发**最终卡片**，这里不再 patch
            }
            _ => return Ok(true),
        }
        // 中间更新：节流决定"这次要不要真发"（把高频 chunk 合并掉）
        if throttle.should_send(Instant::now(), false)
            && let Err(e) = patch_card(card_id, &render_working(&progress, &partial))
        {
            // 中间更新失败**不**中断回答：抖动不值得让用户拿不到答案
            log::warn!("[bot] 中间卡片更新失败（继续）：{e}");
        }
        Ok(true)
    })?;

    let Some(done) = done_payload else {
        // 流结束却没有 `ai-done`：有内容就用内容收尾，否则算失败
        if partial.trim().is_empty() {
            return Err("SSE 流结束但没收到 ai-done（也没有任何内容）".to_string());
        }
        log::warn!("[bot] 未收到 ai-done，用已接收的部分内容收尾");
        return Ok(Turn {
            full_text: partial,
            evidence_count: 0,
            took_ms: 0,
            cancelled: false,
            updates: throttle.sent(),
        });
    };

    let cancelled = done.get("cancelled").and_then(|c| c.as_bool()).unwrap_or(false);
    let authoritative = done
        .get("full_text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let full_text = if authoritative.is_empty() { partial.trim().to_string() } else { authoritative };
    if full_text.is_empty() {
        return Err(if cancelled {
            "本次回答已取消".to_string()
        } else {
            "回答为空（主程序日志里应有原因）".to_string()
        });
    }
    let evidence_count = done
        .get("evidence")
        .and_then(|e| e.as_array())
        .map(|a| a.len())
        .or_else(|| done.get("source_ids").and_then(|s| s.as_array()).map(|a| a.len()))
        .unwrap_or(0);
    let took_ms = done.get("took_ms").and_then(|t| t.as_u64()).unwrap_or(0);
    Ok(Turn { full_text, evidence_count, took_ms, cancelled, updates: throttle.sent() })
}

// ───────────────────────────── 卡片收发（走 lark-cli raw API）─────────────────────────────

/// 调 `lark-cli api`（raw 逃生口）。
///
/// 两个刻意的选择：
/// - **参数数组传值、不经 shell** → 卡片正文里的引号/换行安全（AGENTS.md 记的那个引号坑
///   只存在于壳层调用）；
/// - **`--data -` 走 stdin** → 不受 argv 长度限制（卡片可能有上万字符）。
fn lark_api(method: &str, path: &str, body: &Value) -> Result<Value, String> {
    let program = lark_cli_program();
    let payload = serde_json::to_string(body).map_err(|e| format!("序列化请求体失败：{e}"))?;
    let mut child = Command::new(&program)
        .args(["api", method, path, "--as", "bot", "--data", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 lark-cli（{}）：{e}", program.display()))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(payload.as_bytes())
            .map_err(|e| format!("写 lark-cli stdin 失败：{e}"))?;
    } // drop → EOF，lark-cli 才会开始处理

    let out = child.wait_with_output().map_err(|e| format!("等待 lark-cli 失败：{e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!(
            "lark-cli api 退出码 {:?}：{} {}",
            out.status.code(),
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: Value = serde_json::from_str(&stdout).map_err(|e| {
        format!("lark-cli 输出不是 JSON：{e}（前 200 字符：{}）", &stdout[..stdout.len().min(200)])
    })?;
    // ⚠️ lark-cli 的封装是 `{ ok, identity, data }`，**不是**飞书的 `{ code, msg }`。
    //（§8.1 实测踩过：按 `code == 0` 判成功会把**成功读成失败**）
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return Err(format!("lark-cli api 返回失败：{}", stdout.trim()));
    }
    Ok(v)
}

/// **回复**一张卡片（`+messages-reply` 不支持 interactive，必须走 raw）。返回新卡片的 `message_id`。
fn reply_card(message_id: &str, card: &Value) -> Result<String, String> {
    if message_id.is_empty() {
        return Err("没有 message_id，无法回复".to_string());
    }
    let body = json!({
        "msg_type": "interactive",
        "content": serde_json::to_string(card).map_err(|e| format!("序列化卡片失败：{e}"))?,
    });
    let out = lark_api("POST", &format!("/open-apis/im/v1/messages/{message_id}/reply"), &body)?;
    out.get("data")
        .and_then(|d| d.get("message_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("回复卡片未返回 message_id：{}", out))
}

/// 更新已发出的卡片（**app 身份** `PATCH`）。
///
/// §8.1 实测：这条路**没有"最多 2 次"限制**（80/80 成功），约束是**频率**而非次数 ——
/// "最多 2 次"属 `card.action.trigger` 回调 token 那条路。因此这里只做节流，不做硬性次数限制。
fn patch_card(card_message_id: &str, card: &Value) -> Result<(), String> {
    let body = json!({
        "content": serde_json::to_string(card).map_err(|e| format!("序列化卡片失败：{e}"))?,
    });
    lark_api("PATCH", &format!("/open-apis/im/v1/messages/{card_message_id}"), &body).map(|_| ())
}

// ───────────────────────────── 凭据读取 ─────────────────────────────

/// 读 webapi token：`LS_WEB_API_TOKEN` 优先，否则读 `config.json`。
///
/// **为什么复刻这段读取逻辑**：它是 `link_searcher_lib::config::web_api_token` 的等价物，
/// 但本 crate 刻意不依赖应用 lib（见文件头）。路径规则必须与 `config.rs::config_dir`
/// 一致：`LS_CONFIG_DIR` 后门 → 否则 `dirs::config_dir()/.link-searcher`。
///
/// 这一段"复刻"是有意的取舍：它只有十来行、失败时**有明确报错**（退出码 2），
/// 而不复刻的代价是把 30 MB 的整个应用链进来。
fn web_api_token() -> String {
    if let Ok(t) = std::env::var("LS_WEB_API_TOKEN") {
        if !t.is_empty() {
            return t;
        }
    }
    let dir = match std::env::var("LS_CONFIG_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => match dirs::config_dir() {
            Some(d) => d.join(".link-searcher"),
            None => return String::new(),
        },
    };
    std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("web_api_token")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

// ───────────────────────────── 父进程看门狗 ─────────────────────────────

/// 取命令行 `--key value` 形式的值（本进程参数极少，不引 clap）。
fn arg_value(key: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == key {
            return args.next();
        }
    }
    None
}

/// 父进程消失即退出（由 `--parent-pid` 启用）。
///
/// 不用"父进程句柄"的原因：Windows 上要 `OpenProcess` + `WaitForSingleObject`，
/// 而 `sysinfo` 已是本包依赖（自适应闸门在用）且**跨平台**，只刷新单个 pid 的成本可忽略。
/// 退出走 `exit(0)`：进程一死，lark-cli 的 stdin 管道即 EOF → 它**优雅退出**
/// （bus daemon 按决策留着复用，不受影响；2026-10-15 实测过这条路径）。
fn watch_parent(parent_pid: u32) {
    std::thread::spawn(move || {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let pid = Pid::from_u32(parent_pid);
        let mut sys = System::new();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            if sys.process(pid).is_none() {
                log::warn!("[bot] 父进程 {parent_pid} 已退出 → 随之退出（避免孤儿 bot 重复回答）");
                std::process::exit(0);
            }
        }
    });
}

// ───────────────────────────── lark-cli 子进程 ─────────────────────────────

/// 探测 `lark-cli` 可执行文件。
///
/// Windows 上 `lark-cli` 只是 Node 的 `.cmd` 包装，而 `CreateProcess` **不能**直接执行 `.cmd`
/// （会被要求经 cmd.exe，届时参数还要再被重解析一次 —— 而回复正文任意，那个坑很深）。
/// 所以优先用 npm 包里的**原生二进制**。
///
/// TODO(依赖中心)：这段发现逻辑应迁到"依赖中心"（见 §十 的 `lark-cli` 决策）。
fn lark_cli_program() -> PathBuf {
    if let Ok(p) = std::env::var("LS_LARK_CLI") {
        return PathBuf::from(p);
    }
    if cfg!(windows)
        && let Ok(local) = std::env::var("LOCALAPPDATA")
    {
        let native = PathBuf::from(local)
            .join("Programs")
            .join("nodejs")
            .join("node_modules")
            .join("@larksuite")
            .join("cli")
            .join("bin")
            .join("lark-cli.exe");
        if native.is_file() {
            return native;
        }
    }
    PathBuf::from(if cfg!(windows) { "lark-cli.cmd" } else { "lark-cli" })
}

fn spawn_consumer() -> Result<Child, String> {
    let program = lark_cli_program();
    log::info!("[bot] 启动事件消费：{}", program.display());
    Command::new(&program)
        .args(["event", "consume", "im.message.receive_v1", "--as", "bot"])
        .stdin(Stdio::piped()) // 保留 stdin：优雅停机靠"关 stdin"（Windows 没有可用的 SIGTERM）
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 lark-cli（{}）：{e}", program.display()))
}

/// 阻塞读 stderr，直到出现就绪标记 `[event] ready event_key=`。
///
/// 这是 lark-cli 给的**子进程契约**（§2 实测）：在它出现之前不要相信 stdout。
fn wait_ready(child: &mut Child) -> Result<(), String> {
    let stderr = child.stderr.take().ok_or("拿不到 lark-cli 的 stderr")?;
    let ready = Arc::new(AtomicBool::new(false));
    let ready_in_thread = Arc::clone(&ready);

    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            // 诊断日志原样转发（`--quiet` 会隐藏事件丢失诊断，故不启用）
            log::info!("[lark-cli] {line}");
            if line.contains("[event] ready event_key=") {
                ready_in_thread.store(true, Ordering::Release);
            }
        }
        log::warn!("[lark-cli] stderr 关闭");
    });

    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if ready.load(Ordering::Acquire) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("等待 {:?} 仍未收到就绪标记（长连接没建起来？）", READY_TIMEOUT))
}

/// 以 `+messages-reply` 回复一条消息。失败只记日志 —— 不让回复失败拖垮消费循环。
fn reply(message_id: &str, text: &str) {
    if message_id.is_empty() {
        return;
    }
    let program = lark_cli_program();
    // 用参数数组逐个传值：**不经 shell**，所以正文里的引号/空格/换行都安全
    //（PowerShell 5.1 那个"吃引号"的坑只存在于壳层调用，见 AGENTS.md 门禁提示）。
    match Command::new(&program)
        .args(["im", "+messages-reply", "--message-id", message_id, "--text", text])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => log::warn!(
            "[bot] 回复失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => log::warn!("[bot] 回复失败：{e}"),
    }
}

// ───────────────────────────── HTTP 客户端 ─────────────────────────────

struct Http {
    agent: ureq::Agent,
    base: String,
    token: String,
}

/// bot 自己的策略快照（来自 `app_settings`）。
struct BotConfig {
    enabled: bool,
    gate: LarkGateConfig,
}

impl Http {
    fn new(base: String, token: String) -> Self {
        Self { agent: insecure_agent(), base, token }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}{}", self.base, path);
        let req = self
            .agent
            .request(method, &url)
            .set("Authorization", &format!("Bearer {}", self.token));
        let result = match body {
            Some(b) => {
                let payload = serde_json::to_string(b).map_err(|e| e.to_string())?;
                req.set("Content-Type", "application/json").send_string(&payload)
            }
            None => req.call(),
        };
        match result {
            Ok(resp) => {
                let text = resp.into_string().map_err(|e| e.to_string())?;
                serde_json::from_str(&text)
                    .map_err(|e| format!("webapi 返回的不是 JSON：{e}（前 200 字符：{}）", &text[..text.len().min(200)]))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let detail = resp.into_string().unwrap_or_default();
                Err(format!("HTTP {code}：{detail}"))
            }
            Err(e) => Err(format!("{e}")),
        }
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, String> {
        self.call("GET", path, None)
    }
    fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        self.call("POST", path, Some(body))
    }
    fn put(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        self.call("PUT", path, Some(body))
    }

    /// 发一个 **SSE** 请求并逐行回调；回调返回 `Ok(false)` 表示下游已拿到要的内容、可以停止读取。
    ///
    /// 与 [`Self::call`] 的关键差别：
    /// - 用 `into_reader()` **流式**读，绝不把响应体整个攒起来（否则"进度"就不流了）；
    /// - 显式 `Accept-Encoding: identity` —— 压缩会缓冲，进度就看不到（bot crate 也已去掉 `gzip` 特性）；
    /// - 读超时沿用 agent 的 600s：SSE 有 15s keep-alive，不会因"看起来空闲"而误断。
    fn stream(
        &self,
        path: &str,
        body: &serde_json::Value,
        mut on_line: impl FnMut(&str) -> Result<bool, String>,
    ) -> Result<(), String> {
        let url = format!("{}{}", self.base, path);
        let payload = serde_json::to_string(body).map_err(|e| e.to_string())?;
        let resp = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream")
            .set("Accept-Encoding", "identity")
            .send_string(&payload)
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    format!("HTTP {code}：{}", resp.into_string().unwrap_or_default())
                }
                other => format!("{other}"),
            })?;
        let reader = BufReader::new(resp.into_reader());
        for line in reader.lines() {
            let line = line.map_err(|e| format!("读 SSE 流失败：{e}"))?;
            if !on_line(&line)? {
                break;
            }
        }
        Ok(())
    }

    /// 读设置，并在 webapi 尚未监听时重试（GUI 拉起 bot 时 bind 可能还在异步进行）。
    fn load_gate_config_with_retry(&self, budget: Duration) -> Result<BotConfig, String> {
        let deadline = Instant::now() + budget;
        loop {
            match self.load_gate_config() {
                Ok(c) => return Ok(c),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    log::warn!("[bot] 读取设置失败，1 秒后重试：{e}");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    /// 读 `app_settings` 里的飞书策略（§7.1 ⑤）。
    fn load_gate_config(&self) -> Result<BotConfig, String> {
        let settings = self.get("/api/settings")?;
        let s = |key: &str| {
            settings
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let raw_users = s("lark_allowed_users");
        let users = parse_allowed_users(&raw_users);
        if !raw_users.trim().is_empty() && users.is_empty() {
            // 解析失败 ⇒ 空名单（＝拒绝全部）。这里额外喊一声，否则会表现为"机器人不理我"。
            log::error!("lark_allowed_users 无法解析（{raw_users:?}）⇒ 按拒绝全部处理");
        }
        Ok(BotConfig {
            enabled: s("lark_bot_enabled").eq_ignore_ascii_case("true"),
            gate: LarkGateConfig {
                // 缺省即 true：没配置时也必须要求私聊
                require_p2p: !s("lark_require_p2p").eq_ignore_ascii_case("false"),
                allowed_users: users,
            },
        })
    }

    /// 取该私聊绑定的会话；没有则新建并绑定（阶段三）。
    fn session_for_chat(&self, chat_id: &str, user_open_id: &str) -> Result<String, String> {
        let existing = self.get(&format!("/api/lark/binding?chat_id={chat_id}"))?;
        if let Some(sid) = existing
            .get("binding")
            .and_then(|b| b.get("session_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(sid.to_string());
        }
        let created = self.post("/api/chat/sessions?origin=lark", &json!({}))?;
        let sid = created
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "创建会话未返回 id".to_string())?
            .to_string();
        self.put(
            "/api/lark/binding",
            &json!({
                "chat_id": chat_id,
                "session_id": sid,
                "user_open_id": user_open_id,
            }),
        )?;
        log::info!("[bot] 为 {chat_id} 新建会话 {sid} 并绑定");
        Ok(sid)
    }
}

/// 接受自签名证书的 HTTP agent。
///
/// webapi 用自签 TLS（`webapi/tls.rs`），同机调用必须跳过校验；`rustls` 的
/// `dangerous()` 是官方给的入口。注意这只影响 **bot → 本机 webapi** 这一段，
/// 且地址固定为回环。
fn insecure_agent() -> ureq::Agent {
    // 与 webapi 同款：先装 CryptoProvider，再建配置。
    let _ = rustls::crypto::ring::default_provider().install_default();

    #[derive(Debug)]
    struct AcceptAnyCert;
    impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            use rustls::SignatureScheme::*;
            vec![
                RSA_PKCS1_SHA256,
                RSA_PKCS1_SHA384,
                RSA_PKCS1_SHA512,
                ECDSA_NISTP256_SHA256,
                ECDSA_NISTP384_SHA384,
                ED25519,
            ]
        }
    }

    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();

    ureq::AgentBuilder::new()
        .tls_config(Arc::new(tls))
        .timeout_connect(Duration::from_secs(5))
        // RAG 可能要几十秒；给足读超时
        .timeout_read(Duration::from_secs(600))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_reply_keeps_short_answer_intact() {
        assert_eq!(truncate_reply("短回答"), "短回答");
        let exact: String = "字".repeat(MAX_REPLY_CHARS);
        assert_eq!(truncate_reply(&exact), exact, "刚好到上限不应截断");
    }

    #[test]
    fn truncate_reply_marks_truncation_and_cuts_by_char() {
        let long: String = "字".repeat(MAX_REPLY_CHARS + 1);
        let out = truncate_reply(&long);
        assert_eq!(
            out.chars().filter(|c| *c == '字').count(),
            MAX_REPLY_CHARS,
            "必须按**字符**（不是字节）截断，否则中文会被切出半个字符"
        );
        assert!(out.contains("已截断"), "截断必须说明原因，否则像「回答就这么多」");
    }

    #[test]
    fn arg_value_reads_pairs_only() {
        // 只测"解析规则"本身：`--key value` 成对取值，孤立 key 取不到。
        //（不能改进程 args，故这里只覆盖无参场景。）
        assert_eq!(arg_value("--parent-pid"), None);
    }
}
