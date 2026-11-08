//! 依赖（模型）的"已安装版本"状态与版本化目录。
//!
//! 目的：模型可以在**不发版**的情况下换版本（远端清单驱动），且失败可回退。
//!
//! 目录布局（以 paddleocr 为例）：
//! ```text
//! <data_dir>/models/paddleocr/
//! ├─ versions/
//! │  ├─ v5-mobile-1/{det.onnx, rec.onnx, ppocrv5_dict.txt}
//! │  └─ v6-tiny-1/{det.onnx, rec.onnx, dict.txt}
//! └─ state.json          {"deps":{"paddleocr":{"current":"v6-tiny-1","previous":"v5-mobile-1"}}}
//! ```
//!
//! 兼容：老版本把文件平铺在 `<data_dir>/models/ppocrv5/`（BGE 等其它依赖仍是平铺）。
//! `migrate_legacy_flat` 在检测到"平铺且 sha256 与内置清单一致"时把它登记为内置版本，
//! 避免老用户升级后重复下载 20MB。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 单条依赖的版本状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    /// 当前生效的版本目录名。
    pub current: String,
    /// 上一个版本（用于一键回退）。
    #[serde(default)]
    pub previous: Option<String>,
    /// 最近一次切换的 unix 秒。
    #[serde(default)]
    pub updated_at: i64,
}

/// `state.json` 的内容。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub deps: BTreeMap<String, Entry>,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(".dep-state.json")
}

/// 某个依赖的版本根目录：`<data_dir>/models/<dep>/versions`。
pub fn versions_root(data_dir: &Path, dep: &str) -> PathBuf {
    data_dir.join("models").join(dep).join("versions")
}

/// 指定版本的目录：`<data_dir>/models/<dep>/versions/<version>`。
pub fn version_dir(data_dir: &Path, dep: &str, version: &str) -> PathBuf {
    versions_root(data_dir, dep).join(version)
}

pub fn load(data_dir: &Path) -> StateFile {
    let p = state_path(data_dir);
    match fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            log::warn!("[DEPS] 状态文件解析失败（按空处理）: {e} ({p:?})");
            StateFile::default()
        }),
        Err(_) => StateFile::default(),
    }
}

/// 原子写：先写 `.tmp` 再 rename，避免半截文件让下次启动读到空状态。
pub fn save(data_dir: &Path, state: &StateFile) -> Result<(), String> {
    let p = state_path(data_dir);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建状态目录失败: {e}"))?;
    }
    let tmp = p.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(state).map_err(|e| format!("序列化状态失败: {e}"))?;
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("写状态失败: {e}"))?;
        f.write_all(body.as_bytes()).map_err(|e| format!("写状态失败: {e}"))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, &p).map_err(|e| format!("提交状态失败: {e}"))?;
    Ok(())
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 目录里是否至少有 `expected` 个非空文件（便宜的存在性检查，
/// 真正的完整性仍由下载层的 sha256 负责）。
fn dir_ready(dir: &Path, expected: usize) -> bool {
    let Ok(rd) = fs::read_dir(dir) else { return false };
    let mut n = 0usize;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && e.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            n += 1;
        }
    }
    n >= expected
}

/// 当前生效版本的目录（不存在或文件不全则返回 None）。
pub fn current_dir(data_dir: &Path, dep: &str, expected_files: usize) -> Option<PathBuf> {
    let st = load(data_dir);
    let entry = st.deps.get(dep)?;
    if entry.current.is_empty() {
        return None;
    }
    let dir = version_dir(data_dir, dep, &entry.current);
    dir_ready(&dir, expected_files).then_some(dir)
}

/// 登记一次成功安装：旧 current 变为 previous。
pub fn register_install(
    data_dir: &Path,
    dep: &str,
    version: &str,
) -> Result<(), String> {
    let mut st = load(data_dir);
    let entry = st.deps.entry(dep.to_string()).or_default();
    if entry.current == version {
        entry.updated_at = now_ts();
        return save(data_dir, &st);
    }
    if !entry.current.is_empty() {
        entry.previous = Some(std::mem::take(&mut entry.current));
    }
    entry.current = version.to_string();
    entry.updated_at = now_ts();
    save(data_dir, &st)
}

/// 回退到上一个版本；返回新的当前版本名。
pub fn rollback(data_dir: &Path, dep: &str, expected_files: usize) -> Result<String, String> {
    let mut st = load(data_dir);
    let entry = st
        .deps
        .get_mut(dep)
        .ok_or_else(|| format!("{dep} 没有已安装版本"))?;
    let prev = entry
        .previous
        .clone()
        .ok_or_else(|| "没有可回退的上一版本".to_string())?;
    if !dir_ready(&version_dir(data_dir, dep, &prev), expected_files) {
        return Err(format!("上一版本目录不完整，无法回退: {prev}"));
    }
    let cur = std::mem::replace(&mut entry.current, prev.clone());
    entry.previous = Some(cur);
    entry.updated_at = now_ts();
    save(data_dir, &st)?;
    Ok(prev)
}

