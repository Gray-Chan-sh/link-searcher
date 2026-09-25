//! Static catalog of installable runtime dependencies.
//!
//! Pure data + pure readiness predicates — no I/O beyond `Path::is_file`.
//! Kept separate from `download.rs` so the exact mirror URLs are easy to
//! audit and change in one place.
//!
//! ## Download source
//!
//! Most model assets are published as **GitHub Releases** on the
//! `Gray-Chan-sh/link-searcher-models` repo (flat asset names, see
//! [`gh_base`]). For China-network friendliness the *app* prefers mirrors of
//! github.com (e.g. `ghproxy`-style) at runtime; the catalog keeps GitHub as
//! the canonical base and the download layer can rewrite it.
//!
//! The BGE embedding model is the exception: its ~1.2GB fp32 ONNX is served
//! from the Xenova HuggingFace conversion via `hf-mirror.com` (with
//! ModelScope as fallback) instead of the GitHub release. Non-GitHub sources
//! are used verbatim (no ghproxy prefix).

use std::path::Path;

/// One file of a dependency: remote path relative to the source base URL, local
/// path relative to the dep's install dir, and the SHA-256 of the published
/// asset. The digest comes from the GitHub release asset (`digest` field);
/// `download.rs` verifies completed files against it so a truncated or corrupt
/// download is never mistaken for a complete one.
#[derive(Debug, Clone)]
pub struct FileSpec {
    pub remote: &'static str,
    pub local: &'static str,
    /// Lowercase hex SHA-256 of the published file.
    pub sha256: &'static str,
}

/// A mirror source: `(label, base_url)`. Files resolve as `<base>/<remote>`.
#[derive(Debug, Clone)]
pub struct Source {
    pub label: &'static str,
    pub base_url: String,
}

/// Platform package manager install spec for system-provided deps
/// (ffmpeg, poppler). When present, `install_dep` invokes the platform
/// package manager instead of downloading files.
#[derive(Debug, Clone)]
pub struct SystemPackage {
    pub winget: Option<&'static str>,
    pub brew: Option<&'static str>,
    pub apt: Option<&'static str>,
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
    pub system_package: Option<SystemPackage>,
}

/// All catalog entries (fresh copy each call; cheap, pure).
pub fn all() -> Vec<DepDef> {
    let mut defs = vec![
        paddleocr(),
        bge_large(),
        bge_base(),
        bge_small(),
    ];
    defs.extend(rerank_defs());
    defs.push(funasr());
    defs.push(ffmpeg());
    defs.push(poppler());
    defs
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
            FileSpec { remote: "paddleocr-det.onnx", local: "det.onnx", sha256: "8c3b7ee97913a7942b8565669dc9acbe8846fbbaf4b63e1d7fdb339005574a33" },
            FileSpec { remote: "paddleocr-rec.onnx", local: "rec.onnx", sha256: "31fb844ce3a4aaf13e4bea62ae35f43bd9a509966061980c30db9b248c542a6b" },
            FileSpec { remote: "paddleocr-ppocrv5_dict.txt", local: "ppocrv5_dict.txt", sha256: "1ea29636956177e400af712d9782e7693f3fb25f98617bed10479d2965a836fd" },
        ],
        sources: vec![Source {
            label: "GitHub Releases",
            base_url: gh_base(),
        }],
        system_package: None,
    }
}

/// BGE-large-zh-v1.5 (1024-dim) — offline semantic embeddings.
///
/// Served from the Xenova ONNX conversion on HuggingFace (via hf-mirror) and
/// ModelScope — the ~1.2GB fp32 asset is NOT published to the GitHub release.
/// Both sources are non-GitHub, so `download.rs` uses their URLs as-is without
/// prepending a ghproxy prefix. SHA-256 of `model.onnx` is the HF LFS oid;
/// the tokenizer digest was measured from the mirrored bytes.
fn bge_large() -> DepDef {
    DepDef {
        id: "bge-large",
        name: "BGE-large 本地语义模型（1024维，离线向量）",
        recommended: true,
        size_bytes: 1_298_815_581,
        hint: "离线语义搜索 / embedding（1024 维，约 1.2GB）",
        files: &[
            FileSpec { remote: "onnx/model.onnx", local: "model.onnx", sha256: "8a78f0b748a6746a0a2ebe0563fddb311762e260abcadaa2b9f19c6964b745fe" },
            FileSpec { remote: "tokenizer.json", local: "tokenizer.json", sha256: "7dfbf1966ebf99d471c3796e9b457329d2b2182b817e144f1e904b957745c839" },
        ],
        sources: vec![
            Source {
                label: "HF Mirror",
                base_url: "https://hf-mirror.com/Xenova/bge-large-zh-v1.5/resolve/main".to_string(),
            },
            Source {
                label: "ModelScope",
                base_url: "https://modelscope.cn/models/Xenova/bge-large-zh-v1.5/resolve/master".to_string(),
            },
        ],
        system_package: None,
    }
}

