//! 飞书接入的**前置**：自检（看得见 App ID / 身份是否真可用）+ 绑定应用。
//!
//! 为什么需要它（2026-10-16 的追问「新用户怎么绑定机器人？我看不到 appid」）：
//! 凭证**不在本应用的配置里**，而在 `lark-cli` 自己的配置里 ——
//! `%USERPROFILE%\.lark-cli\config.json` 的 `apps[0]`（`app_secret` 存 OS 凭据库，
//! 文件里只留 `{source:"keychain", id:…}` 指针）。设置页此前只有**策略层**
//! （开关 / 白名单 / 配对，见 [`crate::commands::lark`]），于是：
//!
//! 1. 用户**看不到 App ID** —— 它压根没被读过；
//! 2. 依赖中心的"就绪"只探可执行文件在不在（[`crate::deps::catalog::lark_cli_path`]），
//!    "装了但没绑定"照样显示 ✅；
//! 3. 集成页"未运行"的提示是「详见日志」—— 用户知道去读日志，却不知道**缺哪一步**。
//!
//! 本模块补上两件事：
//! - [`self_check`]：**只读**诊断。跑 `lark-cli doctor`（会联网**真的校验** bot 身份与
//!   两个端点）+ `event consume card.action.trigger --dry-run`（卡片回调是否已订阅），
//!   把结果如实摊开，并给出**下一条该做的事**（[`first_missing`]）。
//! - **绑定会话**（[`bind_start`] / [`bind_status`] / [`bind_cancel`]）：代跑
//!   `lark-cli config init`（`--new` 浏览器流程 / 已有应用的表单），把原始输出、
//!   验证链接、退出码如实回显。
//!
//! ⚠️ 四条刻意的边界（别"顺手优化"掉）：
//!
//! 1. **不搬凭证**：`app_secret` 只经 stdin 交给 lark-cli（`--app-secret-stdin`，由它写进
//!    OS 凭据库）—— 本应用**不落盘、不打日志、不进 argv**（argv 会出现在进程列表里）。
//!    为什么不干脆自己存：把 secret 搬进明文 `config.json` 是**安全降级**，还要复刻
//!    token 刷新 / 事件订阅 / scope 申请，等于重写 lark-cli 的授权栈。
//! 2. **`auth status` 判不了死活**（2026-10-16 隔离 HOME 实测）：给一个**根本不存在的**
//!    appId，`lark-cli auth status` 依然报 `identities.bot.status = "ready"` 且 exit 0 ——
//!    它只做**本地结构**检查。真正**联网校验**的是 `lark-cli doctor`（同一个假 appId 下
//!    `bot_identity = warn (verify failed: The specified app does not exist.)` +
//!    `identity_ready = fail` + exit 1）。所以"到底能不能用"一律以 `doctor` 为准。
//! 3. **`config init` 先写盘再校验**（同一轮实测）：App ID/Secret 填错时它**已经把新值写进
//!    配置**（先 `OK: Configuration saved to …`，再报 `invalid_client`、exit 3）——
//!    于是"手滑填错"会**冲掉原本能用的绑定**。所以：改动前**快照**，判定失败即**回滚**。
//! 4. **子进程一律带超时**：`doctor` / `--dry-run` 都要联网，卡住不能把设置页拖死。

