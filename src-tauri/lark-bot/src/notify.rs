//! 生命周期通知（`docs/LARK_BOT.md` §十二）：**优雅退出**时发「正在下线」，
//! 下次**成功上线**时给同一批私聊补发「已重新上线」。
//!
//! ## 为什么是"自己发一条普通消息"
//!
//! 飞书**没有**"自定义应用是否在线"的接口，机器人在别人眼里永远是"在的" ——
//! 消息发过去只是没人回答（Hermes Agent 也是同样的做法：shutdown 前**自己广播一条**）。
//! 所以这一条通知不是状态，而是**一句人话**：告诉用户"别等了，等它回来再发"。
//!
//! ## 四条设计约束
//!
//! 1. **只发"有会话绑定的私聊"**：机器人是私聊助手，从没聊过的人不该收到通知。
//!    目标 = `GET /api/lark/bindings`（应用还在跑时装全量）∪ 本进程**见过消息**的私聊
//!    （缓存：应用退出时 webapi 可能已经不在，不能只依赖它）。
//! 2. **配对**：只有"确实发出过下线通知"的私聊才会收到上线通知 —— 待办清单落在
//!    `data_dir/lark_offline_pending.json`。所以平时启动、改设置重启**都是安静的**。
//! 3. **有界等待**：应用只给几秒（它在退出路径上等我们）—— 取数 2s、单条发送 5s 封顶、
//!    多私聊**并发**发送，发不出去就放弃，绝不拖住应用退出。
//! 4. **逃生阀** `LS_LARK_LIFECYCLE=off`：一键关掉两条通知（排查/偏好）。
//!
//! 应用侧怎么叫我们：**写它自己的 stdin**（`shutdown` = 发下线通知再退；`stop` = 静默退）。
//! 见 `src/lark_bot.rs`；这里只负责解析那一行与"怎么做"。

use std::collections::BTreeSet;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// 下线通知正文（用户已确认的措辞）。
const OFFLINE_TEXT: &str =
    "⚠️ Link-Searcher 正在下线 —— 正在进行的任务会中断。等它重新上线后，直接发消息即可继续。";
/// 上线通知正文（与下线通知**配对**出现）。
const ONLINE_TEXT: &str = "✅ Link-Searcher 已重新上线，直接发消息即可继续。";

/// 待办清单文件名（在 `data_dir` 下）：记下"收到过下线通知、还在等上线通知"的私聊。
pub const PENDING_FILE: &str = "lark_offline_pending.json";

/// 单条发送的等待上限（超时就放弃，不拖住应用退出）。
/// 发送要经 `lark-cli` → 飞书云端，慢网络下得留几秒余量。
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// 取"绑定列表"的总时限：**必须有**（应用在退出路径上只等几秒）。webapi 的 DB 连接池
/// 若被索引/提取占满，一次 `GET` 可能要几十秒 —— 那时宁可不发，也不能拖住应用退出。
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);
/// 收件人上限：防御性封顶。正常就一两个私聊，这个数只是"别因为脏数据去刷屏"。
const MAX_TARGETS: usize = 20;

/// 控制指令字面量（与 `src/lark_bot.rs` 的常量**必须一致**）。
const CMD_SHUTDOWN: &str = "shutdown";
const CMD_STOP: &str = "stop";

/// 逃生阀环境变量：置 `off` 时两条通知都不发。
const LIFECYCLE_ENV: &str = "LS_LARK_LIFECYCLE";

// ───────────────────────────── 控制指令 ─────────────────────────────

/// 应用经 stdin 发来的控制指令。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Control {
    /// 优雅停机：**先发下线通知**再退出（应用正在退出）。
    Shutdown,
    /// 静默停机：直接退出，一个字都不发（应用要重拉机器人：改设置 / 热重载）。
    Stop,
}

