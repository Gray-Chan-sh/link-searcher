//! Cross-platform subprocess helpers.
//!
//! On Windows, the Link-Searcher GUI is a `windows_subsystem = "windows"`
//! binary with no console. Spawning a console-subsystem child (tesseract,
//! poppler tools, ffmpeg…) from such a process makes Windows create a brand
//! new console window for the child — visible as black cmd windows flashing
//! during scans. Setting `CREATE_NO_WINDOW` tells the child to run without
//! creating a console, so the flash is gone.
//!
//! All `Command` construction for external CLI tools should go through
//! [`new`] / [`probe_ok`] so this flag is applied consistently everywhere.

use std::process::Command;

/// Build a [`Command`] for an external program, suppressing the console
/// window on Windows.
pub fn new<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);
    suppress_console(&mut cmd);
    cmd
}

/// Run a cheap capability probe (`<program> <args…>`) discarding output.
/// Never flashes a console window on Windows.
pub fn probe_ok<S: AsRef<std::ffi::OsStr>>(program: S, args: &[&str]) -> bool {
    let mut cmd = new(program);
    cmd.args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Append `.exe` on Windows. `poppler-bin`/`ffmpeg-bin` trees and the
/// "next to the executable" probes store Windows binaries under their
/// platform name; the code was written against bare Unix names.
pub fn windows_exe_name(name: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        if name.to_ascii_lowercase().ends_with(".exe") {
            name.to_string()
        } else {
            format!("{name}.exe")
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        name.to_string()
    }
}

/// Refresh this process's `PATH` from the OS so tools installed *while the app
/// is running* become visible without a restart.
///
/// A process keeps the environment it was launched with, so an installer that
/// updates the OS-level PATH does not affect an already-running app.
///
/// - **Windows**: re-read the user + machine PATH from the registry (winget /
///   chocolatey / scoop update the registry).
/// - **macOS**: run `/usr/libexec/path_helper -s`, which rebuilds PATH from
///   `/etc/paths` and `/etc/paths.d` (a GUI app launched from Finder only gets
///   a minimal PATH).
/// - **Linux**: no-op (desktop sessions normally inherit a full PATH).
#[cfg(target_os = "windows")]
pub fn refresh_path() {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let read = |hive, subkey: &str| -> Option<String> {
        RegKey::predef(hive)
            .open_subkey(subkey)
            .ok()?
            .get_value::<String, _>("Path")
            .ok()
    };

    let machine = read(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    let user = read(HKEY_CURRENT_USER, r"Environment");
    if machine.is_none() && user.is_none() {
        return;
    }

    let mut combined = String::new();
    if let Some(m) = machine {
        combined.push_str(&m);
    }
    if let Some(u) = user {
        if !combined.is_empty() {
            combined.push(';');
        }
        combined.push_str(&u);
    }

    let expanded = expand_env_vars(&combined);
    // SAFETY: best-effort refresh of this process's own environment. Called
    // once at startup and once after an install, never concurrently in a hot
    // path.
    unsafe { std::env::set_var("PATH", expanded) };
}

#[cfg(target_os = "macos")]
pub fn refresh_path() {
    // `path_helper -s` prints `PATH="…"; export PATH;`.
    let Ok(out) = new("/usr/libexec/path_helper").arg("-s").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(value) = text
        .split("PATH=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
    else {
        return;
    };
    if value.is_empty() {
        return;
    }
    // SAFETY: best-effort refresh of this process's own environment. Called
    // once at startup and once after an install, never concurrently in a hot
    // path.
    unsafe { std::env::set_var("PATH", value) };
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn refresh_path() {}

/// Common non-PATH install locations probed on macOS/Linux when a bare-name
/// lookup fails: a GUI app may not inherit the shell PATH, and there is no
/// registry to re-read. Homebrew (Apple Silicon/Intel), MacPorts, snap and the
/// standard system dirs.
#[cfg(not(target_os = "windows"))]
pub const UNIX_BIN_PREFIXES: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/snap/bin",
    "/usr/bin",
];

/// Expand `%NAME%` references in a Windows `REG_EXPAND_SZ` value using the
/// current process environment (e.g. `%SystemRoot%` → `C:\Windows`).
#[cfg(target_os = "windows")]
fn expand_env_vars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                if name.is_empty() {
                    out.push('%');
                } else if let Ok(val) = std::env::var(name) {
                    out.push_str(&val);
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `CREATE_NO_WINDOW` — the child gets no console window even when the parent
/// is a GUI process. Only meaningful on Windows; a no-op elsewhere.
#[cfg(target_os = "windows")]
fn suppress_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn suppress_console(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::windows_exe_name;

    #[test]
    fn exe_name_append_only_on_windows() {
        let name = windows_exe_name("ffmpeg");
        #[cfg(target_os = "windows")]
        assert_eq!(name, "ffmpeg.exe");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(name, "ffmpeg");
    }

    #[test]
    fn exe_name_idempotent() {
        let once = windows_exe_name("ffmpeg");
        let twice = windows_exe_name(&once);
        assert_eq!(once, twice);
    }
}