/// BGE-base-zh-v1.5 (768-dim) — optional offline semantic embeddings. Part of
/// the single-choice BGE group (the dependency center shows a dimension
/// dropdown); `bge-large` is the recommended default.
fn bge_base() -> DepDef {
    DepDef {
        id: "bge-base",
        name: "BGE-base 本地语义模型（768维，离线向量）",
        recommended: false,
        size_bytes: 407_392_295, // model.onnx + tokenizer.json
        hint: "离线语义搜索 / embedding（768 维，约 407MB）",
        files: &[
            FileSpec { remote: "onnx/model.onnx", local: "model.onnx", sha256: "5e5619f7cca7380b824d329c157dba10bee7cc00d0c139e82fdb7906051b8e4f" },
            FileSpec { remote: "tokenizer.json", local: "tokenizer.json", sha256: "7dfbf1966ebf99d471c3796e9b457329d2b2182b817e144f1e904b957745c839" },
        ],
        sources: vec![
            Source {
                label: "HF Mirror",
                base_url: "https://hf-mirror.com/Xenova/bge-base-zh-v1.5/resolve/main".to_string(),
            },
            Source {
                label: "ModelScope",
                base_url: "https://modelscope.cn/models/Xenova/bge-base-zh-v1.5/resolve/master".to_string(),
            },
        ],
        system_package: None,
    }
}

/// BGE-small-zh-v1.5 (512-dim) — optional lightweight offline embeddings.
/// Part of the single-choice BGE group.
fn bge_small() -> DepDef {
    DepDef {
        id: "bge-small",
        name: "BGE-small 本地语义模型（512维，离线向量）",
        recommended: false,
        size_bytes: 95_291_002, // model.onnx + tokenizer.json
        hint: "离线语义搜索 / embedding（512 维，约 95MB）",
        files: &[
            FileSpec { remote: "onnx/model.onnx", local: "model.onnx", sha256: "69a0b846f4f116b5e6aabf9546ea6754d02264f3211a13a1bd69b31b8040749a" },
            FileSpec { remote: "tokenizer.json", local: "tokenizer.json", sha256: "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26" },
        ],
        sources: vec![
            Source {
                label: "HF Mirror",
                base_url: "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main".to_string(),
            },
            Source {
                label: "ModelScope",
                base_url: "https://modelscope.cn/models/Xenova/bge-small-zh-v1.5/resolve/master".to_string(),
            },
        ],
        system_package: None,
    }
}

