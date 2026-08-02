use std::path::{Component, Path, PathBuf};

/// Best-effort canonicalization: canonical path when the dir exists, otherwise
/// canonicalize the nearest existing ancestor and append the rest — this keeps
/// symlinks (e.g. /tmp → /private/tmp) consistent across both operands.
fn canonicalize_or_abs(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let mut existing = p;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) if parent != existing => {
                if let Some(name) = existing.file_name() {
                    tail.push(name.to_os_string());
                }
                existing = parent;
            }
            _ => break,
        }
    }
    let mut out = match existing.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(p)
            };
            let mut norm = PathBuf::new();
            for comp in abs.components() {
                match comp {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        norm.pop();
                    }
                    other => norm.push(other.as_os_str()),
                }
            }
            return norm;
        }
    };
    for name in tail.iter().rev() {
        out.push(name);
    }
    out
}

/// Reject `candidate` if it overlaps `data_dir` (same path, inside it, or
/// containing it). Component-aware: `/a/b` never matches `/a/bc`.
pub fn check_data_dir_overlap(data_dir: &Path, candidate: &Path) -> Result<(), String> {
    let data = canonicalize_or_abs(data_dir);
    let cand = canonicalize_or_abs(candidate);
    if cand.starts_with(&data) {
        return Err("此目录位于数据目录内，不允许监控".to_string());
    }
    if data.starts_with(&cand) {
        return Err("此目录包含数据目录，不允许监控".to_string());
    }
    Ok(())
}

/// True when `candidate` is `base` itself or a descendant.
pub fn is_within(base: &Path, candidate: &Path) -> bool {
    canonicalize_or_abs(candidate).starts_with(&canonicalize_or_abs(base))
}

/// Reject a CLI `--data-dir` that overlaps any monitored directory persisted
/// in the current (config.json) data dir's database.
pub fn check_cli_data_dir_overlap(cli: &Path) -> Result<(), String> {
    let config = crate::config::load_config();
    let db_path = config.data_dir.join("data.db");
    if !db_path.exists() {
        return Ok(());
    }
    let conn = rusqlite::Connection::open(&db_path).map_err(|e| format!("无法打开数据库: {e}"))?;
    let dirs = crate::db::dir_config::list_dirs(&conn).map_err(|e| format!("{e}"))?;
    for dir in &dirs {
        if check_data_dir_overlap(Path::new(&dir.path), cli).is_err() {
            return Err(format!(
                "数据目录 {} 与监控目录 '{}' 存在交叠，不允许使用该数据目录",
                cli.display(),
                dir.path
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ls_ov_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_check_data_dir_overlap() {
        let tmp = tmp_dir("data");
        let data = tmp.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let inside = data.join("sub");
        std::fs::create_dir_all(&inside).unwrap();
        let sibling = tmp.join("other");
        std::fs::create_dir_all(&sibling).unwrap();

        assert!(check_data_dir_overlap(&data, &inside).is_err(), "inside data dir");
        assert!(check_data_dir_overlap(&data, &data).is_err(), "same path");
        assert!(check_data_dir_overlap(&data, &tmp).is_err(), "parent contains data");
        assert!(check_data_dir_overlap(&data, &sibling).is_ok(), "unrelated sibling");

        // Component-aware: prefix must not match partial names.
        let sibling_prefix = tmp.join("data_other");
        std::fs::create_dir_all(&sibling_prefix).unwrap();
        assert!(check_data_dir_overlap(&data, &sibling_prefix).is_ok(), "name prefix only");

        // Non-existent candidate inside data dir → lexical fallback still rejects.
        let not_yet = data.join("future").join("deep");
        assert!(check_data_dir_overlap(&data, &not_yet).is_err(), "non-existent inside");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_is_within() {
        let tmp = tmp_dir("within");
        let base = tmp.join("base");
        std::fs::create_dir_all(&base).unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(is_within(&base, &sub));
        assert!(is_within(&base, &base));
        assert!(!is_within(&base, &tmp));
        assert!(!is_within(&base, &tmp.join("outside")));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