use std::io::{BufRead, Read, Write};
use std::process::{Child, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// `lark-cli doctor` 超时：它会联网校验 bot 身份 + 两个端点（真机实测 1.7s）。
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(20);
/// `event consume … --dry-run` 超时：联网查订阅与 scope。
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(20);
/// 绑定会话保留的输出行数上限（够看错误，又不会把整个日志塞进界面）。
const BIND_LOG_CAP: usize = 200;
/// 单行截断长度（lark-cli 偶有超长行，别让它撑爆 payload）。
const BIND_LINE_CAP: usize = 500;
/// 读取子进程输出的大小上限（防某个子进程无限输出把内存吃掉）。
const READ_CAP: u64 = 256 * 1024;

// ───────────────────────────── 自检（只读诊断） ─────────────────────────────

/// `lark-cli doctor` 的一条检查项（**原样**下发，不做"看起来不错"的加工）。
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LarkCheck {
    pub name: String,
    /// `pass` / `warn` / `fail`（lark-cli 的原话）
    pub status: String,
    pub message: String,
}

/// 接入自检结果（前端按 `missing` 的键做 i18n；状态与消息用 lark-cli 原文）。
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LarkSelfCheck {
    /// `lark-cli` 可执行文件路径（`None` = 没装）。
    pub cli_path: Option<String>,
    /// lark-cli 版本（`doctor` 的 `cli_version` 检查项）。
    pub cli_version: Option<String>,
    /// 已绑定应用的 App ID（用户"看不到"的就是它）。
    pub app_id: Option<String>,
    /// `feishu` / `lark`。
    pub brand: Option<String>,
    /// `doctor` 的总体判定（它会**联网**校验，所以这是"真能用吗"的答案）。
    pub healthy: bool,
    /// `bot_identity` 检查项的原始状态/说明 —— bot 收发消息靠它。
    pub bot_status: String,
    pub bot_message: String,
    /// `user_identity` 检查项（**bot 收发消息不需要 user 身份**）。
    pub user_status: String,
    pub user_message: String,
    /// 卡片回调订阅：`ready` / `blocked` / `unknown`。
    pub card_callback: String,
    pub card_callback_detail: String,
    /// `doctor` 的全部检查项（逐个展示，用户能自己看到是哪一条红了）。
    pub checks: Vec<LarkCheck>,
    /// **下一条该做的事**：`install_cli` / `bind_app` / `rebind_app` / `card_callback`；
    /// 空 = 前置齐了（剩下的"启用"与"配对"就在设置页上）。
    pub missing: Vec<String>,
    /// 自检本身失败时的说明（命令缺失 / 超时 / 输出无法解析）。
    pub error: Option<String>,
}

/// 跑一次自检（**每次都会起子进程**：`doctor` 约 2s，含联网 —— 别放进轮询）。
pub fn self_check() -> LarkSelfCheck {
    let Some(cli) = crate::deps::catalog::lark_cli_path() else {
        return LarkSelfCheck {
            cli_path: None,
            card_callback: "unknown".into(),
            missing: vec!["install_cli".into()],
            ..Default::default()
        };
    };

    let mut out = LarkSelfCheck {
        cli_path: Some(cli.display().to_string()),
        card_callback: "unknown".into(),
        ..Default::default()
    };

    let report = read_doctor();
    out.cli_version = report.version.clone();
    out.app_id = report.app_id.clone();
    out.brand = report.brand.clone();
    out.healthy = report.healthy;
    out.bot_status = report.bot_status.clone();
    out.bot_message = report.bot_message.clone();
    out.user_status = report.user_status.clone();
    out.user_message = report.user_message.clone();
    out.checks = report.checks.clone();
    out.error = report.raw_error.clone();

    // 卡片回调只在**已绑定**时才值得问：没绑定时它必然失败，白等一次联网。
    if out.app_id.is_some() {
        match run_cli(
            &["event", "consume", "card.action.trigger", "--as", "bot", "--dry-run"],
            None,
            DRY_RUN_TIMEOUT,
        ) {
            Ok(o) => {
                let c = parse_card_dry_run(&o.combined);
                out.card_callback = c.status;
                out.card_callback_detail = c.detail;
            }
            Err(e) => out.card_callback_detail = e,
        }
    }

    out.missing = first_missing(&out);
    out
}

/// **下一条该做的事**（有序，取第一个阻塞项）。
///
/// 为什么只报第一条：这是"下一步"提示，不是清单 —— 前面那条没做完，后面的本来也做不了
/// （没装 CLI 就无所谓绑没绑）。完整状态由 [`LarkSelfCheck`] 的其它字段 + 界面上的
/// `doctor` 检查项列表摊开，所以不会丢信息。
fn first_missing(c: &LarkSelfCheck) -> Vec<String> {
    if c.cli_path.is_none() {
        return vec!["install_cli".into()];
    }
    if c.app_id.is_none() {
        return vec!["bind_app".into()];
    }
    // 绑了但**联网校验不过**（凭据失效 / 应用被删 / 没开机器人能力）——
    // 这正是"界面显示就绪、机器人却不动"的那一类，必须单独说清楚。
    if c.bot_status != "pass" {
        return vec!["rebind_app".into()];
    }
    if c.card_callback != "ready" {
        return vec!["card_callback".into()];
    }
    Vec::new()
}

/// `lark-cli doctor` 的解析结果（纯数据，便于单测）。
#[derive(Debug, Default, PartialEq, Eq)]
struct DoctorReport {
    version: Option<String>,
    app_id: Option<String>,
    brand: Option<String>,
    healthy: bool,
    bot_status: String,
    bot_message: String,
    user_status: String,
    user_message: String,
    checks: Vec<LarkCheck>,
    /// 输出无法解析 / 命令失败时的说明。
    raw_error: Option<String>,
}

/// 跑 `doctor` 并解析（自检与绑定收尾共用 **同一份**"能不能用"判据）。
fn read_doctor() -> DoctorReport {
    match run_cli(&["doctor"], None, DOCTOR_TIMEOUT) {
        Ok(o) => {
            let mut d = parse_doctor(&o.combined);
            if o.timed_out {
                d.raw_error = Some("lark-cli doctor 超时未响应（已放弃等待）".into());
                d.healthy = false;
            }
            d
        }
        Err(e) => DoctorReport {
            raw_error: Some(e),
            ..Default::default()
        },
    }
}

/// **"这个绑定到底能不能用"的唯一判据**：`app_resolved` 有 appId **且** `bot_identity` 通过。
///
/// 为什么必须是 `bot_identity == pass` 而不是 `identity_ready == pass`：
/// 后者只要求"bot 或 user 有一个可用"，而我们的 bot **必须有 bot 身份**
/// （它要用 bot 身份收事件、发回复）；只有 user 身份可用时 `identity_ready` 照样 pass，
/// 但机器人根本动不了 —— 那正是要避免的"绿勾但不动"。
fn binding_is_usable(d: &DoctorReport) -> bool {
    d.app_id.is_some() && d.bot_status == "pass"
}

/// 解析 `lark-cli doctor` 的 JSON。
fn parse_doctor(raw: &str) -> DoctorReport {
    let mut out = DoctorReport::default();
    let Some(v) = first_json_object(raw) else {
        out.raw_error = Some(format!(
            "无法解析 lark-cli doctor 的输出：{}",
            truncate_line(raw.trim())
        ));
        return out;
    };
    if let Some(checks) = v.get("checks").and_then(|c| c.as_array()) {
        for c in checks {
            let name = c.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let status = c
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let message = c.get("message").and_then(|s| s.as_str()).unwrap_or("").to_string();
            match name.as_str() {
                "cli_version" => out.version = Some(message.clone()),
                "app_resolved" => {
                    let (id, brand) = parse_app_resolved(&message);
                    out.app_id = id;
                    out.brand = brand;
                }
                "bot_identity" => {
                    out.bot_status = status.clone();
                    out.bot_message = message.clone();
                }
                "user_identity" => {
                    out.user_status = status.clone();
                    out.user_message = message.clone();
                }
                _ => {}
            }
            out.checks.push(LarkCheck { name, status, message });
        }
    }
    let ok = v.get("ok").and_then(|b| b.as_bool());
    out.healthy = match ok {
        Some(b) => b,
        // 老/新版本没给 `ok` 时的兜底：没有任何一条 fail 才算健康；**空列表不算健康**
        // （解析不到东西就说"健康"是最危险的撒谎方式）。
        None => !out.checks.is_empty() && !out.checks.iter().any(|c| c.status == "fail"),
    };
    if out.checks.is_empty() && ok.is_none() {
        out.raw_error = Some("lark-cli doctor 的输出里没有检查项".into());
    }
    out
}

/// 从 `app_resolved` 的人类可读消息里取 appId / brand：`"app: cli_xxx (feishu)"`。
///
/// 为什么敢解析这句文本：`doctor` **没有** `--json`（实测 `--json` 直接报
/// `unknown flag`），而 `app_resolved` 是唯一带 appId 的检查项。这里要求 appId **以
/// `cli` 开头**才算数 —— 认不出来就当 `None`（"看不到 App ID"是可接受的降级，
/// 把一句话整段当成 appId 才是污染）。
fn parse_app_resolved(msg: &str) -> (Option<String>, Option<String>) {
    let rest = msg
        .split_once("app:")
        .map(|(_, r)| r.trim())
        .unwrap_or_else(|| msg.trim());
    let (id_part, brand) = match rest.split_once('(') {
        Some((id, tail)) => (id.trim(), tail.trim_end_matches(')').trim().to_string()),
        None => (rest, String::new()),
    };
    if !id_part.starts_with("cli") {
        return (None, None);
    }
    let brand = if brand.is_empty() { None } else { Some(brand) };
    (Some(id_part.to_string()), brand)
}

/// 卡片回调订阅的解析结果。
#[derive(Debug, PartialEq, Eq)]
struct CardCheck {
    /// `ready` / `blocked` / `unknown`
    status: String,
    detail: String,
}

/// 解析 `event consume card.action.trigger --dry-run` 的 JSON。
///
/// - `ok: true` + `decision.status == "ready"` → 已订阅
/// - `ok: true` + 其它 status → 未订阅（`blocked`），并把**没通过的先决条件**原文带上
/// - `ok: false` → `unknown`：命令自己失败了（可能是没绑定、也可能是网络抖动），
///   这时既不能说"订阅好了"、也不该断言"被拦了" —— 把原文留给用户看
fn parse_card_dry_run(raw: &str) -> CardCheck {
    let unknown = |detail: String| CardCheck {
        status: "unknown".into(),
        detail,
    };
    let Some(v) = first_json_object(raw) else {
        return unknown(format!("无法解析输出：{}", truncate_line(raw.trim())));
    };
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let msg = error_message(&v).unwrap_or_else(|| truncate_line(raw.trim()));
        return unknown(msg);
    }
    let decision = v.get("data").and_then(|d| d.get("decision"));
    let status = decision
        .and_then(|d| d.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
    if status == "ready" {
        return CardCheck {
            status: "ready".into(),
            detail: String::new(),
        };
    }
    let mut parts = vec![status.to_string()];
    if let Some(pre) = decision.and_then(|d| d.get("preconditions")).and_then(|p| p.as_array()) {
        for p in pre {
            let name = p.get("name").and_then(|s| s.as_str()).unwrap_or("");
            let st = p.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if st != "ok" {
                parts.push(format!("{name}={st}"));
            }
        }
    }
    CardCheck {
        status: "blocked".into(),
        detail: parts.join(" "),
    }
}

/// `{"ok":false,"error":{…}}` 里的错误说明（lark-cli 的握手 JSON 形状）。
fn error_message(v: &serde_json::Value) -> Option<String> {
    let err = v.get("error")?;
    let message = err.get("message").and_then(|s| s.as_str()).unwrap_or("");
    let subtype = err.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
    match (subtype.is_empty(), message.is_empty()) {
        (true, true) => None,
        (true, false) => Some(message.to_string()),
        (false, true) => Some(subtype.to_string()),
        (false, false) => Some(format!("{subtype}: {message}")),
    }
}

/// 取字符串里**第一个完整的 JSON 对象**（lark-cli 的握手 JSON 走 stderr，
/// 且前后可能混有别的输出）。
fn first_json_object(raw: &str) -> Option<serde_json::Value> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

/// 单行截断（避免超长行撑爆界面 payload）。
fn truncate_line(s: &str) -> String {
    if s.chars().count() <= BIND_LINE_CAP {
        return s.to_string();
    }
    let mut out: String = s.chars().take(BIND_LINE_CAP).collect();
    out.push('…');
    out
}

// ───────────────────────────── 绑定会话 ─────────────────────────────

/// 绑定请求（`new` / `existing` 二选一）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BindRequest {
    /// `new` = 让 lark-cli 建一个新应用（浏览器流程）；`existing` = 绑定已有 App ID/Secret。
    pub mode: String,
    /// 仅 `existing` 用。
    #[serde(default)]
    pub app_id: Option<String>,
    /// 仅 `existing` 用；**只经 stdin 传给 lark-cli**，本应用不落盘、不打日志。
    #[serde(default)]
    pub app_secret: Option<String>,
    /// `feishu`（默认）/ `lark`。
    #[serde(default)]
    pub brand: Option<String>,
    /// 仅 `new` 用：lark-cli 在 Agent 工作区（`OPENCLAW_HOME` / `HERMES_HOME`）里会**拒绝**
    /// 新建应用，用户显式确认后加 `--force-init` 重试。
    #[serde(default)]
    pub force_init: bool,
}

