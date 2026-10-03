#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
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

/// Windows 上，链接了 tauri GUI 栈（`windows` crate → `comctl32!TaskDialogIndirect`）
/// 的**测试二进制**默认没有应用 manifest，加载器会按 System32 的 comctl32
/// **v5.82** 绑定，而 `TaskDialogIndirect` 只有 **v6** 才导出 → 进程在任何测试
/// 代码运行前就以 `STATUS_ENTRYPOINT_NOT_FOUND` (0xc0000139) 终止（`ipc_test` /
/// `auto_ui_e2e` 即此症）。
///
/// 主二进制由 `tauri_build::build()` 嵌入了完整的 v6 manifest，但那只作用于
/// bin target；这里用 `rustc-link-arg-tests` **只给测试目标**补一份声明
/// comctl32 v6 的 manifest，不触碰主二进制。
#[cfg(windows)]
fn embed_test_manifest() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let manifest_path = std::path::Path::new(&out_dir).join("comctl32-v6.manifest");
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;
    if std::fs::write(&manifest_path, manifest).is_err() {
        return;
    }
    println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
        manifest_path.display()
    );
    println!("cargo:rustc-link-arg-tests=/MANIFESTUAC:NO");
}

fn main() {
    #[cfg(windows)]
    embed_test_manifest();

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
        let dst = bin_dir.join(bin);
        if dst.exists() { continue; }
        for prefix in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            let src = std::path::PathBuf::from(prefix).join(bin);
            if src.exists() {
                std::fs::copy(&src, &dst).ok();
                #[cfg(unix)]
                std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).ok();
                break;
            }
        }
    }
    println!("cargo:rustc-env=POPPLER_BIN_DIR=poppler-bin");

    // Copy ffmpeg into resources for app-bundle packaging (audio decode).
    let ff_dir = std::path::PathBuf::from("ffmpeg-bin");
    let _ = std::fs::create_dir_all(&ff_dir);
    let ff_dst = ff_dir.join("ffmpeg");
    if !ff_dst.exists() {
        for prefix in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            let src = std::path::PathBuf::from(prefix).join("ffmpeg");
            if src.exists() {
                std::fs::copy(&src, &ff_dst).ok();
                #[cfg(unix)]
                std::fs::set_permissions(&ff_dst, std::fs::Permissions::from_mode(0o755)).ok();
                break;
            }
        }
    }
    println!("cargo:rustc-env=FFMPEG_BIN_DIR=ffmpeg-bin");

    // Create model directories
    let _ = std::fs::create_dir_all("models/funasr");

    // Check for optional FunASR models (sherpa-onnx int8 layout)
    let has_asr = [
        "models/funasr/encoder_adaptor.int8.onnx",
        "models/funasr/llm.int8.onnx",
        "models/funasr/embedding.int8.onnx",
        "models/funasr/Qwen3-0.6B/tokenizer.json",
    ]
    .iter()
    .all(|p| std::path::Path::new(p).is_file());
    println!("cargo:rustc-env=HAS_ASR_MODELS={}", if has_asr { "1" } else { "0" });

    tauri_build::build()
}