/// 解析一行控制指令（纯函数，便于单测）：首尾空白与 `\r` 都容忍，大小写不敏感；
/// **认不出的行一律忽略**（绝不因为多打了什么就擅自下线/退出）。
pub fn parse_control(line: &str) -> Option<Control> {
    let t = line.trim();
    if t.eq_ignore_ascii_case(CMD_SHUTDOWN) {
        Some(Control::Shutdown)
    } else if t.eq_ignore_ascii_case(CMD_STOP) {
        Some(Control::Stop)
    } else {
        None
    }
}

/// 开始听应用发来的控制指令（后台线程；stdin 一行一条）。
///
/// `fetch` 是"要一份全量绑定列表"的取数口（正常就是 `GET /api/lark/bindings`）——
/// 用闭包而不是 `&Http`，是为了让本模块不依赖 HTTP 类型、也能单测。
///
/// **stdin EOF 不做任何事**：应用硬杀/崩溃时管道也会关，但那条路要**保持安静**
/// （用户明确排除了"硬杀通知"）；而且"关 stdin"还是 lark-cli 侧的停机契约，语义不同。
pub fn spawn_control(fetch: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static) {
    std::thread::spawn(move || {
        let mut fetch = Some(fetch);
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            match parse_control(&line) {
                Some(Control::Shutdown) => {
                    log::info!("[bot] 收到 shutdown（应用正在退出）→ 发下线通知后退出");
                    let targets = match fetch.take() {
                        Some(f) => collect_targets(f),
                        // 第二次收到也不会走到这儿（第一次就 exit 了）；这里只是**不 panic**。
                        None => merge_targets(&[], &cached_chats()),
                    };
                    notify_offline(&targets);
                    std::process::exit(0);
                }
                Some(Control::Stop) => {
                    log::info!("[bot] 收到 stop（应用要重拉机器人）→ 静默退出");
                    std::process::exit(0);
                }
                None => {
                    if !line.trim().is_empty() {
                        log::debug!("[bot] 忽略控制行：{}", crate::card::clip_str(&line, 80));
                    }
                }
            }
        }
        log::debug!("[bot] 控制通道结束（stdin EOF）");
    });
}

// ───────────────────────────── 收件人 ─────────────────────────────

/// 本进程**见过消息**的私聊（`chat_id`）。缓存一份的原因见模块注释第 1 条：
/// 应用退出时 webapi 可能已经不在了。
static CHATS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// 记下一个"有真实会话"的私聊（准入判定通过时调用）。
pub fn remember_chat(chat_id: &str) {
    let id = chat_id.trim();
    if id.is_empty() {
        return;
    }
    CHATS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.to_string());
}

/// 批量记下（启动时预热用）。
pub fn remember_chats(ids: &[String]) {
    for id in ids {
        remember_chat(id);
    }
}