/// 绑定会话的状态快照（前端在会话期间每 1s 拉一次）。
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LarkBindStatus {
    /// `new` / `existing`；**空串 = 没有会话**。
    pub mode: String,
    /// 子进程还活着。
    pub running: bool,
    /// 收尾也做完了（成功与否、回滚与否都已知）。前端在 `running || !finalized` 时轮询。
    pub finalized: bool,
    /// 从输出里捞到的验证链接（`--new` 用：给用户点开去浏览器完成）。
    pub urls: Vec<String>,
    /// 原始输出（stdout + stderr，尾部 `BIND_LOG_CAP` 行）。
    pub log: Vec<String>,
    pub exit_code: Option<i32>,
    /// `exit 0` **且** `doctor` 确认现在这个绑定真的能用。
    pub success: bool,
    /// 收尾时 `doctor` 看到的 appId（成功时＝新绑定的那个）。
    pub app_id_after: Option<String>,
    /// 检测到 lark-cli 的"Agent 工作区拒绝新建"分支 → 界面给 `--force-init` 重试按钮。
    pub needs_force_init: bool,
    pub error: Option<String>,
}

struct BindSession {
    child: Option<Child>,
    mode: String,
    running: bool,
    /// 已经有人在收尾（收尾只跑一次）。
    finalizing: bool,
    finalized: bool,
    lines: Vec<String>,
    urls: Vec<String>,
    exit_code: Option<i32>,
    success: bool,
    app_id_after: Option<String>,
    /// 改动前的 appId（只在"取消"分支用：判断是不是**新**绑了一个能用的应用）。
    prev_app_id: Option<String>,
    /// 改动前 `~/.lark-cli/config.json` 的内容（`None` = 当时没有这个文件）。
    snapshot: Option<String>,
    needs_force_init: bool,
    error: Option<String>,
}