/// 只保留当前、上一版和 `keep_new`（刚装好的版本），其余目录删除。
pub fn prune(data_dir: &Path, dep: &str, keep_new: Option<&str>) {
    let st = load(data_dir);
    let entry = st.deps.get(dep);
    let root = versions_root(data_dir, dep);
    let Ok(rd) = fs::read_dir(&root) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let keep = entry.map(|en| {
            en.current == name
                || en.previous.as_deref() == Some(name.as_str())
                || keep_new == Some(name.as_str())
        });
        if keep == Some(true) {
            continue;
        }
        log::info!("[DEPS] {dep}: 清理旧版本目录 {name}");
        let _ = fs::remove_dir_all(e.path());
    }
}

/// 老布局（平铺）迁移：`<data_dir>/models/<legacy_sub>/` → `versions/<version>/`。
///
/// 只在"平铺目录里的每个文件 sha256 都与内置清单一致"时迁移 —— 不一致说明
/// 用户手动换过文件，保留原样（继续由平铺路径兜底），不做破坏性移动。
pub fn migrate_legacy_flat(
    data_dir: &Path,
    dep: &str,
    legacy_dir: &Path,
    version: &str,
    expected: &[(String, String)],
) -> bool {
    if expected.is_empty() || !legacy_dir.is_dir() {
        return false;
    }
    let st = load(data_dir);
    if st.deps.get(dep).map(|e| !e.current.is_empty()).unwrap_or(false) {
        return false;
    }
    for (name, sha) in expected {
        let p = legacy_dir.join(name);
        if !p.is_file() {
            return false;
        }
        match crate::deps::download::file_sha256(&p) {
            Ok(got) if got.eq_ignore_ascii_case(sha) => {}
            _ => return false,
        }
    }

    let dest = version_dir(data_dir, dep, version);
    if let Err(e) = fs::create_dir_all(&dest) {
        log::warn!("[DEPS] 迁移建目录失败: {e}");
        return false;
    }
    for (name, _) in expected {
        let from = legacy_dir.join(name);
        let to = dest.join(name);
        if to.is_file() {
            continue;
        }
        if let Err(e) = fs::rename(&from, &to) {
            // rename 跨盘失败时退化为复制
            if let Err(e2) = fs::copy(&from, &to) {
                log::warn!("[DEPS] 迁移 {name} 失败: {e} / {e2}");
                return false;
            }
        }
    }
    if let Err(e) = register_install(data_dir, dep, version) {
        log::warn!("[DEPS] 迁移登记失败: {e}");
        return false;
    }
    log::info!("[DEPS] {dep}: 已把平铺安装迁移为版本 {version}");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ls_depstate_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn register_and_rollback_rotates_versions() {
        let root = tmp_root("roll");
        for v in ["v1", "v2"] {
            let d = version_dir(&root, "paddleocr", v);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("det.onnx"), b"x").unwrap();
        }
        register_install(&root, "paddleocr", "v1").unwrap();
        assert_eq!(current_dir(&root, "paddleocr", 1).unwrap(), version_dir(&root, "paddleocr", "v1"));

        register_install(&root, "paddleocr", "v2").unwrap();
        let st = load(&root);
        let e = &st.deps["paddleocr"];
        assert_eq!(e.current, "v2");
        assert_eq!(e.previous.as_deref(), Some("v1"));

        let back = rollback(&root, "paddleocr", 1).unwrap();
        assert_eq!(back, "v1");
        let st = load(&root);
        assert_eq!(st.deps["paddleocr"].current, "v1");
        assert_eq!(st.deps["paddleocr"].previous.as_deref(), Some("v2"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rollback_without_previous_fails() {
        let root = tmp_root("noprev");
        register_install(&root, "paddleocr", "v1").unwrap();
        assert!(rollback(&root, "paddleocr", 1).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn current_dir_requires_files() {
        let root = tmp_root("empty");
        let d = version_dir(&root, "paddleocr", "v1");
        fs::create_dir_all(&d).unwrap();
        register_install(&root, "paddleocr", "v1").unwrap();
        assert!(current_dir(&root, "paddleocr", 1).is_none(), "空目录不算就绪");
        fs::write(d.join("det.onnx"), b"x").unwrap();
        assert!(current_dir(&root, "paddleocr", 1).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_keeps_current_and_previous() {
        let root = tmp_root("prune");
        for v in ["v1", "v2", "v3"] {
            let d = version_dir(&root, "paddleocr", v);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("det.onnx"), b"x").unwrap();
        }
        register_install(&root, "paddleocr", "v1").unwrap();
        register_install(&root, "paddleocr", "v2").unwrap();
        register_install(&root, "paddleocr", "v3").unwrap(); // current=v3 previous=v2, v1 应被清理
        prune(&root, "paddleocr", None);
        assert!(!version_dir(&root, "paddleocr", "v1").exists());
        assert!(version_dir(&root, "paddleocr", "v2").exists());
        assert!(version_dir(&root, "paddleocr", "v3").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn state_roundtrip_is_atomic_and_tolerant() {
        let root = tmp_root("io");
        let mut st = StateFile::default();
        st.deps.insert(
            "paddleocr".into(),
            Entry { current: "v9".into(), previous: None, updated_at: 1 },
        );
        save(&root, &st).unwrap();
        assert_eq!(load(&root).deps["paddleocr"].current, "v9");
        // 损坏的 JSON 不应 panic，按空状态处理
        fs::write(state_path(&root), b"{not json").unwrap();
        assert!(load(&root).deps.is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
