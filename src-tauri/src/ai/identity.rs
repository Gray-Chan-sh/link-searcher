//! 向量空间身份：给每条存储的向量盖章"它由哪个模型空间生成"。
//!
//! ## 为什么需要
//!
//! v14 之前，`doc_embeddings` / `chunk_embeddings` 里只有 `md5 / dim / vector /
//! updated_at` —— **没有任何"这条向量是谁生成的"的事实**。于是切换 embedding
//! 模型后，判断语料是否仍与当前模型同空间只能靠两件事：
//!
//! 1. **维度**：同维就判"一致"。`bge-m3` 与另一个网关上的 `bge-m3` 同为 1024 维，
//!    这条判据对它们完全无感（2026-09-29 的实际事故形态）。
//! 2. **事后推断**：翻日志、算相邻 chunk 的 cosine。2026-11-01 那次审计就是这么做
//!    的 —— 能得出结论，但不是系统保证的，而且下次不一定还有运气。
//!
//! ## 指纹是什么
//!
//! ```text
//! fp = sha256(base_url | model_name | preproc_version | extra)[..16]
//! ```
//!
//! - `base_url`：**故意包含**。"同名模型在不同服务器上不是同一个空间"是这里最
//!   需要抓住的形态；代价是"只是搬了台机器"也会被判为不同 —— 这正是 v14 的
//!   既定取舍：宁可在切换时问一句"要不要重建"，也不要静默混用。
//! - `model_name`：模型标识。
//! - `preproc_version`：预处理版本，见 [`PREPROC_VERSION`]。
//! - `extra`：本地模型额外混入模型目录哈希（权重 / 分词器 / 配置全在里面）。
//!
//! **不包含** provider UUID —— 删掉再加回同一个网关（新 UUID、同 base_url、同
//! 模型名）不该被判为换了模型。也不包含 api_key / 超时 / 并发度这类与向量无关的项。
//!
//! ## 边界
//!
//! 指纹记录的是**声明的身份**，不是**验证过的身份**。网关若在同一个 URL 后面
//! 悄悄换了权重，指纹看不出来。这一点无法在离线状态下解决 —— 只能靠"无法确认"
//! 的诚实提示（见 `commands/config.rs::set_active_model`）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

use crate::config::AppConfig;

/// 预处理版本。
///
/// **凡改动会影响向量数值的代码，必须 bump 这个常量**：
/// `local_embed::QUERY_PREFIX`、`local_embed::MAX_SEQ_LEN`、`ai::EMBED_MAX_CHARS`、
/// 池化方式、是否 L2 归一化。
///
/// 它保证"同样的模型 + 同样的预处理"才得到同一个指纹：改了预处理就自动让全库
/// 向量变成 `Stale`（要求重建），而不是静默换语义 —— 后者是 2026-09 那次审计
/// 里 P1-1 / P1-2 的形态（本地查询带前缀、远程不带；本地截 512 token、远程截
/// 2000 字符）。
///
/// 防漏改的兜底是 `local_embed` 的金标准测试：改了预处理但不 bump 它，测试会红。
pub const PREPROC_VERSION: u32 = 1;

/// 归一化 base_url：去首尾空白、去尾斜杠、scheme 与 host 小写（path 保持原样）。
///
/// `http://Host:8000/v1/` 与 `http://host:8000/v1` 必须得到同一个指纹，否则用户
/// 只是顺手删了个斜杠就会触发"要不要重建"的提示。
pub fn normalize_base_url(url: &str) -> String {
    let trimmed = url.trim();
    let without_slash = trimmed.strip_suffix('/').unwrap_or(trimmed);
    match without_slash.find("://") {
        Some(idx) => {
            let (scheme, rest) = without_slash.split_at(idx + 3);
            let (authority, path) = match rest.find('/') {
                Some(p) => (&rest[..p], &rest[p..]),
                None => (rest, ""),
            };
            format!(
                "{}{}{}",
                scheme.to_ascii_lowercase(),
                authority.to_ascii_lowercase(),
                path
            )
        }
        None => without_slash.to_ascii_lowercase(),
    }
}