impl BindSession {
    fn status(&self) -> LarkBindStatus {
        LarkBindStatus {
            mode: self.mode.clone(),
            running: self.running,
            finalized: self.finalized,
            urls: self.urls.clone(),
            log: self.lines.clone(),
            exit_code: self.exit_code,
            success: self.success,
            app_id_after: self.app_id_after.clone(),
            needs_force_init: self.needs_force_init,
            error: self.error.clone(),
        }
    }

    fn push_line(&mut self, line: &str) {
        let line = truncate_line(line);
        if detect_agent_refusal(&line) {
            self.needs_force_init = true;
        }
        for u in extract_urls(&line) {
            if !self.urls.iter().any(|x| x == &u) {
                self.urls.push(u);
            }
        }
        self.lines.push(line);
        if self.lines.len() > BIND_LOG_CAP {
            let drop_n = self.lines.len() - BIND_LOG_CAP;
            self.lines.drain(..drop_n);
        }
    }
}

static BIND: OnceLock<Mutex<Option<BindSession>>> = OnceLock::new();

fn lock_bind() -> MutexGuard<'static, Option<BindSession>> {
    BIND.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 当前绑定会话（没有会话时各字段为空）。
pub fn bind_status() -> LarkBindStatus {
    match lock_bind().as_ref() {
        Some(s) => s.status(),
        None => LarkBindStatus::default(),
    }
}

/// 开始一次绑定（`config init`）。**不阻塞**：起子进程后台跑，进度靠 [`bind_status`] 拉。
pub fn bind_start(req: &BindRequest) -> Result<(), String> {
    let cli = crate::deps::catalog::lark_cli_path()
        .ok_or_else(|| "未发现 lark-cli：请先在「依赖中心」安装飞书 lark-cli".to_string())?;
    let mode = req.mode.trim().to_ascii_lowercase();
    let brand = req
        .brand
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("feishu");
    if brand != "feishu" && brand != "lark" {
        return Err("brand 只能是 feishu 或 lark".into());
    }

    let mut args: Vec<String> = vec!["config".into(), "init".into()];
    let mut stdin_secret: Option<String> = None;
    match mode.as_str() {
        "new" => {
            args.push("--new".into());
            if req.force_init {
                args.push("--force-init".into());
            }
        }
        "existing" => {
            let app_id = req.app_id.as_deref().unwrap_or("").trim();
            let secret = req.app_secret.as_deref().unwrap_or("").trim();
            if app_id.is_empty() {
                return Err("App ID 不能为空".into());
            }
            if app_id.chars().any(char::is_whitespace) {
                return Err("App ID 不能含空白字符".into());
            }
            if secret.is_empty() {
                return Err("App Secret 不能为空".into());
            }
            args.push("--app-id".into());
            args.push(app_id.to_string());
            // ⚠️ secret **只走 stdin**（`--app-secret-stdin`）：不进 argv，就不会出现在
            // 进程列表 / 日志里。下面 join 出来的"已启动"那行因此是安全的。
            args.push("--app-secret-stdin".into());
            args.push("--brand".into());
            args.push(brand.to_string());
            stdin_secret = Some(secret.to_string());
        }
        other => return Err(format!("未知的绑定模式：{other}（应为 new 或 existing）")),
    }

    // 检查 + 快照 + 起进程 + 落状态都在锁内：避免两次点击并发起两个 init
    // （两个 init 会互相覆盖，而回滚快照只有一份）。
    let mut g = lock_bind();
    if let Some(s) = g.as_ref()
        && (s.running || s.finalizing)
    {
        return Err("已有绑定流程正在进行中".into());
    }

    let snapshot = read_config_file();
    let prev_app_id = snapshot.as_deref().and_then(app_id_from_config);

    let mut cmd = crate::process::new(&cli);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin_secret.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|e| format!("启动 lark-cli 失败：{e}"))?;
    if let Some(secret) = &stdin_secret
        && let Some(mut si) = child.stdin.take()
    {
        let _ = si.write_all(secret.as_bytes());
        let _ = si.write_all(b"\n");
        // 必须显式关闭：lark-cli 把 stdin 读到 EOF 才往下走。
        drop(si);
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut session = BindSession {
        child: Some(child),
        mode: mode.clone(),
        running: true,
        finalizing: false,
        finalized: false,
        lines: vec![format!("[link-searcher] 已启动 lark-cli {}", args.join(" "))],
        urls: Vec::new(),
        exit_code: None,
        success: false,
        app_id_after: None,
        prev_app_id,
        snapshot,
        needs_force_init: false,
        error: None,
    };
    if mode == "existing" {
        session.push_line("[link-searcher] App Secret 只经 stdin 传递（不进进程列表、不落盘）");
    }
    *g = Some(session);
    drop(g);

    if let Some(o) = stdout {
        spawn_line_reader(o);
    }
    if let Some(e) = stderr {
        spawn_line_reader(e);
    }
    log::info!("[LARK-SETUP] 开始绑定飞书应用（mode={mode}）");
    spawn_watcher();
    Ok(())
}

