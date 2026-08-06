use std::process::Command;

fn git_output(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string())
}

fn git_dirty() -> bool {
    Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false)
}

fn main() {
    let hash = git_output(&["rev-parse", "--short", "HEAD"]);
    let time = git_output(&["log", "-1", "--format=%ci"]);
    let dirty = git_dirty();

    let version = if dirty {
        format!("{}-dirty", hash.trim())
    } else {
        hash.trim().to_string()
    };

    println!("cargo:rustc-env=GIT_VERSION={version}");
    println!("cargo:rustc-env=GIT_COMMIT_TIME={}", time.trim());
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");

    // Copy poppler binaries into resources for app-bundle packaging
    let bin_dir = std::path::PathBuf::from("poppler-bin");
    let _ = std::fs::create_dir_all(&bin_dir);
    for bin in ["pdftoppm", "pdfimages", "pdfinfo", "pdftotext"] {
        for prefix in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            let src = std::path::PathBuf::from(prefix).join(bin);
            if src.exists() {
                let dst = bin_dir.join(bin);
                let _ = std::fs::copy(&src, &dst);
                break;
            }
        }
    }
    println!("cargo:rustc-env=POPPLER_BIN_DIR=poppler-bin");

    tauri_build::build()
}