/// 当前缓存里的私聊（快照）。
pub fn cached_chats() -> Vec<String> {
    CHATS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

/// `GET /api/lark/bindings` 响应 → `chat_id` 列表（纯函数）。
///
/// 字段缺失/元素残缺都不 panic（与 `parse_session_briefs` 同姿态）：
/// 结构变了最多是"少通知一个私聊"，绝不能让退出路径出错。
pub fn parse_bound_chats(v: &Value) -> Vec<String> {
    v.get("bindings")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|b| b.get("chat_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 合并"问服务端要的"与"本进程见过的"收件人：去重、去空白、封顶（纯函数）。
///
/// 服务端的排前面（它是权威名单），缓存在后补漏。
fn merge_targets(fetched: &[String], cached: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in fetched.iter().chain(cached.iter()) {
        let id = id.trim();
        if id.is_empty() || out.iter().any(|x| x == id) {
            continue;
        }
        out.push(id.to_string());
        if out.len() >= MAX_TARGETS {
            break;
        }
    }
    out
}

/// 取一份"绑定列表"（**有界等待**）：`fetch` 被丢进独立线程并用 [`FETCH_TIMEOUT`] 兜住。
///
/// 为什么必须有界：这条路径的代价是"应用等不下去直接强杀我们"（一条通知都发不出），
/// 而 webapi 的一次 `GET` 在 DB 连接池被占满时可能要几十秒 —— 那时宁可退回缓存。
/// `None` ＝ 失败或超时（具体原因已记日志）。
fn fetch_bounded(
    fetch: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
) -> Option<Vec<String>> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<String>, String>>();
    std::thread::spawn(move || {
        let _ = tx.send(fetch());
    });
    match rx.recv_timeout(FETCH_TIMEOUT) {
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) => {
            log::warn!("[bot] 读取绑定列表失败（{e}）");
            None
        }
        Err(_) => {
            log::warn!(
                "[bot] 读取绑定列表超时（{}s）",
                FETCH_TIMEOUT.as_secs()
            );
            None
        }
    }
}

/// 收集收件人：优先用 `fetch`（全量绑定），失败/超时就退回本进程见过的私聊。
///
/// 这里**不再单独记日志**：取数失败/超时已由 [`fetch_bounded`] 说明；
/// 而"取到了但为空"（还没有任何绑定）是正常状态，不该报警。
pub fn collect_targets(
    fetch: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
) -> Vec<String> {
    let fetched = fetch_bounded(fetch).unwrap_or_default();
    merge_targets(&fetched, &cached_chats())
}

// ───────────────────────────── 待办清单 ─────────────────────────────

/// 通知的落点目录：应用注入的 `LS_DATA_DIR` 优先（**唯一可靠来源**：`config.json` 里
/// `data_dir` 可能是空串，真值由应用的 `load_config` 补）。
///
/// 手工拉起 bot（没有应用注入）时才走兜底：读 `config.json` 的 `data_dir`，
/// 再退回默认数据目录 —— 与 `web_api_token` 的复刻同姿态（本 crate 不依赖应用 lib）。
fn state_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("LS_DATA_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    let cfg_dir = match std::env::var("LS_CONFIG_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => dirs::config_dir()?.join(".link-searcher"),
    };
    let configured = std::fs::read_to_string(cfg_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("data_dir")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty());
    if let Some(d) = configured {
        return Some(PathBuf::from(d));
    }
    Some(dirs::data_local_dir()?.join("link-searcher"))
}

/// 待办清单的绝对路径（`None` ＝ 连数据目录都解析不出来）。
fn pending_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join(PENDING_FILE))
}

/// 解析待办清单（纯函数）：认不出/结构不对 → 空表（**绝不 panic**）。
pub fn parse_pending(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let items = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("chats").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    for item in items {
        if let Some(id) = item.as_str() {
            let id = id.trim();
            if !id.is_empty() && !out.iter().any(|x| x == id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// 读待办清单：文件不存在/内容坏了 → 空表（**不是错误**：没有待办才是常态）。
pub fn read_pending_file(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|raw| parse_pending(&raw))
        .unwrap_or_default()
}

/// 写待办清单（**原子写**：临时文件 + 改名，避免退出途中被打断留下半个 JSON）。
/// 空表等价于"清掉"。
pub fn write_pending_file(path: &Path, chats: &[String]) -> Result<(), String> {
    if chats.is_empty() {
        return clear_pending_file(path);
    }
    let body = serde_json::to_string(&json!({ "chats": chats })).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    // Windows 上 rename 也是**替换**语义（Rust 内部用 MOVEFILE_REPLACE_EXISTING）。
    std::fs::rename(&tmp, path).map_err(|e| format!("改名 {} 失败：{e}", path.display()))
}

/// 清空待办清单（文件本来就不在 → 成功）。
pub fn clear_pending_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删除 {} 失败：{e}", path.display())),
    }
}

// ───────────────────────────── 发送 ─────────────────────────────

/// 逃生阀是否关掉了通知（纯函数，便于单测）。
fn lifecycle_off(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some(v) if v.eq_ignore_ascii_case("off"))
}