/// 由已知部件算指纹。16 位十六进制（64 bit）—— 碰撞概率对本用途足够。
pub fn fingerprint(base_url: &str, model_name: &str, extra: &str) -> String {
    fingerprint_v(PREPROC_VERSION, base_url, model_name, extra)
}

fn fingerprint_v(version: u32, base_url: &str, model_name: &str, extra: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_base_url(base_url).as_bytes());
    hasher.update([0]);
    hasher.update(model_name.trim().as_bytes());
    hasher.update([0]);
    hasher.update(version.to_le_bytes());
    hasher.update([0]);
    hasher.update(extra.as_bytes());
    hex16(&hasher.finalize())
}

fn hex16(digest: &[u8]) -> String {
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// 某个模型 id 的指纹。`None` = 未配置 / 指向已删除的 provider。
///
/// - `local:<name>` → 混入模型目录哈希
/// - `<provider-uuid>:<model-id>` → 混入该 provider 的 base_url
pub fn model_fp(cfg: &AppConfig, model_id: &str) -> Option<String> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return None;
    }
    if let Some(name) = crate::ai::local_embed::local_model_dir_name(model_id) {
        let extra = local_model_dir_hash(&cfg.data_dir, name).unwrap_or_default();
        return Some(fingerprint("local://", name, &extra));
    }
    let (provider_id, remote_model) = model_id.split_once(':')?;
    let provider = cfg.providers.iter().find(|p| p.id == provider_id)?;
    Some(fingerprint(&provider.base_url, remote_model, ""))
}

/// 当前 active embedding 模型的指纹（读一次配置）。
pub fn active_embedding_fp() -> Option<String> {
    let cfg = crate::config::load_config();
    model_fp(&cfg, &cfg.active_embedding_model_id)
}

/// 本地模型目录的 Merkle 式哈希。
///
/// **黑名单策略**：哈希目录下全部文件，只排除明显与向量无关的（`*.md`、`.git*`、
/// `LICENSE`）。白名单（只哈希已知文件）漏掉任何一个影响向量的文件都会造成
/// 假阴性 —— 静默用错向量比多跑一次重建糟得多。
///
/// 缓存签名：`(文件数, 总字节数, 最新 mtime 纳秒)` → 结果，避免每次启动都重读
/// 1–2 GB 权重。
///
/// **必须是纳秒级 mtime**：秒级精度下"同一秒内用另一个同大小的权重文件替换"
/// 会得到相同的签名 → 缓存返回旧哈希 → 指纹不变 → 换了模型却判"一致"。
/// 这正是本模块要防的那类静默错误，单元测试也直接踩到了它。
///
/// 目录不存在时返回空串（指纹仍可算，只是少了这一层保护）。
pub fn local_model_dir_hash(data_dir: &Path, model_name: &str) -> std::io::Result<String> {
    let dir = data_dir.join("models").join(model_name);
    if !dir.is_dir() {
        return Ok(String::new());
    }

    let mut files: Vec<(PathBuf, String, u64, u128)> = Vec::new();
    collect_files(&dir, &dir, &mut files)?;
    files.sort_by(|a, b| a.1.cmp(&b.1));

    let sig = DirSignature {
        files: files.len() as u64,
        total_bytes: files.iter().map(|f| f.2).sum(),
        max_mtime_ns: files.iter().map(|f| f.3).max().unwrap_or(0),
    };

    if let Some(hit) = {
        let cache = dir_hash_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(&dir)
            .filter(|(s, _)| *s == sig)
            .map(|(_, h)| h.clone())
    } {
        return Ok(hit);
    }

    let mut hasher = Sha256::new();
    for (path, rel, len, _) in &files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(len.to_le_bytes());
        let mut file = std::fs::File::open(path)?;
        std::io::copy(&mut file, &mut hasher)?;
        hasher.update([0xff]);
    }
    let hash = hex16(&hasher.finalize());

    let mut cache = dir_hash_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(dir, (sig, hash.clone()));
    Ok(hash)
}