/// 取消绑定：杀掉子进程，然后走**同一套收尾**（取消也可能已经把配置写脏了）。
/// 返回是否真的取消了一个在跑的会话。
pub fn bind_cancel() -> bool {
    {
        let mut g = lock_bind();
        let Some(s) = g.as_mut() else { return false };
        if !s.running || s.finalizing {
            return false;
        }
        if let Some(mut child) = s.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        s.running = false;
        s.push_line("[link-searcher] 收到取消请求，已终止 lark-cli");
    }
    std::thread::spawn(|| finalize(None, true));
    true
}

/// 收起**已结束**的会话（让面板回到"未开始"）。运行中不收。
pub fn bind_dismiss() {
    let mut g = lock_bind();
    if let Some(s) = g.as_ref()
        && (s.running || s.finalizing)
    {
        return;
    }
    *g = None;
}

/// 按行读子进程输出（stdout / stderr 各一个线程），逐行落进会话状态。
fn spawn_line_reader<R: Read + Send + 'static>(r: R) {
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            let mut g = lock_bind();
            let Some(s) = g.as_mut() else { break };
            s.push_line(trimmed);
        }
    });
}

/// 看住子进程退出（轮询 `try_wait`，200ms 一次 —— 只做等待，不持锁）。
fn spawn_watcher() {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let code = {
                let mut g = lock_bind();
                let Some(s) = g.as_mut() else { return };
                // 已被取消（收尾中）→ 让取消那条路径收尾，这里退出。
                if s.finalizing || !s.running {
                    return;
                }
                let Some(child) = s.child.as_mut() else { return };
                match child.try_wait() {
                    Ok(Some(st)) => {
                        s.exit_code = st.code();
                        s.running = false;
                        s.child = None; // try_wait 已经回收，这里只是放下句柄
                        Some(st.code())
                    }
                    Ok(None) => None,
                    Err(e) => {
                        s.error = Some(format!("等待 lark-cli 失败：{e}"));
                        s.push_line(&format!("[link-searcher] 等待子进程失败：{e}"));
                        s.running = false;
                        if let Some(mut c) = s.child.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        s.exit_code = None;
                        Some(None)
                    }
                }
            };
            if let Some(code) = code {
                finalize(code, false);
                return;
            }
        }
    });
}

/// **收尾**（只跑一次）：判定成功、必要时回滚、成功则重启 bot。
///
/// 为什么要把慢操作放在锁外：`doctor` 要联网（秒级），持锁会让设置页的轮询一起卡住。
/// `finalizing` 标志保证 watcher 与"取消"两条路径只有一条真正跑这里。
fn finalize(exit_code: Option<i32>, cancelled: bool) {
    let (snapshot, prev_app_id) = {
        let mut g = lock_bind();
        let Some(s) = g.as_mut() else { return };
        if s.finalizing {
            return;
        }
        s.finalizing = true;
        if !cancelled {
            s.exit_code = exit_code;
        }
        (s.snapshot.take(), s.prev_app_id.clone())
    };

    let report = read_doctor();
    let usable_now = binding_is_usable(&report);
    let bound_a_new_usable_app = usable_now && report.app_id != prev_app_id;
    let success = if cancelled {
        // 取消：**只有**"确实新绑上了一个能用的应用"才算成功。
        // （用户可能在浏览器里已经点完、我们晚了一步才杀 —— 此时不该把好绑定回滚掉。）
        bound_a_new_usable_app
    } else {
        exit_code == Some(0) && usable_now
    };

    let mut notes: Vec<String> = Vec::new();
    if cancelled {
        notes.push("已取消".to_string());
    }
    if !success {
        // 关键安全网：`config init` 是**先写盘再校验**的，填错就可能把原本能用的绑定冲掉。
        match rollback_config(snapshot.as_deref()) {
            Ok(()) => notes.push("未生效：已把 lark-cli 配置回滚到改动前".to_string()),
            Err(e) => notes.push(format!(
                "回滚失败：{e} —— 请手工检查 ~/.lark-cli/config.json"
            )),
        }
    }

    {
        let mut g = lock_bind();
        if let Some(s) = g.as_mut() {
            s.success = success;
            s.app_id_after = report.app_id.clone();
            for n in &notes {
                s.push_line(&format!("[link-searcher] {n}"));
            }
            s.finalized = true;
        }
    }

    if success {
        log::info!("[LARK-SETUP] 应用绑定成功：app_id={:?}", report.app_id);
        // 绑上了但 bot 还在用旧凭据（或压根没起来）→ 重启一次让新绑定立刻生效。
        crate::lark_bot::restart();
    } else {
        log::warn!(
            "[LARK-SETUP] 绑定未成功（exit={exit_code:?} cancelled={cancelled} usable={usable_now}）"
        );
    }
}

