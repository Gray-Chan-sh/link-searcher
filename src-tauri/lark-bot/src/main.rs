//! 飞书 bot sidecar —— `docs/LARK_BOT.md` P1 的 bot 本体。
//!
//! ## 它做的事（§11 起为多会话 + 搜索）
//!
//! ```text
//! 消费 lark-cli 的 NDJSON 事件（三条流汇进同一个处理循环）：
//!   ├─ im.message.receive_v1  消息  → §7.1 准入判定（`lark-gate`）→ /api/lark/seen 去重
//!   │     ├─ 命中斜杠命令（commands::parse_command）→ 命令实现（/help /new /sessions
//!   │     │   /switch /rename /delete /find）→ 回卡片/文本，结束
//!   │     └─ 否则 → 取/建**活动会话**（chat_id ↔ 多会话表的 is_active 行）
//!   │           → 回"检索中"卡片 → 消费 `POST /api/ai/conversation/ask/stream`（SSE）
//!   │           → **节流**更新卡片（§8.1）→ 收尾成最终卡片 → 这一轮落回会话（origin=lark）
//!   ├─ card.action.trigger    卡片按钮（打开详情 / 换一批证据 / 切换 / 删除 / 新建 / 取消）
//!   └─ application.bot.menu_v6 机器人菜单点击（§11；菜单在开发者后台配置，仅单聊）
//! ```
//!
//! 三条入口各自过白名单：消息走完整闸门（L0/L2/L1/L1′），回调与菜单事件没有
//! `chat_type`，只做身份判定（fail-closed）。
//!
//! 卡片的渲染/节流/SSE 分帧在 [`card`] 模块；命令解析在 [`commands`]；
//! "回复数字点选"的状态在 [`menu`]（都是纯逻辑，可单测）。
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
//! - **自签名 TLS**：webapi 用自签证书，**回环**地址（默认）由 [`insecure_agent`] 显式接受；
//!   非回环地址一律走真实证书校验（见 [`agent_for`]），确需自签的异地部署要显式设
//!   `LS_WEBAPI_INSECURE=1`。
//! - **`lark-cli` 的可执行文件发现归应用侧的依赖中心**：应用解析出的路径经 `LS_LARK_CLI`
//!   注入给本进程；本进程自带的 [`lark_cli_program`] 只是"被手工拉起"时的兜底
//!   （Unix 上直接交回裸名 `lark-cli`，由 PATH 解析 —— 见该函数注释）。
//!
//! ## 尚未做（后续增量）
//!
//! - **入站**文件 / 语音 / 图片（P2；见 `docs/LARK_BOT.md` 九、分阶段实施建议）。
//!   **出站**（把本地库里的文件发到飞书）已实现：`/find` 结果卡上的「发送 N」按钮
//!   → `media` 上传 → 回复图片/语音/文件消息（见 §5.13）。
//! - 定时摘要与"写飞书文档/表格"出口（P3）
//! - 卡片按钮的**回调 token 更新**（当前按钮只做"回复式"动作，刻意绕开
//!   `card.action.trigger` token 的"最多 2 次更新"限制，见 §8.1）

mod card;
mod commands;
mod media;
mod menu;
mod notify;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use std::collections::HashMap;

use card::{
    render_answer_card, render_delete_list_card, render_error, render_history_card,
    render_search_card, render_session_list_card, render_working, split_for_cards, CardActionEvent,
    ListItem, SseAccum, Throttle, MAX_BODY_BYTES, MAX_CARDS, MAX_MENU_LINES,
};
use commands::{parse_command, parse_selection, Command as Cmd};
use lark_gate::{
    gate, strip_leading_mention, GateVerdict, LarkGateConfig, LarkMessageEvent, NON_P2P_NOTICE,
};
use menu::{MenuAction, MenuMemory};
use serde_json::{json, Value};

/// webapi 基址（自签名 TLS）。可用 `LS_WEB_API_URL` 覆盖。
const DEFAULT_BASE: &str = "https://127.0.0.1:8443";
/// 飞书**机器人菜单点击**事件（§11：开发者后台配置的自定义菜单）。
const MENU_EVENT_TYPE: &str = "application.bot.menu_v6";
/// 等长连接就绪的上限。
const READY_TIMEOUT: Duration = Duration::from_secs(90);
/// 菜单消费者的就绪等待上限：菜单是**可选**能力（后台没配就没有就绪标记），
/// 所以短等即走、绝不占用后台线程（详见 [`poll_consumer_into`]）。
const MENU_READY_TIMEOUT: Duration = Duration::from_secs(15);
/// 纯文本回复的截断长度（飞书消息有长度上限，且 Windows 命令行也有；§8 的卡片分页是后续增量）。
const MAX_REPLY_CHARS: usize = 3000;
/// `/find` 每页条数（飞书侧 5 条既有信息量又不刷屏）。
const FIND_PAGE_SIZE: usize = 5;

/// 上次列表菜单记忆（§11：零配置的"回复数字点选"）。
///
/// 进程内存即可：它是**一次性 UI 状态**（bot 重启后翻篇的列表不再有意义），
/// 真正的会话状态在 `lark_chat_sessions` 表里。用 `Mutex` 而非线程局部：
/// 卡片回调消费者（后台线程）也要读它。
static MENUS: Mutex<Option<MenuMemory>> = Mutex::new(None);

/// 在菜单记忆上执行一次操作（拿到全局单例，惰性初始化）。
fn with_menu<R>(f: impl FnOnce(&mut MenuMemory) -> R) -> R {
    let mut guard = MENUS.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(MenuMemory::new))
}

/// 每个私聊**当前正在生成**的会话（`chat_id → session_id`）。
///
/// 两个用途（§11.8）：
/// 1. **每私聊串行**：消息改并发消费后（`/stop` 必须能在生成途中被处理），
///    同一私聊的第二条提问会被这里拦下，避免两会话并发写同一份会话历史；
/// 2. **`/stop` 的准星**：停止时精确知道该取消**哪条会话**的生成
///    （用户可能在中途切换过会话，不能拿"当前绑定"去猜）。
static GENERATING: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// 在生成登记表上执行一次操作（拿到全局单例，惰性初始化）。
fn with_generating<R>(f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
    let mut guard = GENERATING.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// RAII 生成登记：进入生成即登记，结束（含失败/提前返回）自动摘除。
struct GeneratingGuard {
    chat_id: String,
}

impl GeneratingGuard {
    /// 登记该私聊开始生成。**同私聊已有生成 → `None`**（调用方据此拒绝并提示 `/stop`）。
    fn begin(chat_id: &str, session_id: &str) -> Option<Self> {
        with_generating(|g| {
            if g.contains_key(chat_id) {
                return None;
            }
            g.insert(chat_id.to_string(), session_id.to_string());
            Some(Self { chat_id: chat_id.to_string() })
        })
    }
}

impl Drop for GeneratingGuard {
    fn drop(&mut self) {
        with_generating(|g| {
            g.remove(&self.chat_id);
        });
    }
}

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

    // ── 1b. 应用侧控制通道（§十二）─────────────────────────────────────────
    // 应用经**我们的 stdin** 发一行指令：`shutdown` = 发「正在下线」再退（应用正在退出），
    // `stop` = 静默退（改设置 / 配置热重载后重拉）。
    // 放在这里（而不是等长连接就绪）是刻意的：应用只等几秒就强杀，中间那 20s 的设置重试
    // 窗口里也得能响应停机。发通知要读一次绑定列表 → 现在就把取数口给它。
    notify::spawn_control({
        let http = Arc::clone(&http);
        move || http.bound_chats()
    });

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
        log::warn!("integrations.lark.enabled = false —— 机器人未启用，退出");
        return;
    }
    if cfg.gate.allowed_users.is_empty() {
        log::warn!("integrations.lark.allowed_users 为空 ⇒ 按 fail-closed **拒绝一切消息**（§7.1 ②）");
    }
    log::info!(
        "[bot] 策略：require_p2p={} 白名单 {} 人",
        cfg.gate.require_p2p,
        cfg.gate.allowed_users.len()
    );

    // ── 3. 起 lark-cli 长连接 ─────────────────────────────────────────────
    let mut child = match spawn_consumer("im.message.receive_v1") {
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

    // ── 3a. 生命周期通知的"上线"半边（§十二）──────────────────────────────
    // 上次是**优雅退出**（我们发过「正在下线」）→ 现在补一条「已重新上线」给同一批私聊；
    // 顺带预热"见过消息的私聊"缓存。后台线程：它要调 lark-cli（秒级），不能挡住消费。
    notify::spawn_startup({
        let http = Arc::clone(&http);
        move || http.bound_chats()
    });

    // ── 3b. 卡片回调：**第二个消费者**（D）──────────────────────────────────
    // 实测（§8.4）：本地多消费者**是允许的** —— "同一 app 只能有一条 bus 连接"指**上游**（daemon→飞书）。
    // 回调是**另一条入口**，必须自己再过一次白名单（消息入口那套不会自动保护它）。
    spawn_card_consumer(Arc::clone(&http), cfg.gate.allowed_users.clone());

    // ── 3c. 机器人菜单（§11）：**第三个消费者**，后台起（失败只降级）─────────────
    // 菜单事件是"推送事件"类菜单项的落点；菜单本身在开发者后台配置（仅单聊）。
    // 起不来（旧版 lark-cli / 未订阅）只损失"点菜单"这一条路，消息与卡片两条主路不受影响。
    spawn_menu_consumer(Arc::clone(&http), cfg.gate.allowed_users.clone());

    // ── 4. 逐行消费事件（stdout 线程 + 菜单消费者，统一进同一个通道）────────────
    // 为什么不是直接 `for line in BufReader::new(stdout).lines()`：菜单消费者是**第二个
    // 事件源**，两条流要汇成一条串行处理（处理逻辑是同步的，天然不需要并发）。
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            log::error!("拿不到 lark-cli 的 stdout");
            std::process::exit(6);
        }
    };
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    let stdout_tx = line_tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if !line.is_empty() && stdout_tx.send(line).is_err() {
                break; // 主线程已退出
            }
        }
    });
    drop(line_tx); // 只留 stdout 那条发送端：它结束时接收端才能自然退出
    for line in line_rx {
        // 每条消息**独立线程**处理：生成（RAG 可能几十秒）不能再堵住后续消息 ——
        // `/stop` 之所以有用，前提就是它能在**生成进行中**被处理（此前是单线程顺序
        // 消费，停止消息只能排到回答跑完，等于没有）。
        // 同一私聊的并发生成由 `GeneratingGuard` 拦下（保持"每私聊串行"）。
        let http = Arc::clone(&http);
        let gate = cfg.gate.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_line(&line, &http, &gate) {
                // 单条失败不影响后续事件：飞书会重投，而我们有 message_id 去重兜底。
                log::warn!("[bot] 处理事件失败：{e}");
            }
        });
    }
    log::warn!("[bot] 事件流结束（lark-cli 已退出）");
    let _ = child.wait();
}

// ───────────────────────────── 事件处理 ─────────────────────────────

