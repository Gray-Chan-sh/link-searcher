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