/// 通知总开关：`LS_LARK_LIFECYCLE=off` 时两条都不发。
pub fn lifecycle_enabled() -> bool {
    !lifecycle_off(std::env::var(LIFECYCLE_ENV).ok().as_deref())
}

/// 发一条私聊消息的参数表（纯函数，便于单测 —— 参数名写错就是**静默发不出去**）。
///
/// 用 `+messages-send --chat-id`：**主动发**（不是回复某条消息）—— 通知时可能压根没有
/// "上一条消息"可回。`--as bot` 显式表态（默认也能解析到 bot，但别依赖默认值）。
pub fn send_argv(chat_id: &str, text: &str) -> Vec<String> {
    vec![
        "im".to_string(),
        "+messages-send".to_string(),
        "--chat-id".to_string(),
        chat_id.to_string(),
        "--as".to_string(),
        "bot".to_string(),
        "--text".to_string(),
        text.to_string(),
    ]
}

/// 发一条私聊消息（**有界等待**）。
///
/// 参数用数组逐个传（不经 shell），所以正文里的引号/空格/换行都安全。
/// stdin 给 `null`：lark-cli 的"关 stdin 即退出"契约是**事件消费者**的，
/// 一次性命令给它空 stdin 免得它等输入（更重要的是**别让它来读我们的控制通道**）。
///
/// stderr 保持管道：失败时要能把 lark-cli 的原话带进日志。等待期间不读它 ——
/// 这对**一次性命令**是安全的（错误输出只有几行，远不到管道缓冲上限 64KB）。
pub fn send_text(chat_id: &str, text: &str) -> Result<(), String> {
    let program = crate::lark_cli_program();
    let mut child = Command::new(&program)
        .args(send_argv(chat_id, text))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 {}：{e}", program.display()))?;

    let deadline = Instant::now() + SEND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("发送超时（{}s）", SEND_TIMEOUT.as_secs()));
            }
            Err(e) => return Err(format!("等待发送进程失败：{e}")),
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("读取发送结果失败：{e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "退出码 {:?}：{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 并发发给多个私聊，返回**成功**的那些（失败只记日志：一个失败不拖累其余）。
fn send_all(chats: &[String], text: &str) -> Vec<String> {
    let handles: Vec<_> = chats
        .iter()
        .map(|chat| {
            let chat = chat.clone();
            let text = text.to_string();
            std::thread::spawn(move || match send_text(&chat, &text) {
                Ok(()) => Some(chat),
                Err(e) => {
                    log::warn!("[bot] 通知发送失败（chat={chat}）：{e}");
                    None
                }
            })
        })
        .collect();

    let mut sent = Vec::new();
    for h in handles {
        if let Ok(Some(chat)) = h.join() {
            sent.push(chat);
        }
    }
    sent
}

/// 下线通知是否已经发过（一个进程只发一次）。
static OFFLINE_SENT: AtomicBool = AtomicBool::new(false);

/// 发下线通知 + 记下"谁该收到上线通知"。
pub fn notify_offline(chats: &[String]) {
    if !lifecycle_enabled() {
        log::info!("[bot] {LIFECYCLE_ENV}=off → 跳过下线通知");
        return;
    }
    if OFFLINE_SENT.swap(true, Ordering::AcqRel) {
        return;
    }
    if chats.is_empty() {
        log::warn!("[bot] 没有可通知的私聊（既无绑定、本进程也没见过消息）→ 跳过下线通知");
        return;
    }
    let sent = send_all(chats, OFFLINE_TEXT);
    log::info!(
        "[bot] 下线通知：成功 {}/{}（目标 {}）",
        sent.len(),
        chats.len(),
        chats.len()
    );
    if sent.is_empty() {
        return; // 一条都没发出去 → 没有"待恢复"的私聊，下次启动保持安静。
    }
    match pending_path() {
        Some(p) => {
            if let Err(e) = write_pending_file(&p, &sent) {
                log::warn!("[bot] 记录待恢复私聊失败（下次启动不会有上线通知）：{e}");
            }
        }
        None => log::warn!("[bot] 解析不出数据目录 → 无法记录待恢复私聊"),
    }
}

/// 启动后的收尾（**后台线程**，不能挡住事件消费）：预热私聊缓存 + 补发上线通知。
pub fn spawn_startup(fetch: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static) {
    std::thread::spawn(move || {
        // 预热：把"有绑定的私聊"先放进缓存，免得应用一退出就只能靠"见过消息"那条路。
        // 失败/超时只影响这份预热（一次提示都没有），不影响下面的上线通知。
        if let Some(chats) = fetch_bounded(fetch) {
            remember_chats(&chats);
        }

        let Some(path) = pending_path() else {
            return;
        };
        let pending = read_pending_file(&path);
        if pending.is_empty() {
            return;
        }
        if !lifecycle_enabled() {
            log::info!("[bot] {LIFECYCLE_ENV}=off → 不发上线通知（清掉待办）");
            if let Err(e) = clear_pending_file(&path) {
                log::warn!("[bot] 清待办失败：{e}");
            }
            return;
        }

        let sent = send_all(&pending, ONLINE_TEXT);
        log::info!(
            "[bot] 上线通知：成功 {}/{}（这 {} 个私聊收到过下线通知）",
            sent.len(),
            pending.len(),
            pending.len()
        );
        // 失败的**留着**：下次启动再补（全成功才清掉文件）。
        let failed: Vec<String> = pending
            .iter()
            .filter(|c| !sent.iter().any(|s| s == *c))
            .cloned()
            .collect();
        let result = if failed.is_empty() {
            clear_pending_file(&path)
        } else {
            write_pending_file(&path, &failed)
        };
        if let Err(e) = result {
            log::warn!("[bot] 更新待办清单失败：{e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// 本测试独占的临时文件名（AGENTS.md：**不要共用**固定名/共享目录 ——
    /// 并行跑时别人的文件会被卷进来）。
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_pending_path() -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join(format!(
                "lark_lifecycle_test_{}_{}_{}",
                std::process::id(),
                nanos,
                seq
            ))
            .join(PENDING_FILE)
    }

    /// 只有**认得出的**指令才动作：多打空格/大小写/CRLF 容忍，别的一律忽略。
    #[test]
    fn parse_control_accepts_only_known_commands() {
        assert_eq!(parse_control("shutdown"), Some(Control::Shutdown));
        assert_eq!(parse_control("  shutdown \r\n"), Some(Control::Shutdown));
        assert_eq!(parse_control("STOP"), Some(Control::Stop));
        assert_eq!(parse_control(" Stop "), Some(Control::Stop));
        // 认不出 → 忽略（绝不擅自退出/下线）
        assert_eq!(parse_control(""), None);
        assert_eq!(parse_control("   "), None);
        assert_eq!(parse_control("restart"), None);
        assert_eq!(parse_control("stop now"), None, "指令是整行，不做前缀匹配");
        assert_eq!(parse_control("shutdown; rm -rf /"), None);
    }

    /// 逃生阀：只有 `off`（大小写/空白不敏感）算关闭，其它值一律**开启**。
    #[test]
    fn lifecycle_flag_off_is_case_insensitive() {
        assert!(lifecycle_off(Some("off")));
        assert!(lifecycle_off(Some(" OFF ")));
        assert!(!lifecycle_off(Some("on")));
        assert!(!lifecycle_off(Some("")));
        assert!(!lifecycle_off(None), "默认开启");
    }

    /// 收件人合并：去重、去空白、服务端名单在前、封顶。
    #[test]
    fn merge_targets_dedups_trims_and_caps() {
        let fetched = vec!["oc_a".to_string(), "  ".to_string(), "oc_b".to_string()];
        let cached = vec!["oc_b".to_string(), "oc_c".to_string(), "".to_string()];
        assert_eq!(merge_targets(&fetched, &cached), vec!["oc_a", "oc_b", "oc_c"]);

        let many: Vec<String> = (0..MAX_TARGETS + 5).map(|i| format!("oc_{i}")).collect();
        let capped = merge_targets(&many, &[]);
        assert_eq!(capped.len(), MAX_TARGETS, "必须有上限（防脏数据刷屏）");
        assert_eq!(capped[0], "oc_0", "顺序保持");
    }

    /// 绑定列表解析：缺字段/垃圾元素不 panic；结构变了就少通知一个，不是报错。
    #[test]
    fn parse_bound_chats_tolerates_partial_rows() {
        let v = serde_json::json!({
            "bindings": [
                { "chat_id": "oc_1", "session_id": "s-1" },
                { "session_id": "s-2" },        // 缺 chat_id → 丢弃
                "垃圾元素",
                { "chat_id": "oc_3" }
            ]
        });
        assert_eq!(parse_bound_chats(&v), vec!["oc_1", "oc_3"]);
        assert!(parse_bound_chats(&serde_json::json!({})).is_empty());
        assert!(parse_bound_chats(&serde_json::json!({ "bindings": "oops" })).is_empty());
    }

    /// 取数口失败 → 退回"本进程见过的私聊"（应用退出时 webapi 可能已经不在了）。
    #[test]
    fn collect_targets_merges_fetch_and_falls_back_to_cache() {
        let mine = "oc_lifecycle_test_fetch".to_string();
        let seen = "oc_lifecycle_test_seen".to_string();
        remember_chat(&seen);

        let fetched = mine.clone();
        let got = collect_targets(move || Ok(vec![fetched]));
        assert!(got.contains(&mine));
        assert!(got.contains(&seen), "缓存里的私聊不能漏：{got:?}");

        let fallback = collect_targets(|| Err("webapi 已停".to_string()));
        assert!(fallback.contains(&seen), "取数失败时仍要有收件人：{fallback:?}");
        assert!(
            !fallback.contains(&mine),
            "没进过缓存的不会凭空出现：{fallback:?}"
        );
    }

    /// 待办清单：写读往返、坏文件当空表、去重去空白。
    #[test]
    fn pending_roundtrip_and_garbage_tolerance() {
        let path = temp_pending_path();
        let dir = path.parent().map(Path::to_path_buf);
        let chats = vec!["oc_1".to_string(), "oc_2".to_string()];

        assert!(read_pending_file(&path).is_empty(), "文件不存在 → 空表");
        write_pending_file(&path, &chats).expect("写待办");
        assert_eq!(read_pending_file(&path), chats, "写读往返必须一致");
        // 临时文件不该留下（原子写的收尾）
        assert!(!path.with_extension("json.tmp").exists());

        std::fs::write(&path, "{ 半个 JSON").expect("写入坏内容");
        assert!(read_pending_file(&path).is_empty(), "坏文件 → 空表（不 panic）");

        assert_eq!(
            parse_pending(r#"{"chats":["oc_a","oc_a","","  "," oc_b "]}"#),
            vec!["oc_a", "oc_b"]
        );
        assert!(parse_pending(r#"{"chats":"不是数组"}"#).is_empty());

        // 空表 = 清掉文件（不是留一个空 JSON）
        write_pending_file(&path, &[]).expect("清待办");
        assert!(!path.exists());
        clear_pending_file(&path).expect("重复清理也应成功");

        if let Some(dir) = dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 发送参数表：`lark-cli im +messages-send --chat-id … --as bot --text …`
    /// （参数名写错就是**静默发不出去**，所以把它钉住）。
    #[test]
    fn send_argv_pins_lark_cli_flags() {
        let argv = send_argv("oc_x", OFFLINE_TEXT);
        assert_eq!(
            argv,
            vec![
                "im",
                "+messages-send",
                "--chat-id",
                "oc_x",
                "--as",
                "bot",
                "--text",
                OFFLINE_TEXT
            ]
        );
    }
}