fn handle_line(line: &str, http: &Http, gate_cfg: &LarkGateConfig) -> Result<(), String> {
    // 事件类型先看一眼（在**反序列化成具体结构之前**）：
    // 菜单事件（`application.bot.menu_v6`）与消息事件字段完全不同，走各自的分支。
    let peek: Value = serde_json::from_str(line)
        .map_err(|e| format!("事件 JSON 解析失败：{e}（原文前 200 字符：{}）", card::clip_str(&line, 200)))?;
    let ev_type = peek
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if ev_type == MENU_EVENT_TYPE {
        return handle_menu_line(&peek, http, &gate_cfg.allowed_users);
    }
    if ev_type != "im.message.receive_v1" {
        return Ok(()); // 只关心新消息与菜单点击
    }

    let ev: LarkMessageEvent = serde_json::from_value(peek)
        .map_err(|e| format!("消息事件解析失败：{e}"))?;
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
        GateVerdict::Pair(sender) => {
            // 配对模式（§7.1 ⑥）：**只登记、绝不回答**。
            // 服务端消费时会校验"配对仍有效"，所以这条路径不能被滥用成加白名单的口子。
            // 消费成功后服务端会重启 bot（策略是启动时读一次的）—— 本进程可能就此退出，
            // 因此这里的日志失败也属正常，不当作错误处理。
            log::info!("[bot] 配对模式：登记 {sender}（本条不回答）");
            match http.post(
                "/api/integrations/lark/pairing/consume",
                &json!({ "open_id": sender }),
            ) {
                Ok(v) => log::info!(
                    "[bot] 配对已消费，白名单={}",
                    v.get("allowed_users").cloned().unwrap_or(Value::Null)
                ),
                Err(e) => log::warn!("[bot] 配对消费未完成（服务端可能已在重启 bot）：{e}"),
            }
            return Ok(());
        }
        GateVerdict::Pass => {}
    }

    // 有真实会话的私聊 → 记下来当"下线通知"的收件人（§十二）。
    // 应用退出时可能已经连不上 webapi，这份缓存是那时的唯一来源。
    notify::remember_chat(&ev.chat_id);

    // 去重（§8：键是 message_id，不是 event_id）
    let seen = http.post("/api/lark/seen", &json!({ "message_id": ev.message_id }))?;
    if !seen.get("first_time").and_then(|v| v.as_bool()).unwrap_or(false) {
        log::info!("[bot] 重投，忽略 message_id={}", ev.message_id);
        return Ok(());
    }

    let question = strip_leading_mention(&ev.content);

    // ── 命令路由（§11）：命中命令 → 执行并结束；未命中 → 一律走 AI 问答 ──────────
    // 数字点选优先于命令：用户在列表后直接回 `3` 是最顺手的操作。
    if let Some(sel) = parse_selection(&question) {
        let action = with_menu(|m| m.resolve(&ev.chat_id, now_secs(), sel));
        if let Some(action) = action {
            log::info!("[bot] 数字点选 {sel} → {action:?}（chat={}）", ev.chat_id);
            return run_menu_action(http, &ev.chat_id, &ev.message_id, &action);
        }
        // 没命中菜单 → 把"3"当普通提问（稀有，但语义一致）
    }
    match parse_command(&question) {
        Cmd::Help => {
            reply_card(&ev.message_id, &commands::help_card())?;
            log::info!("[bot] 已回命令帮助（chat={}）", ev.chat_id);
            return Ok(());
        }
        Cmd::New => return reply_new_session(http, &ev),
        Cmd::List => return reply_session_list(http, &ev),
        Cmd::Current => return reply_current(http, &ev),
        Cmd::History => return reply_history(http, &ev),
        Cmd::Switch(arg) => return reply_switch(http, &ev, &arg),
        Cmd::Rename(name) => return reply_rename(http, &ev, &name),
        Cmd::Stop => return reply_stop(http, &ev),
        Cmd::Delete(arg) => return reply_delete_confirm(http, &ev, &arg),
        Cmd::Find { query, page } => return reply_find(http, &ev, &query, page),
        // 不是命令 / 是空正文：走 AI 问答
        Cmd::Ai => {}
    }

    if question.is_empty() {
        reply(&ev.message_id, "请直接说出你的问题。");
        return Ok(());
    }

    // 取/建会话绑定（阶段三）
    let session_id = http.session_for_chat(&ev.chat_id, &ev.sender_id)?;
    // 同一私聊已有生成 → 拒绝（消息并发消费后，这里是"每私聊串行"的守门人）。
    let Some(_guard) = GeneratingGuard::begin(&ev.chat_id, &session_id) else {
        reply(&ev.message_id, "正在回答上一条问题，请稍候（发 /stop 可中止）。");
        return Ok(());
    };
    match ask_with_session(http, &ev, &question, &session_id, None) {
        Ok(()) => Ok(()),
        // 兜底：绑定指向的会话可能已被**别的端**（桌面/WebUI）删除 —— 随机换一条新的，
        // 并提醒用户（而不是把 404 原样抛给用户或静默失败）。
        Err(e) if e.contains("404") || e.contains("session not found") => {
            log::warn!(
                "[bot] 会话 {session_id} 已不存在（可能在桌面/WebUI 被删）—— 自动新建并继续"
            );
            let fresh = http.session_for_chat_fresh(&ev.chat_id, &ev.sender_id)?;
            reply(
                &ev.message_id,
                "原会话已不存在（可能在其他端被删），已自动开一条新会话。",
            );
            ask_with_session(http, &ev, &question, &fresh, None)
        }
        Err(e) => Err(e),
    }
}

/// 用已有会话发起一次问答（带上历史与这一轮问题）。
///
/// 从原 `handle_line` 尾部抽出：`/switch` 之后的"首次追问"也想复用同一条路径，
/// 但那条路径的会话**已经明确**，不该再走一遍"取/建绑定"。
fn ask_with_session(
    http: &Http,
    ev: &LarkMessageEvent,
    question: &str,
    session_id: &str,
    retry_prior_answer: Option<&str>,
) -> Result<(), String> {
    let mut session = http.get(&format!("/api/chat/sessions/{session_id}?origin=lark"))?;
    let mut session_id = session_id.to_string();
    // **自动新话题**（§11.9）：仅"新提问"路径判定（重跑是对原问题重搜，绝不切换）。
    // 判定失败/拿不准 → 一律留在当前会话（丢上下文比会话凌乱严重得多）。
    if retry_prior_answer.is_none()
        && let Some(fresh) = maybe_auto_switch(http, ev, question, &session)
    {
        session_id = fresh;
        session = http.get(&format!("/api/chat/sessions/{session_id}?origin=lark"))?;
    }
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
        match answer_with_card(http, ev, question, &session_id, session.clone(), messages.clone(), retry_prior_answer) {
            Ok(()) => return Ok(()),
            Err(e) => log::warn!("[bot] 卡片流程未能启动（{e}）→ 退回纯文本"),
        }
    }
    answer_with_text(http, ev, question, &session_id, session, messages, retry_prior_answer)
}

/// 自动新话题开关的逃生阀：`LS_LARK_AUTO_SWITCH=off` → 关闭自动切换（排查/偏好用）。
fn auto_switch_enabled() -> bool {
    !std::env::var("LS_LARK_AUTO_SWITCH")
        .map(|v| v.eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

/// 自动新话题判定 + 切换（§11.9）：返回 `Some(新 session_id)` ＝ 已另起会话。
///
/// 三个刻意的边界：
/// - **空会话不判定**：没有"当前话题"可对比，切换无意义；
/// - **判定失败/超时/拿不准 → 不切换**（错误只记日志）—— 判定是"锦上添花"，
///   绝不能因为它让正常问答失败或丢上下文；
/// - 切换后给用户**一句提示**（否则"列表里怎么多了一条"无从解释）。
fn maybe_auto_switch(
    http: &Http,
    ev: &LarkMessageEvent,
    question: &str,
    session: &Value,
) -> Option<String> {
    if !auto_switch_enabled() {
        return None;
    }
    let history: Vec<Value> = session
        .get("messages")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    if history.is_empty() {
        return None; // 首问：没有可切换的对象
    }
    let verdict = match http.post(
        "/api/ai/topic-shift",
        &json!({ "messages": history, "question": question }),
    ) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[bot] 新话题判定失败（留在当前会话）：{e}");
            return None;
        }
    };
    if verdict.get("new_topic").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    match http.session_for_chat_fresh(&ev.chat_id, &ev.sender_id) {
        Ok(fresh) => {
            log::info!(
                "[bot] 判定为新话题 → 自动另起会话 {fresh}（chat={}）",
                ev.chat_id
            );
            reply(
                &ev.message_id,
                "检测到新话题，已自动为你开一条新会话（旧会话保留，发 /sessions 可切换回去）。",
            );
            Some(fresh)
        }
        Err(e) => {
            log::warn!("[bot] 自动新会话创建失败（留在当前会话）：{e}");
            None
        }
    }
}

