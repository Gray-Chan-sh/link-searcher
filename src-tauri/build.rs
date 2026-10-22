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

/// 声明「哪些 git 文件变了才需要重跑本脚本」。
///
/// ⚠️ `cargo:rerun-if-changed` 的路径以**包根（`src-tauri/`）**为基准，而 `.git`
/// 在上一层的仓库根。原来写的 `.git/HEAD` 因此根本不存在，而 cargo 的规则是
/// **声明了但文件不存在 = 永远脏** —— 于是这个脚本每次构建都重跑，它输出的
/// `GIT_VERSION` 又让整个 lib 的指纹失效，连"无改动"的构建都要全量重编
/// （实测 2m15s，本应 <1s）。
///
/// 只声明真实存在的路径，这样从没有 `.git` 的源码包里构建时也不会退化成"永远脏"。
///
/// 故意**不**监听 `.git/index`：`git status`/`git add` 都会刷新它，会把每次查看
/// 状态都变成一次全量重编。代价是版本号里的 `-dirty` 后缀只在提交/切分支时刷新。
fn watch_git_files() {
    let git = std::path::Path::new("../.git");
    let mut watched = vec![git.join("HEAD")];
    // 提交号在 ref 文件里；HEAD 只是指向它的名字。
    if let Ok(head) = std::fs::read_to_string(git.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            watched.push(git.join(reference));
        }
    }
    for path in watched {
        if path.exists() {
            let normalized = path.to_string_lossy().replace('\\', "/");
            println!("cargo:rerun-if-changed={normalized}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
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
    watch_git_files();

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
        "models/funasr/Qwen3-0.6B/vocab.json",
        "models/funasr/Qwen3-0.6B/merges.txt",
    ]
    .iter()
    .all(|p| std::path::Path::new(p).is_file());
    println!("cargo:rustc-env=HAS_ASR_MODELS={}", if has_asr { "1" } else { "0" });

    tauri_build::build()
}
