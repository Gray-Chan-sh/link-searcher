//! 飞书 bot 的**生命周期**（`docs/LARK_BOT.md` §5.4 的 C3 —— 文档里标为"唯一必做"）。
//!
//! 由 GUI 拉起 bot（`docs/LARK_BOT.md` §5.1「最优形态：GUI 拉起 bot sidecar」），
//! 应用退出时把它收掉。三条设计要点：
//!
//! 1. **可执行文件按"与主程序同目录"解析**（`current_exe()` 的旁边）。
//!    开发下 `cargo` 已把两个 bin 都放进 `target/debug/`，所以**无需** `externalBin`
//!    就能跑 —— 这很重要：`externalBin` 要求 `<name>-<target-triple>` 文件存在，
//!    配置了却没准备文件会**直接打断 `tauri dev`**。打包时再补那一步。
//! 2. **退出时 kill bot 即可**，不需要给它发信号：bot 持有 lark-cli 的 stdin 管道，
//!    bot 一死该管道即 EOF，lark-cli 会**优雅退出**（它自己的提示就说了 "close stdin"）。
//!    这条路已实测：kill bot 后 lark-cli 消费者退出、**bus daemon 存活**。
//! 3. **不主动停 bus daemon**（按决策"留着复用"）：它是跨 app 单例，杀掉既有订阅泄漏风险，
//!    又会迫使下次启动重连。留着它，下次 bot 起来直接复用。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// 已拉起的 bot 子进程。`None` ＝ 没在跑。
static BOT: Mutex<Option<Child>> = Mutex::new(None);

/// bot 的可执行文件名（与主程序同目录）。
fn bot_file_name() -> &'static str {
    if cfg!(windows) { "lark-bot.exe" } else { "lark-bot" }
}

/// 可执行文件后缀（用于容错匹配）。
fn bot_ext() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

/// 解析 bot 可执行文件路径：**主程序旁边**。
///
/// 两种命名都要认，否则打包后会找不到：
/// - 开发：`cargo` 产出 `lark-bot[.exe]`（无后缀）
/// - 打包：`bundle.externalBin` 的命名要求是 `lark-bot-<target-triple>[.exe]`
///   （Tauri 在装包时可能保留也可能去掉三元组后缀 → 所以这里按前缀扫一遍兜住）
fn bot_exe_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe 失败: {e}"))?;
    let dir = exe.parent().ok_or_else(|| "主程序没有父目录".to_string())?;

    let plain = dir.join(bot_file_name());
    if plain.is_file() {
        return Ok(plain);
    }

    let ext = bot_ext();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("lark-bot") && name.ends_with(ext) && entry.path().is_file() {
                return Ok(entry.path());
            }
        }
    }

    Err(format!(
        "在 {} 里没找到 lark-bot{}（开发下跑 `npm run sidecar` 生成；打包需 bundle.externalBin）",
        dir.display(),
        ext
    ))
}

/// 解析 `lark-cli` 可执行文件 —— **应用侧的唯一权威实现**（同时是「依赖中心」的就绪探测）。
///
/// 顺序（与 bot 内的兜底探测保持一致，便于两边行为对齐）：
/// 1. 环境变量 `LS_LARK_CLI`（用户/CI 显式指定）；
/// 2. npm 全局的原生二进制 —— Windows 上 `lark-cli` 只是 `.cmd` 包装，而 `CreateProcess`
///    **不能**直接执行 `.cmd`（要经 cmd.exe，届时参数会被二次重解析，而回复正文任意 → 坑很深）；
/// 3. PATH 上的 `lark-cli`（只认 `.exe`；`.cmd` 交给 bot 自己那层兜底）。
///
/// 找不到返回 `None` —— **不报错**："还没装 lark-cli"是可接受的初始状态，且 bot 自己还有一层兜底。
///
/// 不写单测是**有意的**：它读进程级 env / PATH，写单测就得 `set_var`（正是 AGENTS ④ 那类
/// "全局可变状态导致并行假红"）。它的正确性由"注入后 bot 是否用它"这条运行时链路保证。
fn lark_cli_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LS_LARK_CLI") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
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
            return Some(native);
        }
    }
    let exe = if cfg!(windows) { "lark-cli.exe" } else { "lark-cli" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(exe))
            .find(|p| p.is_file())
    })
}

/// 若 `lark_bot_enabled == "true"` 则拉起 bot。**任何失败都只记日志**，
/// 绝不因此影响应用启动。
pub fn spawn_if_enabled(app: &AppHandle) {
    let enabled = app
        .state::<AppState>()
        .db
        .get()
        .ok()
        .and_then(|conn| {
            conn.query_row::<String, _, _>(
                "SELECT value FROM app_settings WHERE key = 'lark_bot_enabled'",
                [],
                |r| r.get(0),
            )
            .ok()
        })
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if !enabled {
        log::info!("[LARK-BOT] 未启用（lark_bot_enabled != \"true\"），不拉起");
        return;
    }

    let path = match bot_exe_path() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[LARK-BOT] {e}");
            return;
        }
    };

    let mut cmd = Command::new(&path);
    cmd
        // 让 bot 看住我们：应用被**硬杀/崩溃**时 `RunEvent::Exit` 不会执行，
        // 没有任何人会收它 —— 不看着父进程就会留下孤儿 bot（2026-10-15 实测确认）。
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        // stdin 保留为管道：它是"关 stdin 即优雅停机"的落点（Windows 没有可用的 SIGTERM）。
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // ── lark-cli 的发现权归**应用侧**（依赖中心），经环境变量交给 bot ────────────────
    // bot 刻意不依赖应用 lib（上一轮拆 crate 的成果），所以两边**不能共享实现**；
    // 这条环境变量就是接缝：应用解析 → 注入 → bot 直接用（bot 自带探测仅作手动运行的兜底）。
    // 也是「依赖中心」将来"显示状态 / 一键安装"的前置。
    match lark_cli_path() {
        Some(cli) => {
            log::info!("[LARK-BOT] 使用 lark-cli = {}", cli.display());
            cmd.env("LS_LARK_CLI", &cli);
        }
        None => log::warn!(
            "[LARK-BOT] 未发现 lark-cli（`LS_LARK_CLI` / npm 全局路径 / PATH 都没有）—— 交给 bot 自行兜底探测"
        ),
    }

    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(out) = child.stdout.take() {
                forward(out, "lark-bot", false);
            }
            if let Some(err) = child.stderr.take() {
                forward(err, "lark-bot", true);
            }
            log::info!(
                "[LARK-BOT] 已拉起 {} (pid={})",
                path.display(),
                child.id()
            );
            *BOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        }
        Err(e) => log::warn!("[LARK-BOT] 启动失败（{}）：{e}", path.display()),
    }
}

/// 把子进程的一路输出逐行转发进应用日志。
fn forward<R: std::io::Read + Send + 'static>(reader: R, tag: &'static str, is_err: bool) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if is_err {
                log::warn!("[{tag}] {line}");
            } else {
                log::info!("[{tag}] {line}");
            }
        }
    });
}

/// 收掉 bot（应用退出时调用）。
///
/// **只 kill bot，不去动 lark-cli 的 bus daemon** —— 见模块注释第 2、3 点。
pub fn shutdown() {
    let mut slot = BOT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = slot.take() {
        match child.kill() {
            Ok(()) => log::info!("[LARK-BOT] 已停止（pid={}）", child.id()),
            Err(e) => log::warn!("[LARK-BOT] 停止失败：{e}"),
        }
        let _ = child.wait();
    }
}