/// Built-in local rerankers (cross-encoders). Not recommended — reranking is
/// opt-in. The dependency center renders them as a single-choice dropdown, and
/// the dep id **is** the model directory name so `local:<id>` resolves directly.
fn rerank_defs() -> Vec<DepDef> {
    vec![
        DepDef {
            id: "bge-reranker-base",
            name: "BGE-Reranker-Base 本地重排模型（中英）",
            recommended: false,
            size_bytes: 1_129_557_667, // model.onnx 1,112,459,588 + tokenizer 17,098,079
            hint: "对检索结果二次精排（cross-encoder，中英，约 1.1GB）",
            files: &[
                FileSpec { remote: "onnx/model.onnx", local: "model.onnx", sha256: "15b9a8c3da82eddf263df571281166e00e9308fe19d077084b642ebfcaf06d2b" },
                FileSpec { remote: "tokenizer.json", local: "tokenizer.json", sha256: "48564c5c7d3fa64d85d95e65414a542385f88b0f128fd8d4163fd7a57f2be05c" },
            ],
            sources: rerank_sources("bge-reranker-base"),
            system_package: None,
        },
        DepDef {
            id: "ms-marco-MiniLM-L-6-v2",
            name: "MS-Marco MiniLM-L6 本地重排模型（英文，快）",
            recommended: false,
            size_bytes: 91_703_511, // model.onnx 90,992,115 + tokenizer 711,396
            hint: "对检索结果二次精排（cross-encoder，英文，约 90MB）",
            files: &[
                FileSpec { remote: "onnx/model.onnx", local: "model.onnx", sha256: "c623d0bcb99f4622beb413eaef00cfbe5db20df9f1dd982da4b4f26022881870" },
                FileSpec { remote: "tokenizer.json", local: "tokenizer.json", sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66" },
            ],
            sources: rerank_sources("ms-marco-MiniLM-L-6-v2"),
            system_package: None,
        },
    ]
}

fn rerank_sources(repo: &str) -> Vec<Source> {
    vec![
        Source {
            label: "HF Mirror",
            base_url: format!("https://hf-mirror.com/Xenova/{repo}/resolve/main"),
        },
        Source {
            label: "ModelScope",
            base_url: format!("https://modelscope.cn/models/Xenova/{repo}/resolve/master"),
        },
    ]
}

/// FunASR-Nano (sherpa-onnx int8) — offline audio transcription (~850MB).
fn funasr() -> DepDef {
    DepDef {
        id: "funasr",
        name: "FunASR 语音转写模型",
        recommended: true,
        size_bytes: 850 * 1024 * 1024,
        hint: "音频转文字（约 850MB）",
        files: &[
            FileSpec { remote: "funasr-encoder_adaptor.int8.onnx", local: "encoder_adaptor.int8.onnx", sha256: "f36dea2e30fbc33b5db1d7a7265cc976c5e5586c77b042d5adb1ad27c72db422" },
            FileSpec { remote: "funasr-llm.int8.onnx", local: "llm.int8.onnx", sha256: "dfbf9aa3be41bccc257587f151e15c63fbe1b549f2b517f5ccd5bdce3bf4322a" },
            FileSpec { remote: "funasr-embedding.int8.onnx", local: "embedding.int8.onnx", sha256: "95e61cd0c9c3b9543339a4cf973c95c116815e745ccc1e0285cbd81f76d18644" },
            FileSpec { remote: "funasr-tokenizer.json", local: "Qwen3-0.6B/tokenizer.json", sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4" },
        ],
        sources: vec![Source {
            label: "GitHub Releases",
            base_url: gh_base(),
        }],
        system_package: None,
    }
}

/// FFmpeg — audio decode. System-provided; installed via platform package
/// manager when missing (winget/brew/apt), not downloaded from GitHub.
fn ffmpeg() -> DepDef {
    DepDef {
        id: "ffmpeg",
        name: "FFmpeg（音频解码）",
        recommended: false,
        size_bytes: 0,
        hint: "音频文件解码，点击安装自动走 winget/brew/apt",
        files: &[],
        sources: vec![],
        system_package: Some(SystemPackage {
            winget: Some("Gyan.FFmpeg"),
            brew: Some("ffmpeg"),
            apt: Some("ffmpeg"),
        }),
    }
}

/// Poppler (pdftoppm) — render PDF pages for OCR fallback. System-provided.
fn poppler() -> DepDef {
    DepDef {
        id: "poppler",
        name: "Poppler（PDF 渲染）",
        recommended: false,
        size_bytes: 0,
        hint: "扫描版 PDF 渲染，点击安装自动走 winget/brew/apt",
        files: &[],
        sources: vec![],
        system_package: Some(SystemPackage {
            winget: Some("oschwartz10612.Poppler"),
            brew: Some("poppler"),
            apt: Some("poppler-utils"),
        }),
    }
}

/// Install directory for a dep under `data_dir`.
pub fn install_dir(def: &DepDef, data_dir: &Path) -> std::path::PathBuf {
    let sub = match def.id {
        "paddleocr" => "ppocrv5",
        "bge-large" => "bge-large-zh-v1.5",
        "bge-base" => "bge-base-zh-v1.5",
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
        "poppler" => return crate::extractor::pdf::poppler_available(),
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

                // Every published asset must carry a 64-hex-char SHA-256 so the
                // downloader can verify integrity.
                for f in def.files {
                    assert_eq!(
                        f.sha256.len(),
                        64,
                        "dep {} file {} sha256 must be 64 hex chars",
                        def.id,
                        f.remote
                    );
                    assert!(
                        f.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                        "dep {} file {} sha256 must be hex",
                        def.id,
                        f.remote
                    );
                }
            }
        }
    }

    #[test]
    fn paddleocr_dev_tree_sha256_matches_catalog() {
        let _g = env_lock();
        // The git-tracked dev models are the same bytes as the published
        // assets; verify the catalog digest really matches them so a typo in
        // catalog.rs is caught (guarded: dev files may be absent in CI).
        let def = all().into_iter().find(|d| d.id == "paddleocr").unwrap();
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models").join("ppocrv5");
        for f in def.files {
            let p = dev.join(f.local);
            if !p.is_file() {
                continue;
            }
            let digest = {
                use sha2::{Digest, Sha256};
                let bytes = std::fs::read(&p).unwrap();
                format!("{:x}", Sha256::digest(&bytes))
            };
            assert_eq!(digest, f.sha256, "dev tree {} digest must match catalog", f.local);
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
                "bge-large" => "bge-large-zh-v1.5",
                "bge-base" => "bge-base-zh-v1.5",
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