/// 从会话响应的 `title` 字段读会话名（`Option<&str>` 直接喂给卡片渲染）。
///
/// 占位标题（"新会话"）由 [`card::headline`] 统一隐藏 —— 这里只管取值。
fn session_title_of(session: &Value) -> Option<String> {
    session
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
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

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 把 Unix 秒格式化成 `MM-DD HH:MM`（**UTC**）。
///
/// 为什么手搓而不引 chrono：bot 是刻意轻量的 sidecar（见文件头"不 import 应用 lib"），
/// 为一个"列表里的相对时间"引入完整时区库不划算；而本地时区在 Windows 上要
/// `GetTimeZoneInformation` 才准（std 拿不到）—— 这个字段只是**参考信息**
/// （"哪个会话最近在用"），按 UTC 显示即可，猜时区反而会给出误导性的"准确时间"。
fn fmt_utc_short(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (_, mm, dd) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    format!("{mm:02}-{dd:02} {hh:02}:{mi:02} UTC")
}

/// 从 Unix 天数得到 `(年, 月, 日)`（Howard Hinnant 的 civil_from_days 算法）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ───────────────── 命令实现（§11：/help /new /sessions /switch /rename /delete /find） ─────────────────

/// 一条会话（供列表展示与切换）。
struct SessionBrief {
    id: String,
    title: String,
    updated_at: i64,
    is_active: bool,
}

/// 从 `GET /api/lark/chat-sessions` 的响应里解析会话列表（纯函数，便于单测）。
fn parse_session_briefs(v: &Value) -> Vec<SessionBrief> {
    v.get("sessions")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(SessionBrief {
                        id: s.get("session_id")?.as_str()?.to_string(),
                        title: s
                            .get("title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("(无标题)")
                            .to_string(),
                        updated_at: s.get("updated_at").and_then(|t| t.as_i64()).unwrap_or(0),
                        is_active: s.get("is_active").and_then(|a| a.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 会话相对时间的短说明（列表卡片右侧那点空间只够这个）。
fn brief_detail(b: &SessionBrief) -> String {
    let when = if b.updated_at > 0 {
        fmt_utc_short(b.updated_at)
    } else {
        String::new()
    };
    if b.is_active {
        if when.is_empty() {
            "当前会话".to_string()
        } else {
            format!("当前会话 · {when}")
        }
    } else {
        when
    }
}

/// 列表项与对应 id（前 [`MAX_MENU_LINES`] 条）。
fn brief_items(briefs: &[SessionBrief]) -> (Vec<ListItem>, Vec<String>) {
    let items: Vec<ListItem> = briefs
        .iter()
        .take(MAX_MENU_LINES)
        .map(|b| ListItem { title: b.title.clone(), detail: brief_detail(b) })
        .collect();
    let ids: Vec<String> = briefs.iter().take(MAX_MENU_LINES).map(|b| b.id.clone()).collect();
    (items, ids)
}

/// 记下"序号 → 动作"（供后续数字点选）。
///
/// 映射**从即将渲染的列表正文**解析出来（`card::menu_text` + `menu::resolve_text_menu`），
/// 而不是另写一套下标逻辑：两条路径（卡片按钮 / 回复数字）与列表上的序号因此**必然一致**，
/// 一旦渲染格式变了（比如不再用 `1.` 前缀），这里会立刻失配而不是悄悄给错会话。
fn remember_current_menu(chat_id: &str, briefs: &[SessionBrief]) {
    let actions: Vec<MenuAction> = briefs
        .iter()
        .take(MAX_MENU_LINES)
        .map(|b| MenuAction::Switch(b.id.clone()))
        .collect();
    let (items, _) = brief_items(briefs);
    let items = menu::resolve_text_menu(&card::menu_text(&items), &actions);
    with_menu(|m| m.remember(chat_id, now_secs(), items));
}

/// 回一张"会话列表"卡片（**同时**记下数字点选，两条路都可用）。
fn reply_session_list(http: &Http, ev: &LarkMessageEvent) -> Result<(), String> {
    let briefs = http.chat_sessions(&ev.chat_id)?;
    let (items, ids) = brief_items(&briefs);
    remember_current_menu(&ev.chat_id, &briefs);
    if items.is_empty() {
        // 空列表：给"新建"按钮，别让用户对着空卡片不知所措
        reply_card(&ev.message_id, &list_empty_card())?;
        return Ok(());
    }
    let footer = format!("共 {} 个会话 · 回复数字可直接切换", briefs.len());
    reply_card(
        &ev.message_id,
        &render_session_list_card("你的会话", &items, &ids, Some(&footer)),
    )?;
    log::info!("[bot] 已回会话列表（{} 个，chat={}）", briefs.len(), ev.chat_id);
    Ok(())
}

/// 空会话列表卡片（带「新建会话」按钮）。
fn list_empty_card() -> Value {
    let mut card = card::render_card(
        "你的会话",
        "还没有会话。发送 `/new` 或点下面的按钮新建一个。",
        None,
        None,
    );
    if let Some(elements) = card.get_mut("elements").and_then(|e| e.as_array_mut()) {
        elements.push(json!({
            "tag": "action",
            "actions": [{
                "tag": "button",
                "text": { "tag": "plain_text", "content": "新建会话" },
                "type": "primary",
                "value": card::action_payload("new_session", ""),
            }],
        }));
    }
    card
}

/// `/new`：新建会话并切换过去，回列表卡（用户立刻看到"现在在这儿"）。
fn reply_new_session(http: &Http, ev: &LarkMessageEvent) -> Result<(), String> {
    let created = http.post(
        "/api/lark/chat-sessions",
        &json!({ "chat_id": ev.chat_id, "user_open_id": ev.sender_id }),
    )?;
    let sid = created
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    log::info!("[bot] 新建会话 {sid}（chat={}）", ev.chat_id);
    reply(&ev.message_id, "已新建会话，直接提问即可。");
    reply_session_list(http, ev)
}

/// 切换后的"锚点"说明：让用户知道接上了**哪条**对话（会话名 + 最后一条提问）。
///
/// §11.6：标题是"知道自己在哪"的主要线索 —— 自动命名（首问前 20 字）或 `/rename`
/// 显式命名，这里原样带出（占位"新会话"/空则由 [`anchor_text`] 省略）。
fn switch_anchor(http: &Http, session_id: &str) -> Result<String, String> {
    let session = http.get(&format!("/api/chat/sessions/{session_id}?origin=lark"))?;
    Ok(anchor_text(&session))
}

/// 从会话里组装切换锚点文本（纯函数，便于单测）。
fn anchor_text(session: &Value) -> String {
    let name = session_title_of(session)
        .filter(|t| t != "新会话")
        .map(|t| format!("「{t}」"))
        .unwrap_or_default();
    let last_q = session
        .get("messages")
        .and_then(|m| m.as_array())
        .and_then(|arr| {
            arr.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        })
        .unwrap_or("")
        .trim()
        .to_string();
    if last_q.is_empty() {
        if name.is_empty() {
            "已切换：这是一条新会话，直接提问即可。".to_string()
        } else {
            format!("已切换到会话 {name} —— 这还是一条新会话，直接提问即可。")
        }
    } else {
        let short: String = last_q.chars().take(60).collect();
        let tail = if last_q.chars().count() > 60 {
            format!("{short}…")
        } else {
            short
        };
        if name.is_empty() {
            format!("已接上这条会话，上一个问题是：{tail}")
        } else {
            format!("已切换到会话 {name}，上一个问题是：{tail}")
        }
    }
}

/// 切换会话并回执（切完**不发**整张列表：避免刷屏，用户想再看看列表可以发 `/sessions`）。
fn do_switch(http: &Http, chat_id: &str, session_id: &str, reply_to: &str) -> Result<(), String> {
    let v = http.put(
        "/api/lark/chat-sessions/active",
        &json!({ "chat_id": chat_id, "session_id": session_id }),
    )?;
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        reply(reply_to, "这个会话不属于当前私聊，切换未生效（发 /sessions 看看列表？）");
        return Ok(());
    }
    let briefs = http.chat_sessions(chat_id)?;
    remember_current_menu(chat_id, &briefs);
    let text = switch_anchor(http, session_id).unwrap_or_else(|_| "已切换会话。".to_string());
    reply(reply_to, &format!("{text}\n（发 /sessions 查看全部会话）"));
    log::info!("[bot] 已切换会话 {session_id}（chat={chat_id}）");
    Ok(())
}

/// `/switch <序号或标题前缀>`。
///
/// 序号优先按**上次列表**解析（与用户看到的一致）；标题前缀按当前列表匹配，
/// 命中多条时列出候选而不猜。
fn reply_switch(http: &Http, ev: &LarkMessageEvent, arg: &str) -> Result<(), String> {
    let briefs = http.chat_sessions(&ev.chat_id)?;
    if briefs.is_empty() {
        reply_card(&ev.message_id, &list_empty_card())?;
        return Ok(());
    }
    // ① 纯数字 → 序号（先查上次菜单快照，再退到当前列表）
    if let Some(n) = parse_selection(arg) {
        let MenuAction::Switch(snapshot) = with_menu(|m| m.resolve(&ev.chat_id, now_secs(), n))
            .unwrap_or(MenuAction::Switch(String::new()));
        let id = if !snapshot.is_empty() {
            Some(snapshot)
        } else {
            // 快照里没有 → 用当前列表的第 n 条
            briefs.get(n - 1).map(|b| b.id.clone())
        };
        let Some(id) = id else {
            reply(&ev.message_id, &format!("没有第 {n} 个会话（发 /sessions 看看列表）。"));
            return Ok(());
        };
        return do_switch(http, &ev.chat_id, &id, &ev.message_id);
    }
    // ② 标题前缀
    let needle = arg.trim();
    let matches: Vec<&SessionBrief> = briefs
        .iter()
        .filter(|b| b.title.contains(needle))
        .collect();
    match matches.len() {
        0 => {
            reply(&ev.message_id, &format!("没有标题包含「{needle}」的会话（发 /sessions 看看列表）。"));
            Ok(())
        }
        1 => do_switch(http, &ev.chat_id, &matches[0].id, &ev.message_id),
        _ => {
            let (items, ids) = brief_items(&briefs);
            remember_current_menu(&ev.chat_id, &briefs);
            let footer = format!("「{needle}」匹配到 {} 个会话 —— 回复数字选择", matches.len());
            reply_card(
                &ev.message_id,
                &render_session_list_card("你要切换哪一个？", &items, &ids, Some(&footer)),
            )?;
            Ok(())
        }
    }
}

/// `/delete <序号>`：**先回确认卡**，真正的删除走卡片按钮（二次确认，防误删）。
fn reply_delete_confirm(http: &Http, ev: &LarkMessageEvent, arg: &str) -> Result<(), String> {
    let briefs = http.chat_sessions(&ev.chat_id)?;
    let Some(n) = parse_selection(arg) else {
        reply(&ev.message_id, "用法：`/delete 3`（先发 `/sessions` 看序号）");
        return Ok(());
    };
    let Some(target) = briefs.get(n - 1) else {
        reply(&ev.message_id, &format!("没有第 {n} 个会话（发 /sessions 看看列表）。"));
        return Ok(());
    };
    let (items, ids) = brief_items(&briefs);
    let footer = format!(
        "确认要删除「{}」吗？对话内容仍保留在本地，可在 WebUI/桌面端查看。",
        target.title
    );
    reply_card(
        &ev.message_id,
        &render_delete_list_card("删除哪个会话？", &items, &ids, Some(&footer)),
    )?;
    log::info!("[bot] 删除确认卡已发（chat={}，目标 {n}）", ev.chat_id);
    Ok(())
}

/// `/rename <新名字>`：只改**当前活动会话**的标题。
///
/// 命名是"知道自己在哪"的主要线索（卡片页眉、`/sessions` 列表、`/switch` 锚点都用它）。
/// 空参数回用法提示而不是静默；改名后回一张最新列表（用户直接看到新名字）。
fn reply_rename(http: &Http, ev: &LarkMessageEvent, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        reply(
            &ev.message_id,
            "用法：`/rename 新名字`（给**当前会话**改名；先 `/sessions` 切换到目标会话）",
        );
        return Ok(());
    }
    // 当前活动会话（没有绑定 → 先让用户有个会话可改）
    let active = http.chat_sessions(&ev.chat_id)?.into_iter().find(|b| b.is_active);
    let Some(active) = active else {
        reply(&ev.message_id, "当前还没有会话。先发 `/new` 或直接提问，再来改名。");
        return Ok(());
    };
    let body = http.put(
        &format!("/api/chat/sessions/{}/rename?origin=lark", active.id),
        &json!({ "title": name }),
    )?;
    let saved = body
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or(name)
        .to_string();
    reply(
        &ev.message_id,
        &format!("已把当前会话改名为「{saved}」（超长会截到 30 字）。"),
    );
    log::info!(
        "[bot] 已改名会话 {} → 「{saved}」（chat={}）",
        active.id,
        ev.chat_id
    );
    reply_session_list(http, ev)
}

/// `/current`（别名 `/session`）：把**当前会话**显示出来 —— 名字放页眉（最显眼），
/// 正文给"最近使用 / 消息数 / 会话总数"，并指路切换与改名。
fn reply_current(http: &Http, ev: &LarkMessageEvent) -> Result<(), String> {
    let briefs = http.chat_sessions(&ev.chat_id)?;
    let Some(active) = briefs.iter().find(|b| b.is_active) else {
        reply(
            &ev.message_id,
            "当前还没有会话 —— 直接提问会自动创建一条（发 /new 也可以）。",
        );
        return Ok(());
    };
    let session = http.get(&format!("/api/chat/sessions/{}?origin=lark", active.id))?;
    let title = session_title_of(&session).filter(|t| t != "新会话");
    let msg_count = session
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    reply_card(
        &ev.message_id,
        &current_card(title.as_deref(), active.updated_at, msg_count, briefs.len()),
    )?;
    log::info!(
        "[bot] 已回当前会话（chat={} session={} 消息 {msg_count} 条）",
        ev.chat_id,
        active.id
    );
    Ok(())
}

/// `/current` 的卡片（纯函数，便于单测）：名字进页眉（醒目、与会话配色一致）。
fn current_card(title: Option<&str>, updated_at: i64, msg_count: usize, total: usize) -> Value {
    let head = match title {
        Some(t) => format!("当前会话：{t}"),
        None => "当前会话".to_string(),
    };
    let mut lines: Vec<String> = Vec::new();
    if title.is_none() {
        lines.push("（这条还没起名 —— 提第一个问题后自动命名，或发 `/rename 新名字`）".to_string());
    }
    let when = if updated_at > 0 {
        fmt_utc_short(updated_at)
    } else {
        "未知".to_string()
    };
    lines.push(format!("最近使用：{when} · 消息 {msg_count} 条 · 共 {total} 个会话"));
    card::render_card(
        &head,
        &lines.join("\n"),
        Some("/sessions 查看全部 · /switch 切换 · /rename 改名"),
        Some(card::TEMPLATE_SESSION),
    )
}

/// `/stop`（别名 `/cancel`）：取消**当前正在生成**的回答（检索阶段同样会被中断）。
///
/// 准星来自 [`GENERATING`]（按私聊记着正在生成哪条会话）——**不猜**"当前绑定"：
/// 用户可能在中途 `/switch` 过，拿绑定去取消会打错目标。
fn reply_stop(http: &Http, ev: &LarkMessageEvent) -> Result<(), String> {
    let session_id = with_generating(|g| g.get(&ev.chat_id).cloned());
    let Some(session_id) = session_id else {
        reply(
            &ev.message_id,
            "当前没有正在生成的回答（可能已经完成）。",
        );
        return Ok(());
    };
    stop_generation(http, ev, &session_id)
}

/// `/history`（别名 `/recall`）：把**当前会话的全部内容**召回成飞书卡片。
///
/// 长会话按 [`split_for_cards`] 分多张卡（与长回答同一套分页/截断规则）：
/// 超上限时尾部合并并提示到 WebUI/桌面端看完整版。
fn reply_history(http: &Http, ev: &LarkMessageEvent) -> Result<(), String> {
    let briefs = http.chat_sessions(&ev.chat_id)?;
    let Some(active) = briefs.iter().find(|b| b.is_active) else {
        reply(
            &ev.message_id,
            "当前还没有会话 —— 直接提问会自动创建一条（发 /new 也可以）。",
        );
        return Ok(());
    };
    let session = http.get(&format!("/api/chat/sessions/{}?origin=lark", active.id))?;
    let title = session_title_of(&session).filter(|t| t != "新会话");
    let messages = session
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if messages.is_empty() {
        reply(&ev.message_id, "当前会话还没有内容 —— 直接提问即可。");
        return Ok(());
    }
    let text = build_history_text(&messages);
    let chunks = split_for_cards(&text, MAX_BODY_BYTES, MAX_CARDS);
    let pages = chunks.len();
    let mut sent = 0usize;
    for (i, chunk) in chunks.iter().enumerate() {
        let footer = (i + 1 == pages)
            .then(|| format!("共 {} 条消息 · 当前会话的完整记录", messages.len()));
        let card = render_history_card(chunk, title.as_deref(), i + 1, pages, footer.as_deref());
        match reply_card(&ev.message_id, &card) {
            Ok(_) => sent += 1,
            // 单页失败不拖垮其余页（与长回答续发同一策略）
            Err(e) => log::warn!("[bot] 历史记录第 {}/{} 页发送失败：{e}", i + 1, pages),
        }
    }
    log::info!(
        "[bot] 已回历史记录（chat={} session={} 消息 {} 条 / {sent}/{pages} 页）",
        ev.chat_id,
        active.id,
        messages.len()
    );
    Ok(())
}

/// 把会话消息渲染成召回文本（纯函数，便于单测）。
///
/// 形态：`**你：** …` / `**助手：** …` 交替、块间空行。空内容的消息（如被停止的空轮）
/// 直接跳过 —— 别在卡里留一排孤零零的名字。
fn build_history_text(messages: &[Value]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let label = if role == "user" { "你" } else { "助手" };
        blocks.push(format!("**{label}：**{content}"));
    }
    blocks.join("\n\n")
}

/// 一次 `/find` 的结果：要么直接回文本（无命中），要么回/更新一张卡。
enum FindOutcome {
    Empty(String),
    Card(Value),
}

/// 搜索并渲染结果卡（`/find` 命令与卡片上的翻页/语义按钮**共用**）。
///
/// `semantic=true` 时带上 `semantic` 参数；服务端在 Embedding 可用且有向量时才真的
/// 融合，响应里的 `semantic_applied` 告诉我们是否生效（没生效就在页脚说明，
/// 不让用户对着一个没反应的开关猜）。
/// `/api/ai/capabilities` 决定要不要显示「✦ 语义」按钮 —— 不可用时显示等于误导。
fn find_outcome(
    http: &Http,
    query: &str,
    page: usize,
    semantic: bool,
) -> Result<FindOutcome, String> {
    let query = query.trim();
    let page = page.clamp(1, crate::commands::MAX_FIND_PAGE);
    let mut url = format!(
        "/api/search?q={}&page={}&page_size={FIND_PAGE_SIZE}",
        url_encode(query),
        page
    );
    if semantic {
        url.push_str("&semantic=true");
    }
    let body = http.get(&url)?;
    let total = body.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
    let semantic_applied = body
        .get("semantic_applied")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let hits: Vec<HitBrief> = parse_hits(&body);
    if hits.is_empty() {
        let text = if total == 0 {
            format!("没有找到与「{query}」相关的内容。")
        } else {
            format!("「{query}」共 {total} 条结果，但第 {page} 页是空的（试试前面的页码）。")
        };
        return Ok(FindOutcome::Empty(text));
    }
    let items: Vec<ListItem> = hits
        .iter()
        .map(|h| ListItem {
            title: h.file_name.clone(),
            detail: search_hit_detail(h),
        })
        .collect();
    let file_ids: Vec<String> = hits.iter().map(|h| h.file_id.clone()).collect();
    let pages = (total as usize).div_ceil(FIND_PAGE_SIZE).max(1);
    let can_nav = query.chars().count() <= card::MAX_QUERY_BUTTON_CHARS;
    let mut footer = format!("第 {page}/{pages} 页 · 共 {total} 条");
    if !can_nav && page < pages {
        // query 太长、塞不进按钮 payload → 保留原来的"手打页码"提示
        footer.push_str(&format!(" · 下一页：/find {query} {}", page + 1));
    }
    if file_ids.iter().any(|id| !id.is_empty()) {
        footer.push_str(" · 点「发送 N」把原文件发到飞书");
    }
    if semantic && !semantic_applied {
        footer.push_str(" · ✦ 语义未生效（未配置 Embedding 或无向量）");
    }
    let semantic_available = http
        .get("/api/ai/capabilities")
        .ok()
        .and_then(|c| c.get("embedding").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    log::info!(
        "[bot] 搜索「{query}」第 {page}/{pages} 页，回 {}/{} 条（semantic={semantic} applied={semantic_applied}）",
        hits.len(),
        total
    );
    let view = card::SearchCardView {
        query,
        footer: Some(&footer),
        file_ids: &file_ids,
        page,
        pages,
        semantic,
        semantic_available,
    };
    Ok(FindOutcome::Card(render_search_card(&view, &items)))
}

/// `/find <关键词> [页码]`：调 `/api/search`，把命中渲染成搜索结果卡。
fn reply_find(http: &Http, ev: &LarkMessageEvent, query: &str, page: usize) -> Result<(), String> {
    let query = query.trim();
    if query.is_empty() {
        reply(
            &ev.message_id,
            "用法：`/find 关键词`（短词更容易命中；可加页码如 `/find 合同 2`；结果卡自带「上一页/下一页」与「✦ 语义」）",
        );
        return Ok(());
    }
    match find_outcome(http, query, page, false)? {
        FindOutcome::Empty(text) => reply(&ev.message_id, &text),
        FindOutcome::Card(card) => {
            reply_card(&ev.message_id, &card)?;
        }
    }
    Ok(())
}

/// 一条搜索命中的精简信息（只取渲染与"发送文件"需要的字段）。
struct HitBrief {
    /// 用于「发送 N」按钮：没有它就只能看、不能发（按钮按空 id 省略）。
    file_id: String,
    file_name: String,
    path: String,
    snippet: String,
}

/// 从 `/api/search` 响应解析命中（纯函数，便于单测）。
fn parse_hits(v: &Value) -> Vec<HitBrief> {
    v.get("hits")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| {
                    Some(HitBrief {
                        file_id: h
                            .get("file_id")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string(),
                        file_name: h.get("file_name")?.as_str()?.to_string(),
                        path: h.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string(),
                        snippet: h.get("snippet").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 搜索命中的两行说明：路径 + 片段（均按单行裁剪 —— 卡片 markdown 里换行会打乱编号）。
fn search_hit_detail(h: &HitBrief) -> String {
    let one_line = |s: &str, max: usize| -> String {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(max)
            .collect()
    };
    let path = one_line(&h.path, 90);
    let snippet = one_line(&h.snippet, 120);
    match (path.is_empty(), snippet.is_empty()) {
        (false, false) => format!("{path}\n   {snippet}"),
        (false, true) => path,
        (true, false) => snippet,
        _ => String::new(),
    }
}

/// 最小 URL 编码（query 值）：非 unreserved 一律百分号编码。
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 执行一条"菜单点选"动作（数字回复或按钮回调共用的收口）。
fn run_menu_action(http: &Http, chat_id: &str, reply_to: &str, action: &MenuAction) -> Result<(), String> {
    match action {
        MenuAction::Switch(session_id) => do_switch(http, chat_id, session_id, reply_to),
    }
}

/// 处理一条**机器人菜单事件**（`application.bot.menu_v6`）。
///
/// 两条前置事实（见 `docs/LARK_BOT.md` §11）：
/// - 菜单是**开发者后台配置**的（仅单聊），事件走长连接（`lark-cli event consume`）；
/// - 事件里**没有 `chat_type`**（它不是消息事件）——所以这里只能做**白名单**判定
///   （fail-closed），"仅私聊"由"菜单本就只存在于单聊"这个平台事实保证。
///
/// 菜单项建议配置成**推送事件**并把 `event_key` 直接写成对应的斜杠命令（如 `/new`）——
/// 这里就按命令解析执行，和用户手打完全同一条路径。
fn handle_menu_line(raw: &Value, http: &Http, allowed_users: &[String]) -> Result<(), String> {
    let operator = card::operator_open_id(raw);
    if !card::card_action_allowed(operator.as_deref(), allowed_users) {
        log::info!("[bot] 拒绝菜单点击：operator={operator:?} 不在白名单（fail-closed）");
        return Ok(());
    }
    let event_key = raw
        .get("event_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let message_id = raw.get("message_id").and_then(|v| v.as_str()).unwrap_or("").trim();
    let chat_id = raw.get("chat_id").and_then(|v| v.as_str()).unwrap_or("").trim();
    log::info!("[bot] 菜单点击 event_key={event_key:?}（chat={chat_id}）");
    if message_id.is_empty() && chat_id.is_empty() {
        log::warn!("[bot] 菜单事件既没有 message_id 也没有 chat_id，无法回复");
        return Ok(());
    }
    // event_key 就是命令（推荐配置）；空则回帮助（用户点了"帮助"但没配 key）
    let cmd_text = if event_key.is_empty() || !event_key.starts_with('/') {
        "/help".to_string()
    } else {
        event_key.to_string()
    };
    let sender = operator.clone().unwrap_or_default();
    let ev = LarkMessageEvent {
        event_type: "im.message.receive_v1".to_string(),
        message_id: message_id.to_string(),
        chat_id: chat_id.to_string(),
        chat_type: "p2p".to_string(),
        message_type: "text".to_string(),
        sender_id: sender,
        sender_type: "user".to_string(),
        content: cmd_text.clone(),
    };
    match parse_command(&cmd_text) {
        Cmd::Help => {
            reply_card(&ev.message_id, &commands::help_card())?;
            Ok(())
        }
        Cmd::Current => reply_current(http, &ev),
        Cmd::History => reply_history(http, &ev),
        Cmd::New => reply_new_session(http, &ev),
        Cmd::List => reply_session_list(http, &ev),
        Cmd::Switch(arg) => reply_switch(http, &ev, &arg),
        Cmd::Rename(name) => reply_rename(http, &ev, &name),
        Cmd::Stop => reply_stop(http, &ev),
        Cmd::Delete(arg) => reply_delete_confirm(http, &ev, &arg),
        Cmd::Find { query, page } => reply_find(http, &ev, &query, page),
        // 菜单项配成了别的文案（不是命令）：回帮助，别猜
        Cmd::Ai => {
            reply(
                &ev.message_id,
                "菜单项未配置为命令。可选：/current、/new、/sessions、/switch <序号或标题>、/rename <新名字>、/stop、/delete <序号>、/find <关键词>、/help",
            );
            Ok(())
        }
    }
}

/// 处理一条卡片回调：**先过身份闸门，再路由动作**。
///
/// ⚠️ 回调是**另一条入口** —— 消息入口的 L0/L1/L2 闸门**不会**自动保护它，所以这里必须再判一次
/// （`card_action_allowed`，fail-closed：取不到操作者或不在白名单一律拒绝）。
fn handle_card_line(http: &Arc<Http>, line: &str, allowed_users: &[String]) -> Result<(), String> {
    let raw: Value = serde_json::from_str(line).map_err(|e| {
        format!("回调 JSON 解析失败：{e}（前 200 字符：{}）", card::clip_str(&line, 200))
    })?;
    let ev: CardActionEvent =
        serde_json::from_value(raw.clone()).map_err(|e| format!("回调字段解析失败：{e}"))?;
    if ev.event_type != "card.action.trigger" {
        return Ok(()); // 该消费者只接这一类
    }

    let operator = crate::card::operator_open_id(&raw);
    if !crate::card::card_action_allowed(operator.as_deref(), allowed_users) {
        log::info!(
            "[bot] 拒绝卡片操作：operator={operator:?} 不在白名单（fail-closed）"
        );
        return Ok(());
    }

    match crate::card::parse_card_action(&ev) {
        None => {
            log::info!("[bot] 忽略无法识别的卡片动作（action_value={:?}）", ev.action_value);
            Ok(())
        }
        Some(action) => {
            log::info!(
                "[bot] 收到卡片动作 {action:?}（chat={} operator={operator:?}）",
                ev.chat_id
            );
            match action {
                crate::card::CardAction::OpenDetail { session_id } => {
                    // 动作本身**不需要回调 token**：直接 reply 一条提示即可
                    // （§8.1：回调 token 那条路有"最多 2 次更新"限制，能不用就不用）。
                    let text = match webui_hint(http, &session_id) {
                        Ok(t) => t,
                        Err(e) => format!("暂时拿不到 WebUI 地址：{e}\n（会话 id：{session_id}）"),
                    };
                    reply(&ev.message_id, &text);
                    Ok(())
                }
                crate::card::CardAction::Retry { session_id } => {
                    // D-4b：**复用主链路** —— 把重跑当成"用户又问了一遍"。
                    // 事件用回调里的 id 合成（白名单已在回调入口判过，故这里不会再被拒）。
                    let session = http.get(&format!("/api/chat/sessions/{session_id}?origin=lark"))?;
                    let messages: Vec<Value> = session
                        .get("messages")
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default();
                    // 问题 = 该会话里**最后一条 user 消息**
                    let question = messages
                        .iter()
                        .rev()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if question.is_empty() {
                        reply(&ev.message_id, "找不到可重跑的问题（这会话里没有用户消息）。");
                        return Ok(());
                    }
                    // 上一轮回答：作为 `retry_prior_answer` 交给服务端，让它**换一批检索词**
                    // 重搜（原词再搜一遍等于白点一次）。注意服务端的"问题"口径也是
                    // **最后一条 user 消息**（`commands/ai/rewrite.rs::question_from_messages`）——
                    // 两边一致，重跑才是"对原问题换词重搜"，而不是"对上一条回答再问一遍"（那是 P1 的旧行为）。
                    let prior_answer = messages
                        .iter()
                        .rev()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let synth = LarkMessageEvent {
                        event_type: "im.message.receive_v1".to_string(),
                        message_id: ev.message_id.clone(),
                        chat_id: ev.chat_id.clone(),
                        chat_type: "p2p".to_string(),
                        message_type: "text".to_string(),
                        sender_id: operator.clone().unwrap_or_default(),
                        sender_type: "user".to_string(),
                        content: question.clone(),
                    };
                    log::info!(
                        "[bot] 重跑会话 {session_id} 的最后一问（{} 字；带上轮回答 {} 字用于换词扩查）",
                        question.chars().count(),
                        prior_answer.chars().count()
                    );
                    let retry = (!prior_answer.is_empty()).then(|| prior_answer.as_str());
                    // 重跑也是一次生成：走同一道"每私聊串行"守门（与消息入口共用）。
                    let Some(_guard) = GeneratingGuard::begin(&ev.chat_id, &session_id) else {
                        reply(
                            &ev.message_id,
                            "这个私聊正在生成回答，请稍候（发 /stop 可中止）。",
                        );
                        return Ok(());
                    };
                    answer_with_card(http, &synth, &question, &session_id, session, messages, retry)
                        .map_err(|e| format!("重跑失败：{e}"))
                }
                crate::card::CardAction::NewSession => {
                    // 合成一条"用户发了 /new"的事件：与手打 /new 完全同一条路径
                    let synth = LarkMessageEvent {
                        event_type: "im.message.receive_v1".to_string(),
                        message_id: ev.message_id.clone(),
                        chat_id: ev.chat_id.clone(),
                        chat_type: "p2p".to_string(),
                        message_type: "text".to_string(),
                        sender_id: operator.clone().unwrap_or_default(),
                        sender_type: "user".to_string(),
                        content: "/new".to_string(),
                    };
                    reply_new_session(http, &synth)
                }
                crate::card::CardAction::Switch { session_id } => {
                    run_menu_action(http, &ev.chat_id, &ev.message_id, &MenuAction::Switch(session_id))
                }
                crate::card::CardAction::Remove { session_id, index } => {
                    // 「删除」按钮 → **确认卡**（真正的删除走 remove_confirm）
                    let briefs = http.chat_sessions(&ev.chat_id)?;
                    let (items, ids) = brief_items(&briefs);
                    let title = briefs
                        .iter()
                        .find(|b| b.id == session_id)
                        .map(|b| b.title.clone())
                        .unwrap_or_else(|| format!("第 {index} 个会话"));
                    let footer = format!(
                        "确认要删除「{title}」吗？对话内容仍保留在本地，可在 WebUI/桌面端查看。"
                    );
                    reply_card(
                        &ev.message_id,
                        &render_delete_list_card("删除哪个会话？", &items, &ids, Some(&footer)),
                    )?;
                    log::info!("[bot] 删除确认卡已发（chat={}，目标 {index}）", ev.chat_id);
                    Ok(())
                }
                crate::card::CardAction::RemoveConfirm { session_id, index } => {
                    let v = http.post(
                        "/api/lark/chat-sessions/delete",
                        &json!({ "chat_id": ev.chat_id, "session_id": session_id }),
                    )?;
                    let deleted = v.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false);
                    if !deleted {
                        reply(&ev.message_id, "这条会话已经不在了（可能刚被删过）。");
                        return Ok(());
                    }
                    log::info!("[bot] 已删除会话 {session_id}（chat={}）", ev.chat_id);
                    // 回执 + 最新列表（合成一条事件：列表渲染不需要真实的 sender/内容）
                    let synth = LarkMessageEvent {
                        event_type: "im.message.receive_v1".to_string(),
                        message_id: ev.message_id.clone(),
                        chat_id: ev.chat_id.clone(),
                        chat_type: "p2p".to_string(),
                        message_type: "text".to_string(),
                        sender_id: operator.clone().unwrap_or_default(),
                        sender_type: "user".to_string(),
                        content: String::new(),
                    };
                    reply(&ev.message_id, &format!("已删除第 {index} 个会话映射。"));
                    reply_session_list(http, &synth)
                }
                crate::card::CardAction::Stop { session_id } => {
                    // 与 `/stop` 同一条实现：按会话取消在途生成。
                    // 这条路径走**卡片回调线程**，与正在流式生成的线程并行 —— 因此按钮真能"打断"。
                    let synth = LarkMessageEvent {
                        event_type: "im.message.receive_v1".to_string(),
                        message_id: ev.message_id.clone(),
                        chat_id: ev.chat_id.clone(),
                        chat_type: "p2p".to_string(),
                        message_type: "text".to_string(),
                        sender_id: operator.clone().unwrap_or_default(),
                        sender_type: "user".to_string(),
                        content: String::new(),
                    };
                    stop_generation(http, &synth, &session_id)
                }
                crate::card::CardAction::SendFile { file_id } => {
                    // 上传可能是几十 MB（秒级）→ **不能阻塞回调消费者**（它还要处理其它按钮）。
                    // 单独线程里跑，失败回一条文本（线程里没有返回值可传）。
                    let http = Arc::clone(http);
                    let message_id = ev.message_id.clone();
                    log::info!(
                        "[bot] 发送文件 {file_id}（chat={} operator={operator:?}）",
                        ev.chat_id
                    );
                    std::thread::spawn(move || {
                        if let Err(e) = send_tracked_file(&http, &message_id, &file_id) {
                            log::warn!("[bot] 发送文件 {file_id} 失败：{e}");
                            reply(&message_id, &format!("发送文件失败：{e}"));
                        }
                    });
                    Ok(())
                }
                crate::card::CardAction::SearchAgain { query, page, semantic } => {
                    // 搜索本身很快，但 `semantic=true` 可能等 embedding（最长 5s）
                    // → 与发送文件一样放独立线程，不阻塞回调消费者。
                    // 成功后**原地更新这张卡**（app 身份 PATCH，无"最多 2 次"限制）；
                    // PATCH 失败才退回"回一张新卡"。
                    let http = Arc::clone(http);
                    let message_id = ev.message_id.clone();
                    log::info!(
                        "[bot] 搜索卡动作 query={query:?} page={page} semantic={semantic}（chat={}）",
                        ev.chat_id
                    );
                    std::thread::spawn(move || match find_outcome(&http, &query, page, semantic) {
                        Ok(FindOutcome::Empty(text)) => reply(&message_id, &text),
                        Ok(FindOutcome::Card(new_card)) => {
                            if let Err(e) = patch_card(&message_id, &new_card) {
                                log::warn!("[bot] 原地更新搜索卡失败（{e}）→ 回一张新卡");
                                if let Err(e2) = reply_card(&message_id, &new_card) {
                                    log::warn!("[bot] 回搜索卡也失败：{e2}");
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("[bot] 重新搜索失败：{e}");
                            reply(&message_id, &format!("搜索失败：{e}"));
                        }
                    });
                    Ok(())
                }
            }
        }
    }
}

/// 按会话请求取消在途生成（`/stop` 与卡片「停止」按钮共用）。
///
/// `session_id` 从**发起方**拿：命令走 [`GENERATING`] 的准星，按钮用卡片 payload 里的 id ——
/// 两条路都**不看**"当前绑定"（用户可能中途切走）。
fn stop_generation(http: &Http, ev: &LarkMessageEvent, session_id: &str) -> Result<(), String> {
    match http.post("/api/ai/cancel", &json!({ "session_id": session_id })) {
        Ok(_) => {
            reply(
                &ev.message_id,
                "已请求停止 —— 已经生成的部分会保留在回答卡片里。",
            );
            log::info!(
                "[bot] 已请求停止会话 {session_id} 的生成（chat={}，入口={}）",
                ev.chat_id,
                if ev.content.is_empty() { "按钮" } else { "命令" }
            );
        }
        Err(e) => {
            reply(&ev.message_id, &format!("停止请求失败：{e}"));
        }
    }
    Ok(())
}

// ───────────────────────────── 会话问答：两条路径 ─────────────────────────────

/// 两个会话问答路由**共用**的请求体（省得两处写法漂移）。
///
/// - 非流式 `/api/ai/conversation/ask`：**忽略** `sessionId`，历史只能由我们带上；
/// - 流式 `/api/ai/conversation/ask/stream`：**接收** `sessionId` → 证据按 session 按轮绑定
///   （这正是"追问时「他」指代不明确"的根因，见 §8.1）。
///
/// `scope` 直接发 `{}`：服务端字段全是 `#[serde(default)]`，语义等同全默认。
///
/// `retry_prior_answer`：**只有重跑**（卡片「换一批证据」）才带这个键 —— 值 = 上一轮
/// 回答正文。服务端据此**换一批检索词**重搜（见 `commands::ai::prompt` 的 retry 分支）。
/// 其余请求保持原来的请求体形状，不塞空字段。
fn ask_body(session_id: &str, messages: &[Value], retry_prior_answer: Option<&str>) -> Value {
    let mut body = json!({
        "messages": messages,
        "sourceIds": [],
        "scope": {},
        "sessionRetrievalScope": [],
        "strictDocs": false,
        "fullRecall": false,
        "sessionId": session_id,
    });
    if let Some(prior) = retry_prior_answer {
        body["retryPriorAnswer"] = json!(prior);
    }
    body
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
    retry_prior_answer: Option<&str>,
) -> Result<(), String> {
    let asked = http.post("/api/ai/conversation/ask", &ask_body(session_id, &messages, retry_prior_answer))?;
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
    retry_prior_answer: Option<&str>,
) -> Result<(), String> {
    // 会话名（页眉用）：占位标题由 `card::headline` 隐藏，这里只管取。
    let title = session_title_of(&session);
    let card_id = reply_card(
        &ev.message_id,
        &render_working("正在检索…", "", title.as_deref(), session_id),
    )?;
    log::info!("[bot] 已回卡片 {card_id}（chat={}）", ev.chat_id);

    match stream_turn(http, session_id, &messages, &card_id, retry_prior_answer, title.as_deref()) {
        Ok(turn) => {
            // 停止发生在"还没生成任何内容"时（如检索阶段被 `/stop`）：**合法收尾**，
            // 把卡片换成"已停止"，且**不写历史**（没有内容可写）。
            if turn.full_text.trim().is_empty() {
                log::info!("[bot] 已在生成内容前停止（session={session_id}）");
                let card = render_answer_card(
                    "（已停止：还没有生成任何内容）",
                    title.as_deref(),
                    1,
                    1,
                    0,
                    0,
                    true,
                );
                if let Err(e) = patch_card(&card_id, &card) {
                    log::warn!("[bot] 收尾卡片更新失败：{e}");
                }
                return Ok(());
            }
            let mut done = messages;
            done.push(json!({ "role": "assistant", "content": turn.full_text }));
            save_turn(http, session_id, &mut session, &done);

            // 超长回答**分多张卡片**（"过长也要能完成"）：
            // 第 1 段更新那张流式卡片（用户正在看它），第 2..N 段作为**新的回复卡片续发**。
            // 续发只发最终内容、不参与流式更新 → **不额外增加 QPS**。
            let chunks = split_for_cards(&turn.full_text, MAX_BODY_BYTES, MAX_CARDS);
            let pages = chunks.len();
            let mut sent_pages = 0usize;
            let mut first = render_answer_card(
                &chunks[0],
                title.as_deref(),
                1,
                pages,
                turn.evidence_count,
                turn.took_ms,
                turn.cancelled,
            );
            if pages == 1 {
                // 只有一张 → 按钮挂它；多张 → 挂最后一张（见下）
                card::append_actions(&mut first, session_id);
            }
            if let Err(e) = patch_card(&card_id, &first) {
                // 收尾更新失败：至少把答案给出去（此时卡片里还没有最终内容，不会重复）
                log::warn!("[bot] 收尾卡片更新失败：{e}");
                reply(&ev.message_id, &truncate_reply(&turn.full_text));
            } else {
                sent_pages = 1;
                for (i, chunk) in chunks.iter().enumerate().skip(1) {
                    let mut card = render_answer_card(
                        chunk,
                        title.as_deref(),
                        i + 1,
                        pages,
                        turn.evidence_count,
                        turn.took_ms,
                        turn.cancelled,
                    );
                    if i + 1 == pages {
                        card::append_actions(&mut card, session_id);
                    }
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
fn stream_turn(
    http: &Http,
    session_id: &str,
    messages: &[Value],
    card_id: &str,
    retry_prior_answer: Option<&str>,
    title: Option<&str>,
) -> Result<Turn, String> {
    let mut throttle = Throttle::standard();
    let mut acc = SseAccum::default();
    let mut partial = String::new();
    let mut progress = String::from("正在检索…");
    let mut done_payload: Option<Value> = None;

    http.stream("/api/ai/conversation/ask/stream", &ask_body(session_id, messages, retry_prior_answer), |line| {
        let Some((event, data)) = acc.push_line(line) else {
            return Ok(true);
        };
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            log::warn!("[bot] SSE 帧不是 JSON（忽略）：{}", card::clip_str(&data, 120));
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
            && let Err(e) = patch_card(card_id, &render_working(&progress, &partial, title, session_id))
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
        if cancelled {
            // 停止发生在"还没生成任何内容"时（检索阶段被 `/stop`/按钮打断）：
            // **合法收尾**，不是错误 —— 上层据此把卡片标成"已停止"且不写历史。
            log::info!("[bot] 本轮在生成任何内容前被停止");
            return Ok(Turn {
                full_text: String::new(),
                evidence_count: 0,
                took_ms: 0,
                cancelled: true,
                updates: throttle.sent(),
            });
        }
        // 失败收尾：webapi 在生成任务出错时会补一发**空内容** `ai-done`（携带 `error` 原因），
        // 没有原因字段时退回兜底文案。
        let detail = done
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        return Err(if detail.is_empty() {
            "回答为空（主程序日志里应有原因）".to_string()
        } else {
            detail
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
        format!("lark-cli 输出不是 JSON：{e}（前 200 字符：{}）", card::clip_str(&stdout, 200))
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

// ───────────────────────────── 出站媒体（项目 → 飞书）─────────────────────────────

/// 把一个**索引里的文件**发到飞书（`/find` 结果卡上的「发送 N」按钮落到这里）。
///
/// 数据流：`file_id` → `GET /api/files/{id}/abs-path`（服务端校验路径仍在索引目录内）
/// → 按扩展名/大小选形态 → [`media::reply_media_file`]（**一步式**上传+回复）。
///
/// 三条边界：
/// - **只认 file_id**：不接受任意路径（模型/用户都不能指定"发哪个本地文件"）；
/// - 空文件 / >30MB 不硬发，回一条说明（>30MB 时尽量附 WebUI 地址）；
/// - 失败**回文本**而不是静默 —— 用户点了按钮必须有反馈。
fn send_tracked_file(http: &Http, message_id: &str, file_id: &str) -> Result<(), String> {
    let meta = http.get(&format!("/api/files/{file_id}/abs-path"))?;
    let abs = meta.get("abs_path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let name = meta
        .get("file_name")
        .and_then(|v| v.as_str())
        .unwrap_or("file")
        .trim()
        .to_string();
    let ext = meta.get("file_ext").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if abs.is_empty() {
        reply(message_id, "服务端没返回文件路径，无法发送。");
        return Ok(());
    }
    let path = PathBuf::from(&abs);
    // 以磁盘上的**实际大小**为准（DB 里的 size 可能已过期）
    let size = match std::fs::metadata(&path) {
        Ok(m) if m.is_file() => m.len(),
        _ => {
            reply(message_id, &format!("文件「{name}」不在磁盘上（可能已被移动或删除）。"));
            return Ok(());
        }
    };
    if size == 0 {
        reply(message_id, &format!("文件「{name}」是空文件，飞书不允许上传 0 字节。"));
        return Ok(());
    }
    if size > media::FILE_MAX_BYTES {
        let mb = size as f64 / 1_048_576.0;
        let mut text = format!(
            "「{name}」{mb:.1} MB，超过飞书单文件 30 MB 上限，无法直接发送。"
        );
        if let Ok(origin) = webui_origin(http) {
            text.push_str(&format!("\n可在 WebUI 里查看：{origin}"));
        }
        reply(message_id, &text);
        return Ok(());
    }

    let program = lark_cli_program();
    let audio = media::audio_plan(&ext, ffmpeg_program().is_some());
    let mut converted: Option<PathBuf> = None;
    let result = (|| -> Result<(), String> {
        match audio {
            media::AudioPlan::Direct => {
                media::reply_media_file(&program, message_id, media::MediaKind::Audio, &path)
            }
            media::AudioPlan::Convert => match convert_to_opus(&path) {
                Some(opus) => {
                    converted = Some(opus.clone());
                    media::reply_media_file(&program, message_id, media::MediaKind::Audio, &opus)
                }
                // 转码失败（没有 ffmpeg / 源文件损坏）→ 退回普通文件，别让用户拿不到东西
                None => {
                    media::reply_media_file(&program, message_id, media::MediaKind::File, &path)
                }
            },
            media::AudioPlan::AsFile => match media::classify(&ext, size) {
                Some(media::MediaPlan::Image) => {
                    media::reply_media_file(&program, message_id, media::MediaKind::Image, &path)
                }
                Some(media::MediaPlan::File) => {
                    media::reply_media_file(&program, message_id, media::MediaKind::File, &path)
                }
                // 大小已在上面挡过；这里只是把"决策返回 None"也收口成可读错误
                None => Err(format!("文件「{name}」不满足飞书的发送限制")),
            },
        }
    })();
    if let Some(tmp) = converted {
        // 转码产物在**独占临时目录**里：整目录清掉（无论成败）
        if let Some(dir) = tmp.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    match result {
        Ok(()) => {
            log::info!(
                "[bot] 已发送文件「{name}」（{size} 字节，形态={audio:?}，file_id={file_id}）"
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// 解析 ffmpeg：应用经 `LS_FFMPEG` 注入（依赖中心解析的路径），否则退回 PATH 上的 `ffmpeg`。
///
/// **每次都探**（不缓存"没找到"）：与 `extractor::audio` 的策略一致 —— 装好 ffmpeg
/// 之后不必重启 bot 就能发语音。
fn ffmpeg_program() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LS_FFMPEG") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| PathBuf::from("ffmpeg"))
}

/// mp3/wav/… → Opus（飞书语音消息的唯一可用编码）；失败返回 `None`。
///
/// 产物放在**独占临时目录** `link-searcher-lark-<pid>-<millis>/<原名主干>.opus`：
/// - 独占目录：并发转码不撞名，清理时整目录删（调用方 `remove_dir_all`）；
/// - 文件名沿用**原主干**：一步式发送（`+messages-reply --audio`）会用磁盘上的文件名，
///   随机名会污染语音消息的显示名。
///
/// 参数按飞书文档给的建议：`-acodec libopus -ac 1 -ar 16000`（单声道 16kHz）。
fn convert_to_opus(src: &Path) -> Option<PathBuf> {
    let ffmpeg = ffmpeg_program()?;
    let dir = std::env::temp_dir().join(format!(
        "link-searcher-lark-{}-{}",
        std::process::id(),
        now_millis()
    ));
    std::fs::create_dir_all(&dir).ok()?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim();
    let stem = if stem.is_empty() { "voice" } else { stem };
    let out = dir.join(format!("{stem}.opus"));
    let status = Command::new(&ffmpeg)
        .args(["-y", "-i"])
        .arg(src)
        .args(["-acodec", "libopus", "-ac", "1", "-ar", "16000"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() || !out.is_file() {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    Some(out)
}

/// Unix 毫秒时间戳（临时文件名用；失败回 0，不影响唯一性 —— pid 也在名字里）。
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
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
/// **Unix（mac/Linux）不需要这套前缀推导**：npm 全局安装的 `lark-cli` 就是
/// `<prefix>/bin/lark-cli`（软链或 shim），而那个 `bin` 目录本来就在 PATH 上 ——
/// 直接交回裸名 `lark-cli`，由 `Command::new` 按 PATH 解析即可（这也是末尾兜底做的事）。
///
/// 正常情况下这里根本不会被用到：**应用**（依赖中心）解析出的路径会经 `LS_LARK_CLI` 注入
/// （见 `lark_bot.rs`），本函数只是"bot 被手工拉起"时的兜底。
///
/// TODO(依赖中心)：这段发现逻辑应迁到"依赖中心"（见 §十 的 `lark-cli` 决策）。
fn lark_cli_program() -> PathBuf {
    if let Ok(p) = std::env::var("LS_LARK_CLI") {
        return PathBuf::from(p);
    }
    if cfg!(windows) {
        // 与依赖中心同源的两个候选根：便携/解包布局（node.exe 旁边）与官方安装器的 %APPDATA%\npm
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs").join("nodejs"));
        }
        if let Some(roaming) = std::env::var_os("APPDATA") {
            roots.push(PathBuf::from(roaming).join("npm"));
        }
        for root in roots {
            let native = root
                .join("node_modules")
                .join("@larksuite")
                .join("cli")
                .join("bin")
                .join("lark-cli.exe");
            if native.is_file() {
                return native;
            }
        }
    }
    PathBuf::from(if cfg!(windows) { "lark-cli.cmd" } else { "lark-cli" })
}

fn spawn_consumer(event_key: &str) -> Result<Child, String> {
    let program = lark_cli_program();
    log::info!("[bot] 启动事件消费：{} （key={event_key}）", program.display());
    Command::new(&program)
        .args(["event", "consume", event_key, "--as", "bot"])
        .stdin(Stdio::piped()) // 保留 stdin：优雅停机靠"关 stdin"（Windows 没有可用的 SIGTERM）
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 lark-cli（{}）：{e}", program.display()))
}

/// 卡片回调消费者的**重连退避**（纯函数，便于单测）。
///
/// - 就绪过再断：5s 起（多为网络抖动，尽快恢复按钮）；
/// - 从未就绪：30s 起（大概率订阅/配置问题，别空转）；
/// - 指数退避，300s 封顶。
fn consumer_backoff(was_ready: bool, attempt: u32) -> Duration {
    let base: u64 = if was_ready { 5 } else { 30 };
    let secs = base.saturating_mul(1u64 << attempt.min(4)).min(300);
    Duration::from_secs(secs)
}

/// 起**卡片回调**消费者（`card.action.trigger`）并在后台线程里逐行处理。
///
/// **失败只记日志 + 自动重连**：回调不可用不该影响消息主链路，也**不能永久失效** ——
/// 翻页 / 语义 / 发送文件 / 停止全走按钮，进程一死就等于半个机器人瘫痪
/// （2026-10-18 审计：原实现只记一条"按钮将不可用"就再也不管了）。
fn spawn_card_consumer(http: Arc<Http>, allowed_users: Vec<String>) {
    std::thread::spawn(move || {
        let mut failures: u32 = 0;
        loop {
            let started = Instant::now();
            let outcome = run_card_consumer_once(&http, &allowed_users);
            // 稳定跑过一段（≥60s）再断 → 计数归零；刚起就断（<60s）→ 视为不稳定，
            // 计数累加（避免"反复秒断"被当成正常抖动而 5s 一次空转）。
            let (was_ready, detail) = match outcome {
                Ok(()) if started.elapsed() >= Duration::from_secs(60) => {
                    failures = 0;
                    (true, String::new())
                }
                Ok(()) => {
                    failures = failures.saturating_add(1);
                    (true, String::new())
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    (false, e)
                }
            };
            let wait = consumer_backoff(was_ready, failures);
            let attempt = failures.max(1);
            if detail.is_empty() {
                log::warn!(
                    "[bot] 卡片回调{} → {}s 后重连（第 {} 次）",
                    if was_ready { "连接断开" } else { "未就绪" },
                    wait.as_secs(),
                    attempt
                );
            } else {
                log::warn!(
                    "[bot] 卡片回调消费者异常（{detail}）→ {}s 后重连（第 {attempt} 次）",
                    wait.as_secs()
                );
            }
            std::thread::sleep(wait);
        }
    });
}

/// 卡片回调消费者的**一次完整生命周期**：起进程 → 等就绪 → 读到 EOF → 回收子进程。
///
/// `Ok(())` = 长连接**就绪过**（之后断开/结束）；`Err` = 就绪前失败（订阅没配 / 启动失败）。
fn run_card_consumer_once(
    http: &Arc<Http>,
    allowed_users: &[String],
) -> Result<(), String> {
    let mut child = spawn_consumer("card.action.trigger")?;
    if let Err(e) = wait_ready(&mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("长连接未就绪：{e}"));
    }
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("拿不到 stdout".to_string());
    };
    log::info!("[bot] 卡片回调长连接就绪");
    // ⚠️ `child` 必须**被本函数持有到读完为止**：lark-cli 的优雅停机方式就是
    // "关 stdin"，所以 `Child` 一旦 drop（stdin 管道 EOF）子进程会**立刻退出**。
    // 2026-10-16 修：原先只把 `stdout` 移进线程、`child` 随函数返回被 drop，
    // 于是回调消费者在"就绪"后 1 秒内就死（日志：`stdin closed → shutting down`）
    // —— **卡片按钮从来没能用**。
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Err(e) = handle_card_line(http, line, allowed_users) {
            log::warn!("[bot] 处理卡片回调失败：{e}");
        }
    }
    log::warn!("[bot] 卡片回调 stdout 结束");
    let _ = child.wait(); // 回收子进程，别留僵尸
    Ok(())
}

// ───────────────────────── 机器人菜单消费者（§11）─────────────────────────

/// 菜单消费者：**确定性失败**（事件未订阅）与**暂时性失败**的退避不同
/// （纯函数，便于单测）。
///
/// 未订阅 = 要用户去开发者后台配置/发布，**不会自己好** → 1 小时退避（原实现每 5 分钟
/// 起一个 lark-cli、等 15s、刷 20 行错误 JSON —— 2026-10-18 审计从日志里看到的浪费）。
fn menu_backoff(not_subscribed: bool) -> Duration {
    if not_subscribed {
        Duration::from_secs(3600)
    } else {
        Duration::from_secs(300)
    }
}

/// stderr 里是否出现"事件未订阅"（确定性失败）。大小写不敏感。
fn is_not_subscribed(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("not subscribed")
}

/// 起**机器人菜单**（`application.bot.menu_v6`）消费者并在后台线程里逐行处理。
///
/// 与卡片回调（[`spawn_card_consumer`]）的两处不同，都是刻意的：
/// - **轮询式**（[`poll_consumer_into`]）而不是"就绪后阻塞读"：菜单订阅在开发者后台
///   没配好时，长连接**永远不出现就绪标记** —— 若像卡片回调那样在初始化线程里等，
///   一个后台线程会被永久占住（不致命但不干净）。这里短等即放弃，退避后再试。
/// - 线程**每次尝试重建**（含 `Child`）：拿不到 stdout 时子进程要能被正确回收。
///
/// 失败只记日志（降级即"点菜单没反应"）—— 消息与卡片两条主路不受影响。
fn spawn_menu_consumer(http: Arc<Http>, allowed_users: Vec<String>) {
    std::thread::spawn(move || loop {
        match spawn_consumer(MENU_EVENT_TYPE) {
            Ok(mut child) => match poll_consumer_into(&mut child, "菜单".to_string(), |line| {
                // 菜单事件没有 chat_type —— 按"仅私聊"的既定事实，构造一份与主链路
                // 一致的策略（`require_p2p = true` 对消息有效；菜单事件走白名单判定）
                let gate_cfg = LarkGateConfig {
                    require_p2p: true,
                    allowed_users: allowed_users.clone(),
                    pairing: false,
                };
                if let Err(e) = handle_line(line, &http, &gate_cfg) {
                    log::warn!("[bot] 处理菜单事件失败：{e}");
                }
            }) {
                Ok(poll) => {
                    if poll.lines > 0 {
                        log::info!("[bot] 菜单长连接断开，5 秒后重试");
                    } else {
                        let wait = menu_backoff(poll.not_subscribed);
                        if poll.not_subscribed {
                            log::warn!(
                                "[bot] 菜单事件未订阅（开发者后台未配置/未发布自定义菜单？）\
                                 —— {}s 后重试；配置好并发布版本后重启应用即可立即生效",
                                wait.as_secs()
                            );
                        } else if poll.stderr_summary.is_empty() {
                            log::warn!("[bot] 菜单长连接未就绪 —— {}s 后重试", wait.as_secs());
                        } else {
                            log::warn!(
                                "[bot] 菜单长连接未就绪：{} —— {}s 后重试",
                                poll.stderr_summary,
                                wait.as_secs()
                            );
                        }
                        std::thread::sleep(wait);
                        continue;
                    }
                }
                Err(e) => log::warn!("[bot] 读取菜单事件失败：{e}"),
            },
            Err(e) => log::warn!("[bot] 菜单消费者启动失败（点菜单将不可用）：{e}"),
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// 菜单消费者一次轮询的结果。
struct MenuPoll {
    /// 读到的 stdout 行数；`0` ＝ 等不到就绪（大概率订阅没配）。
    lines: usize,
    /// stderr 摘要（失败时用于**一行**日志；成功时为空）。
    stderr_summary: String,
    /// stderr 明确说"事件未订阅"（确定性失败 → 长退避）。
    not_subscribed: bool,
}

/// 短等就绪后轮询式读取一路子进程的输出，逐行交给 `on_line`。
///
/// 为什么不是 [`wait_ready`] + 阻塞读：那个组合要求"就绪标记一定会出现"，否则读线程
/// 会一直挂着。菜单订阅则是**可选**的（后台没配就没有标记），所以这里：
/// - 最多等 [`MENU_READY_TIMEOUT`]；
/// - 之后**非阻塞**轮询（`try_wait` + 已就绪后阻塞读的组合在这里不适用，
///   因为 lark-cli 的输出是行缓冲的管道 —— 用 `read_until(b'\n')` 配超时太绕，
///   改为"就绪即转入阻塞读"：一旦拿到 ready，说明订阅有效，之后的读是安全的）。
///
/// **日志策略**（2026-10-18 审计后改）：只有 `[event]` / `[source]` 开头的诊断行实时落
/// info；错误 JSON 这类"多行文本"改为 debug（默认级别不落盘）并累积成**一条摘要** ——
/// 原实现每 5 分钟把 20 行 JSON 逐行刷进日志。
fn poll_consumer_into(
    child: &mut Child,
    tag: String,
    mut on_line: impl FnMut(&str),
) -> Result<MenuPoll, String> {
    let ready = Arc::new(AtomicBool::new(false));
    let ready_reader = Arc::clone(&ready);
    let diag = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let diag_reader = Arc::clone(&diag);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.starts_with("[event]") || line.starts_with("[source]") {
                    log::info!("[lark-cli/{tag}] {line}");
                    if line.contains("[event] ready event_key=") {
                        ready_reader.store(true, Ordering::Release);
                    }
                } else {
                    log::debug!("[lark-cli/{tag}] {line}");
                    let mut d = diag_reader.lock().unwrap_or_else(|e| e.into_inner());
                    if d.len() < 4000 {
                        d.push_str(&line);
                        d.push('\n');
                    }
                }
            }
        });
    }
    let deadline = Instant::now() + MENU_READY_TIMEOUT;
    while Instant::now() < deadline {
        if ready.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !ready.load(Ordering::Acquire) {
        let _ = child.kill();
        let _ = child.wait();
        // 给 stderr 读线程一点时间把剩余行收完（kill 会关闭管道）
        std::thread::sleep(Duration::from_millis(200));
        let raw = diag.lock().unwrap_or_else(|e| e.into_inner()).clone();
        return Ok(MenuPoll {
            lines: 0,
            stderr_summary: summarize_stderr(&raw),
            not_subscribed: is_not_subscribed(&raw),
        });
    }
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("拿不到菜单消费者的 stdout".to_string());
    };
    let mut read = 0usize;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        read += 1;
        on_line(line);
    }
    let _ = child.wait(); // 回收子进程
    Ok(MenuPoll {
        lines: read,
        stderr_summary: String::new(),
        not_subscribed: false,
    })
}

/// 把累积的 stderr 压成**一行**摘要：优先取错误 JSON 里信息量最大的 `"message": "…"`，
/// 拿不到就把整段空白压平再截断。纯函数，便于单测。
fn summarize_stderr(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("\"message\":") {
            let v = rest.trim().trim_matches(|c| c == '"' || c == ',');
            if !v.is_empty() {
                return clip_chars(v, 200);
            }
        }
    }
    clip_chars(&raw.split_whitespace().collect::<Vec<_>>().join(" "), 200)
}

/// 按**字符**截断（不是字节）并加省略号。
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// 从 `GET /api/session` 的状态里选 WebUI 的**外部可达基址**。
///
/// 优先级（多网卡机器的取舍，见 `docs/LARK_BOT.md` §5.13）：
/// 1. `webui_base_url`（用户手配，例如固定域名 / 反向代理）—— 显式配置永远最优先；
/// 2. `tailscale_ip`（100.64/10）—— **同时适用于同一 Wi-Fi 与异地**，是"不做网络判断"
///    也能点开的最佳答案；
/// 3. `lan_ip`（默认路由的局域网地址）—— 只在同 Wi-Fi 时可用。
///
/// 返回 `(主地址, 备选地址)`；备选会附在文案里（"打不开就换这个"）。纯函数，便于单测。
fn webui_address(state: &Value) -> Result<(String, Option<String>), String> {
    let port = state.get("port").and_then(|p| p.as_u64()).unwrap_or(0);
    let lan = state.get("lan_ip").and_then(|v| v.as_str()).unwrap_or("").trim();
    let ts = state.get("tailscale_ip").and_then(|v| v.as_str()).unwrap_or("").trim();
    let custom = state
        .get("webui_base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let from_ip = |ip: &str| -> Option<String> {
        if ip.is_empty() || ip == "127.0.0.1" || port == 0 || port > u16::MAX as u64 {
            return None;
        }
        Some(format!("https://{ip}:{port}"))
    };
    let custom_origin =
        (!custom.is_empty()).then(|| custom.trim_end_matches('/').to_string());
    let ts_origin = from_ip(ts);
    let lan_origin = from_ip(lan);

    let primary = custom_origin
        .clone()
        .or_else(|| ts_origin.clone())
        .or_else(|| lan_origin.clone())
        .ok_or_else(|| "webapi 未运行，或没取到可用地址".to_string())?;
    let alternate = [custom_origin, ts_origin, lan_origin]
        .into_iter()
        .flatten()
        .find(|o| *o != primary);
    Ok((primary, alternate))
}

/// 生成"在 WebUI 里打开"的提示（含**外部可达地址** + 备选）。
fn webui_hint(http: &Http, session_id: &str) -> Result<String, String> {
    let st = http.get("/api/session")?;
    let (origin, alternate) = webui_address(&st)?;
    webui_hint_text(&origin, alternate.as_deref(), session_id)
}

/// 只取 WebUI 基址（没有会话可指时用，例如"文件太大，去 WebUI 看"）。
fn webui_origin(http: &Http) -> Result<String, String> {
    let st = http.get("/api/session")?;
    webui_address(&st).map(|(origin, _)| origin)
}

/// 提示文案（纯函数，便于单测）。
///
/// 证书那句是 2026-10-16 实测后补的：点开链接**先撞自签名证书告警**，用户不知道要信任一次，
/// 只会以为链接坏了（当时还叠加了 release 下静态资源 404，见 docs/LARK_BOT.md §5.11）。
/// 链接本身换成**直达该条会话的深链**（同一实测日期）：以前给的是站点根地址，
/// 点开只到首页，"打开详情"名不副实。
///
/// 多网卡机器**不做"是否同一网络"的判断**（飞书不给客户端 IP，判断不了）：
/// 主地址按 [`webui_address`] 的优先级选，另一个可用地址作为"打不开就换这个"附在后面。
fn webui_hint_text(
    origin: &str,
    alternate: Option<&str>,
    session_id: &str,
) -> Result<String, String> {
    let origin = origin.trim().trim_end_matches('/');
    if origin.is_empty() {
        return Err("webapi 未运行，或没取到可用地址".to_string());
    }
    let mut text = format!(
        "在浏览器打开（自签名证书会先提示「不安全」，点「继续访问」信任一次即可）：\n{}\n",
        crate::card::webui_deep_link(origin, session_id)
    );
    if let Some(alt) = alternate.map(str::trim).filter(|a| !a.is_empty()) {
        let alt = alt.trim_end_matches('/');
        if alt != origin {
            text.push_str(&format!(
                "\n如果上面这个打不开，换这个地址试试：\n{}\n",
                crate::card::webui_deep_link(alt, session_id)
            ));
        }
    }
    text.push_str("\n这个链接直达该条会话，可以直接在浏览器里继续追问。");
    Ok(text)
}

/// 阻塞读 stderr，直到出现就绪标记 `[event] ready event_key=`。
///
/// 这是 lark-cli 给的**子进程契约**（§2 实测）：在它出现之前不要相信 stdout。
///
/// **日志策略**（2026-10-18 与菜单消费者统一）：只有 `[event]` / `[source]` 开头的诊断行
/// 实时落 info；其它（多为错误 JSON）落 debug 并累积成**一条摘要**放进失败信息 ——
/// 否则卡片消费者每次重连失败都会把 JSON 逐行刷进日志。
fn wait_ready(child: &mut Child) -> Result<(), String> {
    let stderr = child.stderr.take().ok_or("拿不到 lark-cli 的 stderr")?;
    let ready = Arc::new(AtomicBool::new(false));
    let ready_in_thread = Arc::clone(&ready);
    let diag = Arc::new(Mutex::new(String::new()));
    let diag_in_thread = Arc::clone(&diag);

    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.starts_with("[event]") || line.starts_with("[source]") {
                // 诊断日志原样转发（`--quiet` 会隐藏事件丢失诊断，故不启用）
                log::info!("[lark-cli] {line}");
                if line.contains("[event] ready event_key=") {
                    ready_in_thread.store(true, Ordering::Release);
                }
            } else {
                log::debug!("[lark-cli] {line}");
                let mut d = diag_in_thread.lock().unwrap_or_else(|e| e.into_inner());
                if d.len() < 4000 {
                    d.push_str(&line);
                    d.push('\n');
                }
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
    let raw = diag.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let summary = summarize_stderr(&raw);
    Err(if summary.is_empty() {
        format!("等待 {:?} 仍未收到就绪标记（长连接没建起来？）", READY_TIMEOUT)
    } else {
        format!("等待 {:?} 仍未收到就绪标记：{summary}", READY_TIMEOUT)
    })
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
        Self { agent: agent_for(&base), base, token }
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
                    .map_err(|e| format!("webapi 返回的不是 JSON：{e}（前 200 字符：{}）", card::clip_str(&text, 200)))
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

    /// 已绑定的私聊 `chat_id` 列表（`GET /api/lark/bindings`）—— 生命周期通知的收件人。
    ///
    /// 只在"应用还在跑"时可用：优雅退出路径上 webapi **可能已经停了**，
    /// 所以调用方要接受失败（退回 [`notify::cached_chats`]，见 `notify::collect_targets`）。
    fn bound_chats(&self) -> Result<Vec<String>, String> {
        let v = self.get("/api/lark/bindings")?;
        Ok(notify::parse_bound_chats(&v))
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

    /// 读飞书策略：`config.json` 的 `integrations.lark`，经 webapi **结构化**暴露
    /// （`GET /api/integrations/lark`）。不再自己剥 `"true"` / 解析 JSON 字符串。
    fn load_gate_config(&self) -> Result<BotConfig, String> {
        let cfg = self.get("/api/integrations/lark")?;
        let bool_of = |key: &str| cfg.get(key).and_then(Value::as_bool);
        let users: Vec<String> = cfg
            .get("allowed_users")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
            .unwrap_or_default();
        Ok(BotConfig {
            enabled: bool_of("enabled").unwrap_or(false),
            gate: LarkGateConfig {
                // 缺省即 true：没配置时也必须要求私聊。
                require_p2p: bool_of("require_p2p").unwrap_or(true),
                allowed_users: users,
                // **裁决后的**配对状态（服务端按 `pairing_expires_at` 算好再下发）——
                // bot 不碰时钟、也不复制那条 fail-closed 规则。缺省 false。
                pairing: bool_of("pairing_active").unwrap_or(false),
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

    /// 取该私聊的多会话列表（§11）。活动会话排第一。
    fn chat_sessions(&self, chat_id: &str) -> Result<Vec<SessionBrief>, String> {
        let v = self.get(&format!("/api/lark/chat-sessions?chat_id={chat_id}"))?;
        Ok(parse_session_briefs(&v))
    }

    /// **强制**新建一条会话并绑定（跳过"已存在就复用"的检查）。
    ///
    /// 用途：绑定指向的会话已被别的端删除时的兜底（见 `handle_line` 的 404 分支）——
    /// 此时旧绑定**必须**被替换，走 [`Self::session_for_chat`] 只会拿回同一条死会话。
    fn session_for_chat_fresh(&self, chat_id: &str, user_open_id: &str) -> Result<String, String> {
        let created = self.post("/api/lark/chat-sessions", &json!({
            "chat_id": chat_id,
            "user_open_id": user_open_id,
        }))?;
        created
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "新建会话未返回 session_id".to_string())
    }
}

/// 接受自签名证书的 HTTP agent。
///
/// webapi 用自签 TLS（`webapi/tls.rs`），**同机**调用必须跳过校验；`rustls` 的
/// `dangerous()` 是官方给的入口。唯一调用点是 [`agent_for`]，它保证只有回环地址
/// （或用户显式设了 `LS_WEBAPI_INSECURE=1`）才会走到这里 —— 非回环一律走真实
/// 证书校验，绝不让"跳过校验 + 携带 token"同时落在网络可达的地址上。
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

/// 从 `base` 里取出主机名（小写，去掉端口 / userinfo / IPv6 方括号）。
fn host_of(base: &str) -> String {
    let after = base.split("://").nth(1).unwrap_or(base);
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
            _ => authority,
        }
    };
    host.to_ascii_lowercase()
}

/// 目标是否在本机（回环）。**只有回环**才允许跳过 TLS 校验。
///
/// 用 IP 解析判定而不是字符串前缀：`127.0.0.1.evil.com` 这类主机名以 `127.` 开头
/// 却解析到外部地址，前缀匹配会把 token 送出去。
fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback() || v4.is_unspecified(),
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unspecified(),
        Err(_) => false,
    }
}

/// 选 HTTP agent：**只有目标是回环**时才接受自签证书。
///
/// 跳过证书校验 + 携带 Bearer token 的组合一旦指向非回环地址，就等于把 webapi
/// **唯一**的门槛交给任何能做中间人 / 劫持 DNS / 伪造局域网 ARP 的人。所以：
/// 回环 → 沿用自签可用的 `insecure_agent`（流量不出本机）；非回环 → 用默认 agent
/// 做**真实**的公共根校验（自签会握手失败，这是有意为之的 fail-closed）；
/// 确需"非回环 + 自签"的部署必须显式设 `LS_WEBAPI_INSECURE=1`（打 WARN，属知情的
/// 用户选择）。
fn agent_for(base: &str) -> ureq::Agent {
    let host = host_of(base);
    if is_loopback_host(&host) {
        return insecure_agent();
    }
    if std::env::var("LS_WEBAPI_INSECURE").is_ok_and(|v| v == "1") {
        log::warn!(
            "[bot] LS_WEBAPI_INSECURE=1：对非回环地址 {host} 跳过 TLS 校验 —— token 有被中间人截获的风险"
        );
        return insecure_agent();
    }
    log::warn!(
        "[bot] 目标 {host} 非回环，启用真实证书校验（自签证书会握手失败；确需自签请设 LS_WEBAPI_INSECURE=1）"
    );
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(600))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有回环才允许跳过 TLS 校验 —— 这条判定是"token 会不会被中间人拿走"的
    /// 唯一开关，必须逐例覆盖。
    #[test]
    fn host_parsing_and_loopback_detection() {
        assert_eq!(host_of("https://127.0.0.1:8443"), "127.0.0.1");
        assert_eq!(host_of("https://localhost:8443/api"), "localhost");
        assert_eq!(host_of("https://[::1]:8443"), "::1");
        assert_eq!(host_of("https://my-host.example.com:8443"), "my-host.example.com");
        assert_eq!(host_of("https://user:pw@example.com:8443"), "example.com");
        assert_eq!(host_of("http://100.64.0.7"), "100.64.0.7");
        // 大小写与无 scheme 的兜底
        assert_eq!(host_of("HTTPS://HOST.Example.COM:8443"), "host.example.com");

        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host(&host_of("https://127.0.0.2:9000")));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));

        // 非回环：绝不放行跳过校验（含"看着像回环"的欺骗性主机名）
        for host in ["100.64.0.7", "192.168.1.5", "127.0.0.1.evil.com", "0.0.0.1", "example.com"] {
            assert!(!is_loopback_host(host), "{host} 不应被当成回环");
        }
    }

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

    /// 「打开详情」的提示：必须给**外部可达地址** + **直达会话的深链**，
    /// 备选地址要出现，且必须提醒自签名证书（否则用户看到"不安全"就以为链接坏了）。
    #[test]
    fn webui_hint_text_gives_deep_link_alternate_and_cert_warning() {
        let text = webui_hint_text(
            "https://100.101.102.101:8443",
            Some("https://192.168.1.54:8443"),
            "sess-1",
        )
        .unwrap();
        assert!(
            text.contains("https://100.101.102.101:8443/#/chat?origin=lark&session=sess-1"),
            "主地址 + 直达会话的深链：{text}"
        );
        assert!(
            text.contains("https://192.168.1.54:8443/#/chat?origin=lark&session=sess-1"),
            "备选地址不能少（打不开时换它）：{text}"
        );
        assert!(text.contains("证书"), "自签名证书的提醒不能少：{text}");

        assert!(webui_hint_text("", None, "s").is_err(), "没地址 = 报错");
        assert!(webui_hint_text("   ", None, "s").is_err(), "空白地址同理");
        let same = webui_hint_text("https://a:1", Some("https://a:1"), "s").unwrap();
        assert!(!same.contains("换这个地址"), "备选与主地址相同就不必再给：{same}");
        let none = webui_hint_text("https://a:1", None, "s").unwrap();
        assert!(!none.contains("换这个地址"), "{none}");
    }

    /// 地址优先级：自定义基址 > Tailscale > 局域网；端口/回环无效时整体不可用。
    #[test]
    fn webui_address_prefers_custom_then_tailscale_then_lan() {
        let full = serde_json::json!({
            "port": 8443,
            "lan_ip": "192.168.1.54",
            "tailscale_ip": "100.101.102.101",
            "webui_base_url": "",
        });
        let (origin, alt) = webui_address(&full).unwrap();
        assert_eq!(origin, "https://100.101.102.101:8443", "Tailscale 优先于局域网");
        assert_eq!(alt.as_deref(), Some("https://192.168.1.54:8443"), "局域网作为备选");

        let custom = serde_json::json!({
            "port": 8443,
            "lan_ip": "192.168.1.54",
            "tailscale_ip": "100.101.102.101",
            "webui_base_url": "https://link.example.com/",
        });
        let (origin, alt) = webui_address(&custom).unwrap();
        assert_eq!(origin, "https://link.example.com", "手配基址最优先（去掉尾斜杠）");
        assert_eq!(alt.as_deref(), Some("https://100.101.102.101:8443"));

        let lan_only = serde_json::json!({
            "port": 8443,
            "lan_ip": "192.168.1.54",
            "tailscale_ip": null,
            "webui_base_url": "",
        });
        let (origin, alt) = webui_address(&lan_only).unwrap();
        assert_eq!(origin, "https://192.168.1.54:8443");
        assert!(alt.is_none(), "只有局域网时没有备选");

        for bad in [
            serde_json::json!({"port": 0, "lan_ip": "192.168.1.54"}),
            serde_json::json!({"port": 70000, "lan_ip": "192.168.1.54"}),
            serde_json::json!({"port": 8443, "lan_ip": "127.0.0.1"}),
            serde_json::json!({"port": 8443}),
        ] {
            assert!(webui_address(&bad).is_err(), "不可用状态必须报错：{bad}");
        }
    }

    // ── §11 多会话 / 搜索的纯函数 ──

    /// `/api/lark/chat-sessions` 响应解析：字段缺失、元素残缺都不能 panic。
    #[test]
    fn parse_session_briefs_tolerates_partial_rows() {
        let v = serde_json::json!({
            "sessions": [
                { "session_id": "s-1", "title": "合同审查", "is_active": true, "updated_at": 1000 },
                { "session_id": "s-2", "title": "汪均益案", "is_active": false, "updated_at": 900 },
                { "session_id": "s-3" },              // 无 title / is_active / updated_at
                { "title": "没有 id" },               // 缺 session_id → 丢弃
                "垃圾元素"
            ]
        });
        let briefs = parse_session_briefs(&v);
        assert_eq!(briefs.len(), 3);
        assert_eq!(briefs[0].id, "s-1");
        assert!(briefs[0].is_active);
        assert!(!briefs[1].is_active);
        assert_eq!(briefs[2].title, "(无标题)");
        assert_eq!(briefs[2].updated_at, 0);
        // 没有 sessions 键 → 空表（不是 Err）
        assert!(parse_session_briefs(&serde_json::json!({})).is_empty());
    }

    /// 切换后的"锚点"：会话名 + 上一问；新会话/无名字时如实说明。
    #[test]
    fn anchor_text_shows_title_and_last_question() {
        let session = serde_json::json!({
            "title": "合同审查",
            "messages": [
                { "role": "user", "content": "第一问" },
                { "role": "assistant", "content": "第一答" },
                { "role": "user", "content": "最后一问" },
            ]
        });
        let text = anchor_text(&session);
        assert!(text.contains("合同审查"), "必须带上会话名：{text}");
        assert!(text.contains("最后一问"), "必须给最后一条 user 消息：{text}");
        assert!(!text.contains("第一问"), "不能给更早的问题：{text}");

        // 占位标题（"新会话"）不显示；没有标题也不显示
        let untitled = serde_json::json!({
            "title": "新会话",
            "messages": [{ "role": "user", "content": "问一句" }]
        });
        let text = anchor_text(&untitled);
        assert!(!text.contains("新会话"), "占位标题不该出现：{text}");
        assert!(text.contains("问一句"), "{text}");
        assert!(!anchor_text(&serde_json::json!({})).contains("「"), "没有名字就没有引号");

        // 空会话：给"新会话"的说明（带名字时也要有）
        let empty_named = serde_json::json!({ "title": "发票整理", "messages": [] });
        let text = anchor_text(&empty_named);
        assert!(text.contains("新会话") && text.contains("发票整理"), "{text}");
        // 超长问题被截到 60 字以内 + 省略号
        let long = "字".repeat(100);
        let text = anchor_text(&serde_json::json!({
            "messages": [{ "role": "user", "content": long }]
        }));
        assert!(text.contains('…'), "超长要截断：{text}");
    }

    /// 会话名提取：空白与缺失都归 `None`（页眉/锚点据此省略名字）。
    #[test]
    fn session_title_of_filters_blank_titles() {
        let s = |j: &str| session_title_of(&serde_json::from_str::<Value>(j).unwrap());
        assert_eq!(s(r#"{"title":"合同审查"}"#).as_deref(), Some("合同审查"));
        assert_eq!(s(r#"{"title":"  合同审查  "}"#).as_deref(), Some("合同审查"), "两侧空白要清掉");
        assert_eq!(s(r#"{"title":"   "}"#), None);
        assert_eq!(s(r#"{"title":""}"#), None);
        assert_eq!(s("{}"), None);
    }

    /// 生成守卫：同私聊串行（第二条被拦）、跨私聊互不影响、drop 即释放、/stop 能查到准星。
    #[test]
    fn generating_guard_serialises_per_chat_and_releases_on_drop() {
        // 用独有的 chat id，避免与其他并行测试互相干扰（静态表是全局的）
        let chat = "oc_guard_test_a";
        let other = "oc_guard_test_b";
        assert!(with_generating(|g| g.get(chat).cloned()).is_none(), "初始应当为空");

        let g1 = GeneratingGuard::begin(chat, "s-1").expect("首次登记应成功");
        assert!(
            GeneratingGuard::begin(chat, "s-2").is_none(),
            "同一私聊的第二条生成必须被拦下（保持每私聊串行）"
        );
        assert!(
            GeneratingGuard::begin(other, "s-9").is_some(),
            "别的私聊不受影响"
        );
        // `/stop` 的准星：按 chat 能查到正在生成的**会话**
        assert_eq!(
            with_generating(|g| g.get(chat).cloned()).as_deref(),
            Some("s-1")
        );

        drop(g1);
        assert!(
            with_generating(|g| g.get(chat).cloned()).is_none(),
            "guard drop 必须摘除登记（否则该私聊会永久被拒）"
        );
        assert!(
            GeneratingGuard::begin(chat, "s-3").is_some(),
            "释放后应能再次开始生成"
        );
    }

    /// `/history` 的召回文本：角色标注、空内容跳过、顺序保持。
    #[test]
    fn history_text_labels_roles_and_skips_empty() {
        let messages = vec![
            json!({"role":"user","content":"第一问"}),
            json!({"role":"assistant","content":"第一答"}),
            json!({"role":"assistant","content":"   "}),
            json!({"role":"user","content":"第二问"}),
        ];
        let text = build_history_text(&messages);
        assert!(text.starts_with("**你：**第一问"), "{text}");
        assert!(text.contains("**助手：**第一答"), "{text}");
        assert!(text.contains("**你：**第二问"), "{text}");
        let q1 = text.find("第一问").unwrap();
        let a1 = text.find("第一答").unwrap();
        let q2 = text.find("第二问").unwrap();
        assert!(q1 < a1 && a1 < q2, "顺序必须保持：{text}");
        assert!(!text.contains("**助手：**\n"), "空内容不得留下孤零零的角色名");
    }

    /// `/current` 卡片：名字进页眉；无名时有解释；时间/计数都在正文里。
    #[test]
    fn current_card_shows_name_or_explains_placeholder() {
        let named = current_card(Some("合同审查"), 1_792_154_040, 12, 3);
        assert_eq!(
            named["header"]["title"]["content"], "当前会话：合同审查",
            "名字必须在页眉（最显眼处）"
        );
        assert_eq!(named["header"]["template"], "blue");
        let body = named["elements"][0]["content"].as_str().unwrap_or("");
        assert!(body.contains("消息 12 条"), "{body}");
        assert!(body.contains("共 3 个会话"), "{body}");
        assert!(body.contains("10-16 12:34 UTC"), "时间要展示：{body}");

        let unnamed = current_card(None, 0, 0, 1);
        assert_eq!(unnamed["header"]["title"]["content"], "当前会话");
        let body = unnamed["elements"][0]["content"].as_str().unwrap_or("");
        assert!(body.contains("还没起名"), "未命名要解释清楚：{body}");
    }

    /// 搜索响应解析 + 命中说明（路径与片段各一行，空白压成单个空格）。
    #[test]
    fn parse_hits_and_detail_flatten_whitespace() {
        let v = serde_json::json!({
            "total": 2,
            "hits": [
                { "file_id": "f-1", "file_name": "判决书.pdf", "path": "合同纠纷/2024/判决书.pdf",
                  "snippet": "……汪均益\n  案的最终结果……" },
                { "file_name": "没有片段的.docx", "path": "目录B/没有片段的.docx" },
                { "path": "缺文件名" }        // 缺 file_name → 丢弃
            ]
        });
        let hits = parse_hits(&v);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].file_id, "f-1");
        assert_eq!(hits[1].file_id, "", "缺 file_id 的命中照样可看，只是没有「发送」按钮");
        let detail = search_hit_detail(&hits[0]);
        assert!(detail.contains("合同纠纷/2024/判决书.pdf"), "{detail}");
        assert!(detail.contains("汪均益 案的最终结果"), "换行要压成空格：{detail}");
        assert!(!detail.contains('\n') || detail.contains("\n   "), "只允许片段前的固定缩进");
        assert_eq!(search_hit_detail(&hits[1]), "目录B/没有片段的.docx");
        assert!(parse_hits(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn url_encode_percent_encodes_non_unreserved() {
        assert_eq!(url_encode("abc-_.~123"), "abc-_.~123");
        assert_eq!(url_encode("汪均益 A&B"), "%E6%B1%AA%E5%9D%87%E7%9B%8A%20A%26B");
    }

    /// `MM-DD HH:MM UTC` 的格式化（纯算术，不依赖 chrono）。
    #[test]
    fn fmt_utc_short_formats_known_timestamps() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(fmt_utc_short(1_704_067_200), "01-01 00:00 UTC");
        // 1970-01-01 00:00:00 UTC
        assert_eq!(fmt_utc_short(0), "01-01 00:00 UTC");
        // 2026-10-16 12:34 UTC 附近（会话列表时间字段的实际量级）
        assert_eq!(fmt_utc_short(1_792_154_040), "10-16 12:34 UTC");
    }

    /// 卡片消费者重连退避：就绪过 → 5s 起（尽快恢复按钮）；从未就绪 → 30s 起；300s 封顶。
    #[test]
    fn card_consumer_backoff_prefers_fast_recovery_when_it_had_worked() {
        assert_eq!(consumer_backoff(true, 0), Duration::from_secs(5));
        assert_eq!(consumer_backoff(true, 1), Duration::from_secs(10));
        assert_eq!(consumer_backoff(false, 0), Duration::from_secs(30));
        assert_eq!(consumer_backoff(false, 1), Duration::from_secs(60));
        assert_eq!(consumer_backoff(false, 99), Duration::from_secs(300), "封顶且不溢出");
    }

    /// 菜单：未订阅是确定性失败 → 1 小时；其它失败 → 5 分钟。
    #[test]
    fn menu_backoff_distinguishes_permanent_from_transient() {
        assert_eq!(menu_backoff(true), Duration::from_secs(3600));
        assert_eq!(menu_backoff(false), Duration::from_secs(300));
    }

    /// "未订阅"识别 + stderr 压成一行（错误 JSON 取 `message` 字段）。
    #[test]
    fn menu_stderr_is_summarized_to_one_line() {
        let raw = "{\n  \"ok\": false,\n  \"error\": {\n    \
                   \"message\": \"EventKey application.bot.menu_v6 requires event types \
                   not subscribed in console: application.bot.menu_v6\",\n    \
                   \"type\": \"validation\"\n  }\n}";
        assert!(is_not_subscribed(raw), "含 not subscribed 应判为确定性失败");
        assert!(!is_not_subscribed("connection reset"), "普通网络错误不是");
        assert!(is_not_subscribed("NOT SUBSCRIBED"), "大小写不敏感");
        let s = summarize_stderr(raw);
        assert!(s.contains("requires event types not subscribed"), "摘要：{s}");
        assert!(!s.contains('\n'), "必须压成一行：{s}");
        assert_eq!(summarize_stderr("   "), "", "空白 → 空摘要");
        assert_eq!(
            summarize_stderr("line one\nline two"),
            "line one line two",
            "没有 message 字段时整段压平"
        );
    }
}