/// 目录"形状"，用于判断缓存的哈希是否还成立。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DirSignature {
    files: u64,
    total_bytes: u64,
    max_mtime_ns: u128,
}

fn dir_hash_cache() -> &'static Mutex<HashMap<PathBuf, (DirSignature, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (DirSignature, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, String, u64, u128)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_irrelevant(&rel) {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_dir() {
            collect_files(root, &path, out)?;
        } else if meta.is_file() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            out.push((path, rel, meta.len(), mtime));
        }
    }
    Ok(())
}

fn is_irrelevant(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name.ends_with(".md")
        || name == ".git"
        || name == ".gitattributes"
        || name == ".gitignore"
        || name == ".ds_store"
        || name.starts_with("license")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_trailing_slash_and_host_case() {
        assert_eq!(
            normalize_base_url("  HTTP://Host:8000/v1/  "),
            "http://host:8000/v1"
        );
        assert_eq!(
            normalize_base_url("http://host:8000/v1"),
            normalize_base_url("http://host:8000/v1/")
        );
    }

    #[test]
    fn keeps_path_case() {
        // path 区分大小写，不能整体小写。
        assert_eq!(
            normalize_base_url("http://Host:8000/V1"),
            "http://host:8000/V1"
        );
    }

    #[test]
    fn fingerprint_tracks_endpoint_and_model_name() {
        let a = fingerprint("http://a:8000/v1", "bge-m3", "");
        let b = fingerprint("http://b:8000/v1", "bge-m3", "");
        let c = fingerprint("http://a:8000/v1", "bge-large-zh-v1.5", "");
        assert_ne!(a, b, "换网关必须换指纹");
        assert_ne!(a, c, "换模型名必须换指纹");
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn fingerprint_ignores_trailing_slash() {
        assert_eq!(
            fingerprint("http://a:8000/v1/", "bge-m3", ""),
            fingerprint("http://a:8000/v1", "bge-m3", "")
        );
    }

    #[test]
    fn preproc_version_bump_changes_fingerprint() {
        let v1 = fingerprint_v(1, "http://a/v1", "bge-m3", "");
        let v2 = fingerprint_v(2, "http://a/v1", "bge-m3", "");
        assert_ne!(v1, v2, "bump 预处理版本必须让全库变成 stale");
    }

    fn temp_model_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("ls_ident_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn local_dir_hash_is_stable_and_content_sensitive() {
        let root = temp_model_dir("hash");
        let models = root.join("models").join("bge");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("model.onnx"), b"weights-v1").unwrap();
        std::fs::write(models.join("tokenizer.json"), b"{}").unwrap();

        let a = local_model_dir_hash(&root, "bge").unwrap();
        let b = local_model_dir_hash(&root, "bge").unwrap();
        assert_eq!(a, b, "同目录两次必须相同");
        assert_eq!(a.len(), 16);

        // Deliberately change the byte length too: Windows may give two quick
        // writes the same coarse mtime, and the directory-hash cache signature
        // includes (file count, total bytes, latest mtime).
        std::fs::write(models.join("model.onnx"), b"weights-v2-long").unwrap();
        let c = local_model_dir_hash(&root, "bge").unwrap();
        assert_ne!(a, c, "权重变了必须变");

        // 黑名单：README 之类的无关文件不该影响指纹。
        std::fs::write(models.join("README.md"), b"hello").unwrap();
        let d = local_model_dir_hash(&root, "bge").unwrap();
        assert_eq!(c, d, "README.md 不该影响指纹");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_local_dir_hashes_to_empty() {
        let root = temp_model_dir("missing");
        assert_eq!(local_model_dir_hash(&root, "nope").unwrap(), "");
        let _ = std::fs::remove_dir_all(&root);
    }
}