/// `lark-cli` 自己的配置路径：`<home>/.lark-cli/config.json`。
///
/// Windows 用 `USERPROFILE`（Go 的 `os.UserHomeDir()` 就是这么解析的），其它平台用 `HOME`。
fn lark_cli_config_path() -> Option<std::path::PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME")
    }?;
    Some(std::path::PathBuf::from(home).join(".lark-cli").join("config.json"))
}

fn read_config_file() -> Option<String> {
    std::fs::read_to_string(lark_cli_config_path()?).ok()
}

/// 回滚：有快照就写回；**原本没有这个文件**就把新建的删掉
/// （留着会把一个假 appId 变成"看起来已绑定"）。
fn rollback_config(snapshot: Option<&str>) -> Result<(), String> {
    let path = lark_cli_config_path().ok_or_else(|| "定位不到 lark-cli 配置路径".to_string())?;
    match snapshot {
        Some(prev) => std::fs::write(&path, prev).map_err(|e| e.to_string()),
        None => match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    }
}

/// 取配置里第一个应用的 appId（`apps[0].appId`）。
fn app_id_from_config(raw: &str) -> Option<String> {
    let v = first_json_object(raw)?;
    v.get("apps")?
        .as_array()?
        .first()?
        .get("appId")?
        .as_str()
        .map(str::to_string)
}

/// 检测 lark-cli 的"Agent 工作区里拒绝 `config init`"分支（其 `--help` 明确写了这条）。
///
/// 兜底性质：真正的原文一律进日志，用户看得到；这里只是**额外**给一个 `--force-init`
/// 重试按钮，所以宁可少判（漏判＝少个按钮）也不要乱判。
fn detect_agent_refusal(raw: &str) -> bool {
    let low = raw.to_ascii_lowercase();
    low.contains("force-init") || low.contains("openclaw_home") || low.contains("hermes_home")
}

/// 从输出里捞 `https://` 链接（`config init --new` 的验证链接）。
fn extract_urls(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = raw;
    while let Some(i) = rest.find("https://") {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | ',' | '`' | '，' | '。')
            })
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '；', '：']).to_string();
        if url.len() > "https://".len() && !out.iter().any(|u| u == &url) {
            out.push(url);
        }
        if end >= tail.len() {
            break;
        }
        // `end` 是终止符的**字节起点**，而终止符可能是多字节字符（`，`/`。` 各 3 字节）
        // —— 按 1 字节步进会切在字符中间 panic（lark-cli 输出"请打开 https://…，完成绑定"
        // 即触发；中文标点是常态，不是边角）。
        let step = tail[end..].chars().next().map_or(1, char::len_utf8);
        rest = &tail[end + step..];
    }
    out.truncate(5);
    out
}

// ───────────────────────────── 子进程（带超时） ─────────────────────────────

struct CliOutput {
    /// stdout + stderr **合并**：lark-cli 的握手 JSON 走 stderr，人类可读信息走 stdout，
    /// 两处都要看（只读 stdout 会漏掉所有错误 JSON）。
    combined: String,
    timed_out: bool,
}

/// 代跑 `lark-cli`：带超时、Windows 不闪黑框、输出合并。
fn run_cli(args: &[&str], stdin_data: Option<&str>, timeout: Duration) -> Result<CliOutput, String> {
    let cli = crate::deps::catalog::lark_cli_path()
        .ok_or_else(|| "未发现 lark-cli".to_string())?;
    let mut cmd = crate::process::new(&cli);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|e| format!("启动 lark-cli 失败：{e}"))?;
    if let Some(data) = stdin_data
        && let Some(mut si) = child.stdin.take()
    {
        let _ = si.write_all(data.as_bytes());
        drop(si);
    }
    let out_reader = child.stdout.take().map(spawn_reader);
    let err_reader = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => {
                log::warn!("[LARK-SETUP] 等待 lark-cli 失败：{e}");
                let _ = child.kill();
                break child.wait().ok();
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = status;

    // 子进程已退出/被杀 → 读线程会拿到 EOF（kill 关掉管道）。
    let mut combined = String::new();
    for r in [out_reader, err_reader].into_iter().flatten() {
        if let Ok(s) = r.join() {
            combined.push_str(&s);
        }
    }
    Ok(CliOutput { combined, timed_out })
}

