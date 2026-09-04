//! Static catalog of installable runtime dependencies.
//!
//! Pure data + pure readiness predicates — no I/O beyond `Path::is_file`.
//! Kept separate from `download.rs` so the exact mirror URLs are easy to
//! audit and change in one place.
//!
//! ## Download source
//!
//! Link-Searcher publishes its model assets as **GitHub Releases** on the
//! `Gray-Chan-sh/link-searcher-models` repo (flat asset names, see
//! [`gh_base`]). For China-network friendliness the *app* prefers mirrors of
//! github.com (e.g. `ghproxy`-style) at runtime; the catalog keeps GitHub as
//! the canonical base and the download layer can rewrite it.

use std::path::Path;

/// One file of a dependency: remote path relative to the source base URL, and
/// local path relative to the dep's install dir.
#[derive(Debug, Clone)]
pub struct FileSpec {
    pub remote: &'static str,
    pub local: &'static str,
}

/// A mirror source: `(label, base_url)`. Files resolve as `<base>/<remote>`.
#[derive(Debug, Clone)]
pub struct Source {
    pub label: &'static str,
    pub base_url: String,
}

/// A model/dependency definition. Owns its strings so `all()` can build
/// source URLs from the configured GitHub repo at call time.
#[derive(Debug, Clone)]
pub struct DepDef {
    pub id: &'static str,
    pub name: &'static str,
    pub recommended: bool,
    pub size_bytes: u64,
    pub hint: &'static str,
    pub files: &'static [FileSpec],
    pub sources: Vec<Source>,
}

/// All catalog entries (fresh copy each call; cheap, pure).
pub fn all() -> Vec<DepDef> {
    vec![
        paddleocr(),
        bge_small(),
        funasr(),
        ffmpeg(),
        poppler(),
    ]
}

/// GitHub owner/repo + release tag hosting Link-Searcher's model assets.
///
/// Default: `Gray-Chan-sh/link-searcher-models`, tag `models-v1`. Override for
/// forks via `LINK_SEARCHER_MODELS_GH` ("owner/repo") and
/// `LINK_SEARCHER_MODELS_TAG`.
///
/// Release assets are named exactly like the local files so a plain
/// `<base>/<remote>` join works.
pub fn gh_base() -> String {
    let repo = std::env::var("LINK_SEARCHER_MODELS_GH")
        .unwrap_or_else(|_| "Gray-Chan-sh/link-searcher-models".to_string());
    let tag = std::env::var("LINK_SEARCHER_MODELS_TAG")
        .unwrap_or_else(|_| "models-v1".to_string());
    format!("https://github.com/{repo}/releases/download/{tag}")
}

/// PP-OCRv5 (det + rec + dict) — built-in cross-platform Chinese OCR.
///
/// Files are published as flat GitHub release assets under the
/// Link-Searcher models repo (see [`gh_base`]). In a dev checkout the
/// identical files are git-tracked at `src-tauri/models/ppocrv5/` so
/// `tauri dev` needs no download.
fn paddleocr() -> DepDef {
    DepDef {
        id: "paddleocr",
        name: "PaddleOCR 模型（图片/扫描件 OCR）",
        recommended: true,
        size_bytes: 20 * 1024 * 1024,
        hint: "图片与扫描版 PDF 的文字识别（约 20MB）",
        files: &[
            FileSpec { remote: "paddleocr-det.onnx", local: "det.onnx" },
            FileSpec { remote: "paddleocr-rec.onnx", local: "rec.onnx" },
            FileSpec { remote: "paddleocr-ppocrv5_dict.txt", local: "ppocrv5_dict.txt" },
        ],
        sources: vec![Source {
            label: "GitHub Releases",
            base_url: gh_base(),
        }],
    }
}

/// BGE-small-zh-v1.5 (512-dim) — offline semantic embeddings.
fn bge_small() -> DepDef {
    DepDef {
        id: "bge-small",
        name: "BGE-small 本地语义模型（离线向量）",
        recommended: true,
        size_bytes: 95 * 1024 * 1024,
        hint: "离线语义搜索 / embedding（约 95MB）",
        files: &[
            FileSpec { remote: "bge-small-model.onnx", local: "model.onnx" },
            FileSpec { remote: "bge-small-tokenizer.json", local: "tokenizer.json" },
        ],
        sources: vec![Source {
            label: "GitHub Releases",
            base_url: gh_base(),
        }],
    }
}

/// FunASR-Nano (sherpa-onnx int8) — offline audio transcription (~850MB).
fn funasr() -> DepDef {
    DepDef {
        id: "funasr",
        name: "FunASR 语音转写模型",
        recommended: false,
        size_bytes: 850 * 1024 * 1024,
        hint: "音频转文字（约 850MB，可选）",
        files: &[
            FileSpec { remote: "funasr-encoder_adaptor.int8.onnx", local: "encoder_adaptor.int8.onnx" },
            FileSpec { remote: "funasr-llm.int8.onnx", local: "llm.int8.onnx" },
            FileSpec { remote: "funasr-embedding.int8.onnx", local: "embedding.int8.onnx" },
            FileSpec { remote: "funasr-tokenizer.json", local: "Qwen3-0.6B/tokenizer.json" },
        ],
        sources: vec![Source {
            label: "GitHub Releases",
            base_url: gh_base(),
        }],
    }
}

/// FFmpeg — audio decode. System-provided; never downloaded by the app but
/// surfaced in the wizard with an install guide when missing.
fn ffmpeg() -> DepDef {
    DepDef {
        id: "ffmpeg",
        name: "FFmpeg（音频解码）",
        recommended: false,
        size_bytes: 0,
        hint: "音频文件解码，缺失时按平台引导安装",
        files: &[],
        sources: vec![],
    }
}

/// Poppler (pdftoppm) — render PDF pages for OCR fallback. System-provided.
fn poppler() -> DepDef {
    DepDef {
        id: "poppler",
        name: "Poppler（PDF 渲染）",
        recommended: false,
        size_bytes: 0,
        hint: "扫描版 PDF 渲染，缺失时按平台引导安装",
        files: &[],
        sources: vec![],
    }
}

/// Install directory for a dep under `data_dir`.
pub fn install_dir(def: &DepDef, data_dir: &Path) -> std::path::PathBuf {
    let sub = match def.id {
        "paddleocr" => "ppocrv5",
        "bge-small" => "bge-small-zh-v1.5",
        "funasr" => "funasr",
        other => other,
    };
    data_dir.join("models").join(sub)
}

/// Readiness predicate: every required file exists locally and is non-empty.
pub fn is_ready(def: &DepDef, data_dir: &Path) -> bool {
    // System deps are checked by their own probes.
    match def.id {
        "ffmpeg" => return crate::extractor::audio::ffmpeg_available(),
        "poppler" => return crate::extractor::pdf::is_pdftoppm_available(),
        _ => {}
    }

    let dir = install_dir(def, data_dir);
    if files_ready(&dir, def.files) {
        return true;
    }

    // Dev fast-path: PP-OCRv5 is git-tracked in the source tree.
    if def.id == "paddleocr" {
        let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("models")
            .join("ppocrv5");
        return files_ready(&dev, def.files);
    }

    false
}

fn files_ready(dir: &Path, files: &[FileSpec]) -> bool {
    !files.is_empty()
        && files.iter().all(|f| {
            dir.join(f.local).metadata().map(|m| m.len() > 0).unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ls_deps_catalog_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Serialize tests that call `all()` against the env-mutating test
    /// (`gh_base_default_and_override`) — env vars are process-global.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn every_dep_has_id_and_unique_files() {
        let _g = env_lock();
        let ids = all().iter().map(|d| d.id).collect::<Vec<_>>();
        // No duplicate ids.
        let mut uniq = ids.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(ids.len(), uniq.len(), "dep ids must be unique");

        for def in all() {
            if !def.files.is_empty() {
                // remote path never collides within a dep (no overwrite).
                let remotes = def.files.iter().map(|f| f.remote).collect::<Vec<_>>();
                let mut r = remotes.clone();
                r.sort_unstable();
                r.dedup();
                assert_eq!(remotes.len(), r.len(), "dep {} has duplicate remote", def.id);
            }
        }
    }

    #[test]
    fn paddleocr_dev_tree_counts_ready() {
        let _g = env_lock();
        // In a dev checkout the git-tracked models exist — is_ready must be
        // true without any data_dir copy.
        let data = tmpdir("paddleocr");
        let def = all().into_iter().find(|d| d.id == "paddleocr").unwrap();
        let has_dev_files = {
            let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models").join("ppocrv5");
            files_ready(&dev, def.files)
        };
        // This assertion is only meaningful when run from a dev tree with the
        // models present; CI without the models dir would fail, so gate it.
        if has_dev_files {
            assert!(is_ready(&def, &data), "dev tree models must satisfy paddleocr dep");
        }
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn install_dir_layout_is_expected() {
        let _g = env_lock();
        let data = PathBuf::from("/tmp/ls_data");
        for def in all() {
            let dir = install_dir(&def, &data);
            let expected = match def.id {
                "paddleocr" => "ppocrv5",
                "bge-small" => "bge-small-zh-v1.5",
                "funasr" => "funasr",
                other => other,
            };
            assert_eq!(dir, data.join("models").join(expected), "layout for {}", def.id);
        }
    }

    #[test]
    fn gh_base_default_and_override() {
        let _g = env_lock();
        unsafe { std::env::remove_var("LINK_SEARCHER_MODELS_GH") };
        unsafe { std::env::remove_var("LINK_SEARCHER_MODELS_TAG") };
        let b = gh_base();
        assert!(b.starts_with("https://github.com/Gray-Chan-sh/link-searcher-models/releases/download/models-v1"), "{b}");

        unsafe { std::env::set_var("LINK_SEARCHER_MODELS_GH", "someone/else") };
        unsafe { std::env::set_var("LINK_SEARCHER_MODELS_TAG", "t2") };
        let b2 = gh_base();
        assert!(b2.contains("someone/else/releases/download/t2"), "{b2}");
        unsafe { std::env::remove_var("LINK_SEARCHER_MODELS_GH") };
        unsafe { std::env::remove_var("LINK_SEARCHER_MODELS_TAG") };
    }

    #[test]
    fn funasr_tokenizer_nested_local_path() {
        let _g = env_lock();
        // The FunASR tokenizer must land under Qwen3-0.6B/ so the extractor
        // finds it at data/models/funasr/Qwen3-0.6B/tokenizer.json.
        let def = all().into_iter().find(|d| d.id == "funasr").unwrap();
        let tok = def.files.iter().find(|f| f.local.contains("tokenizer.json")).unwrap();
        assert_eq!(tok.local, "Qwen3-0.6B/tokenizer.json");
        let data = PathBuf::from("/tmp/ls_data");
        let dest = install_dir(&def, &data).join(tok.local);
        assert_eq!(dest, data.join("models/funasr/Qwen3-0.6B/tokenizer.json"));
    }
}