fn spawn_reader<R: Read + Send + 'static>(r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // 出错（非法 UTF-8 等）也把已读到的部分留下 —— 丢输出比乱码更糟。
        let _ = r.take(READ_CAP).read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机 `lark-cli doctor` 的**原始输出**（2026-10-16，1.0.96，一切正常的那台）。
    const DOCTOR_OK: &str = r#"{
  "_notice": {
    "update": {
      "command": "lark-cli update",
      "current": "1.0.96",
      "latest": "1.0.98",
      "message": "lark-cli 1.0.98 available, current 1.0.96, run: lark-cli update"
    }
  },
  "checks": [
    { "name": "cli_version", "status": "pass", "message": "1.0.96" },
    { "name": "cli_update", "status": "warn", "message": "1.0.96 → 1.0.98 available",
      "hint": "run: lark-cli update" },
    { "name": "config_file", "status": "pass", "message": "config.json found" },
    { "name": "app_resolved", "status": "pass", "message": "app: cli_aa5a2e242e785c09 (feishu)" },
    { "name": "bot_identity", "status": "pass", "message": "Bot identity: ready" },
    { "name": "user_identity", "status": "warn", "message": "User identity: missing (no user logged in)",
      "hint": "run: lark-cli auth login --help" },
    { "name": "identity_ready", "status": "pass", "message": "at least one identity is available" },
    { "name": "endpoint_open", "status": "pass", "message": "https://open.feishu.cn reachable" },
    { "name": "endpoint_mcp", "status": "pass", "message": "https://mcp.feishu.cn/mcp reachable" }
  ],
  "ok": true,
  "workspace": "local"
}"#;

    /// 假 appId（隔离 `USERPROFILE` 下 `config init --app-id cli_bogus…` 留下的残留）
    /// 的**原始输出** —— 这是"本地看着已绑定、其实用不了"的样本。
    const DOCTOR_BOGUS: &str = r#"{
  "checks": [
    { "name": "cli_version", "status": "pass", "message": "1.0.96" },
    { "name": "config_file", "status": "pass", "message": "config.json found" },
    { "name": "app_resolved", "status": "pass", "message": "app: cli_bogus000000000 (feishu)" },
    { "name": "bot_identity", "status": "warn",
      "message": "Bot identity: verify failed: The specified app does not exist.",
      "hint": "check app credentials or the active credential provider" },
    { "name": "user_identity", "status": "warn", "message": "User identity: missing (no user logged in)" },
    { "name": "identity_ready", "status": "fail", "message": "no usable bot or user identity is available" },
    { "name": "endpoint_open", "status": "pass", "message": "https://open.feishu.cn reachable" },
    { "name": "endpoint_mcp", "status": "pass", "message": "https://mcp.feishu.cn/mcp reachable" }
  ],
  "ok": false,
  "workspace": "local"
}"#;

    #[test]
    fn parse_doctor_reads_healthy_output() {
        let d = parse_doctor(DOCTOR_OK);
        assert_eq!(d.version.as_deref(), Some("1.0.96"));
        assert_eq!(d.app_id.as_deref(), Some("cli_aa5a2e242e785c09"));
        assert_eq!(d.brand.as_deref(), Some("feishu"));
        assert!(d.healthy);
        assert_eq!(d.bot_status, "pass");
        assert_eq!(d.user_status, "warn");
        assert!(d.raw_error.is_none());
        assert_eq!(d.checks.len(), 9, "检查项要原样全带上（用户自己看是哪条红了）");
        assert!(binding_is_usable(&d));
    }

    /// **本轮最关键的一条**：假 appId 在 `auth status` 里报 ready（见模块注释第 2 点），
    /// 只有 `doctor` 的联网校验能判死 —— 判据必须落在这里。
    #[test]
    fn parse_doctor_flags_an_unusable_binding() {
        let d = parse_doctor(DOCTOR_BOGUS);
        assert_eq!(d.app_id.as_deref(), Some("cli_bogus000000000"));
        assert!(!d.healthy, "ok:false → 不健康");
        assert_eq!(d.bot_status, "warn");
        assert!(
            d.bot_message.contains("verify failed"),
            "原文要留着（用户据此判断是凭据问题）：{}",
            d.bot_message
        );
        assert!(
            !binding_is_usable(&d),
            "appId 存在但 bot 身份校验不过 → **不能用**（否则就是'绿勾但不动'）"
        );
    }

    #[test]
    fn parse_doctor_survives_garbage() {
        let d = parse_doctor("bash: lark-cli: command not found\n");
        assert!(d.raw_error.is_some());
        assert!(!d.healthy, "看不懂的输出绝不能当'健康'");
        assert!(!binding_is_usable(&d));
        assert!(d.app_id.is_none());
    }

    /// 没有 `ok` 字段时：**空检查项不算健康**（解析不到东西就说健康是最危险的撒谎）。
    #[test]
    fn parse_doctor_without_ok_field_is_conservative() {
        let empty = parse_doctor(r#"{"checks": []}"#);
        assert!(!empty.healthy);
        assert!(empty.raw_error.is_some());

        let only_warns = parse_doctor(
            r#"{"checks":[{"name":"cli_update","status":"warn","message":"old"}]}"#,
        );
        assert!(only_warns.healthy, "没有 fail 且确实读到了检查项 → 健康");

        let has_fail = parse_doctor(
            r#"{"checks":[{"name":"identity_ready","status":"fail","message":"nope"}]}"#,
        );
        assert!(!has_fail.healthy);
    }

    /// `app_resolved` 的文本解析：认得出来就要，认不出来（不是 appId）宁可 `None`。
    #[test]
    fn parse_app_resolved_is_conservative() {
        let (id, brand) = parse_app_resolved("app: cli_aa5a2e242e785c09 (feishu)");
        assert_eq!(id.as_deref(), Some("cli_aa5a2e242e785c09"));
        assert_eq!(brand.as_deref(), Some("feishu"));

        let (id, brand) = parse_app_resolved("app: cli_x");
        assert_eq!(id.as_deref(), Some("cli_x"));
        assert!(brand.is_none());

        // 整段话别当成 appId（宁可"看不到 App ID"）
        let (id, _) = parse_app_resolved("no app is configured yet");
        assert!(id.is_none());
        let (id, _) = parse_app_resolved("");
        assert!(id.is_none());
    }

    #[test]
    fn parse_card_dry_run_ready() {
        let raw = r#"{"ok":true,"identity":"bot","dry_run":true,"data":{"decision":{
          "event_key":"card.action.trigger","domain":"card","identity":"bot","status":"ready",
          "preconditions":[{"name":"credentials_available","status":"ok"},
            {"name":"console_event_published","status":"ok"},
            {"name":"scopes_granted","status":"ok"}]}}}"#;
        let c = parse_card_dry_run(raw);
        assert_eq!(c.status, "ready");
    }

    #[test]
    fn parse_card_dry_run_blocked_reports_the_failing_precondition() {
        let raw = r#"{"ok":true,"data":{"decision":{"status":"blocked","preconditions":[
          {"name":"credentials_available","status":"ok"},
          {"name":"console_event_published","status":"blocked"}]}}}"#;
        let c = parse_card_dry_run(raw);
        assert_eq!(c.status, "blocked");
        assert!(c.detail.contains("console_event_published=blocked"), "{}", c.detail);
    }

    /// 命令自己失败（没绑定 / 网络抖）时**不能**断言"被拦了"，也不能说"订阅好了"。
    #[test]
    fn parse_card_dry_run_unknown_on_error() {
        let c = parse_card_dry_run(
            r#"{"ok":false,"error":{"type":"config","subtype":"not_configured","message":"not configured"}}"#,
        );
        assert_eq!(c.status, "unknown");
        assert!(c.detail.contains("not_configured"), "{}", c.detail);

        let c = parse_card_dry_run("totally not json");
        assert_eq!(c.status, "unknown");
        assert!(c.detail.contains("无法解析"));
    }

    #[test]
    fn extract_urls_finds_dedupes_and_strips_punctuation() {
        let raw = "open https://open.feishu.cn/verify?code=abc , or https://open.feishu.cn/verify?code=abc";
        assert_eq!(extract_urls(raw), vec!["https://open.feishu.cn/verify?code=abc".to_string()]);

        let two = extract_urls("a https://x.y/z b https://p.q/r)  ");
        assert_eq!(two, vec!["https://x.y/z".to_string(), "https://p.q/r".to_string()]);

        assert!(extract_urls("no link here").is_empty());
        assert!(extract_urls("https://").is_empty(), "半个 URL 不算");
    }

    #[test]
    fn extract_urls_handles_multibyte_terminators() {
        // 终止符紧跟多字节字符（`，`/`。`，各 3 字节）时不能按字节 +1 续扫，
        // 否则切片落在字符中间 panic（2026-10-18 审计发现的真实崩溃路径）。
        let raw = "请打开 https://open.feishu.cn/verify?code=abc，完成绑定";
        assert_eq!(extract_urls(raw), vec!["https://open.feishu.cn/verify?code=abc".to_string()]);

        let raw2 = "链接 https://a.b/c。";
        assert_eq!(extract_urls(raw2), vec!["https://a.b/c".to_string()]);

        // 中文标点之后还有第二个链接：续扫必须继续找到它。
        let raw3 = "先 https://a.b/c，再 https://d.e/f。";
        assert_eq!(extract_urls(raw3), vec!["https://a.b/c".to_string(), "https://d.e/f".to_string()]);
    }

    #[test]
    fn detect_agent_refusal_only_on_real_signals() {
        assert!(detect_agent_refusal("... use --force-init if you really want ..."));
        assert!(detect_agent_refusal("inside a HERMES_HOME workspace"));
        assert!(!detect_agent_refusal("OK: Configuration saved to C:\\Users\\x\\.lark-cli\\config.json"));
    }

    #[test]
    fn app_id_from_config_reads_first_app() {
        let raw = r#"{"apps":[{"appId":"cli_abc","brand":"feishu"}],"other":1}"#;
        assert_eq!(app_id_from_config(raw).as_deref(), Some("cli_abc"));
        assert!(app_id_from_config(r#"{"apps":[]}"#).is_none());
        assert!(app_id_from_config("not json").is_none());
    }

    /// "下一步"取第一个阻塞项，且**顺序**不能反（没装 CLI 时谈绑定没意义）。
    #[test]
    fn first_missing_walks_the_prerequisite_chain() {
        let base = LarkSelfCheck {
            cli_path: Some("C:\\x\\lark-cli.exe".into()),
            app_id: Some("cli_abc".into()),
            bot_status: "pass".into(),
            card_callback: "ready".into(),
            ..Default::default()
        };
        assert!(first_missing(&base).is_empty(), "前置齐了就没有提示");

        let no_cli = LarkSelfCheck {
            cli_path: None,
            ..base.clone()
        };
        assert_eq!(first_missing(&no_cli), vec!["install_cli".to_string()]);

        let unbound = LarkSelfCheck {
            app_id: None,
            ..base.clone()
        };
        assert_eq!(first_missing(&unbound), vec!["bind_app".to_string()]);

        // 绑了但联网校验不过 —— "绿勾但不动"那一类，必须报出来
        let broken = LarkSelfCheck {
            bot_status: "warn".into(),
            ..base.clone()
        };
        assert_eq!(first_missing(&broken), vec!["rebind_app".to_string()]);

        let no_card = LarkSelfCheck {
            card_callback: "blocked".into(),
            ..base.clone()
        };
        assert_eq!(first_missing(&no_card), vec!["card_callback".to_string()]);
    }

    /// `bind_start` 的参数校验（**不碰真配置**：全在起子进程之前就返回了）。
    #[test]
    fn bind_start_rejects_bad_requests() {
        let bad_mode = BindRequest {
            mode: "whatever".into(),
            app_id: None,
            app_secret: None,
            brand: None,
            force_init: false,
        };
        assert!(bind_start(&bad_mode).is_err());

        let no_id = BindRequest {
            mode: "existing".into(),
            app_id: Some("  ".into()),
            app_secret: Some("s".into()),
            brand: None,
            force_init: false,
        };
        assert!(bind_start(&no_id).is_err());

        let no_secret = BindRequest {
            mode: "existing".into(),
            app_id: Some("cli_abc".into()),
            app_secret: Some("".into()),
            brand: None,
            force_init: false,
        };
        assert!(bind_start(&no_secret).is_err());

        let bad_brand = BindRequest {
            mode: "new".into(),
            app_id: None,
            app_secret: None,
            brand: Some("dingtalk".into()),
            force_init: false,
        };
        assert!(bind_start(&bad_brand).is_err());
    }

    /// 会话状态在没有会话时是"空的"，界面据此判断"未开始"。
    #[test]
    fn bind_status_without_session_is_empty() {
        bind_dismiss();
        let st = bind_status();
        assert!(st.mode.is_empty());
        assert!(!st.running);
        assert!(st.urls.is_empty());
        assert!(st.log.is_empty());
    }
}
