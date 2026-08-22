//! Directory configuration CRUD operations.
//!
//! Manages per-directory scan settings: search path, OCR language, file filters,
//! and recursion preference.

use anyhow::{Context, Result};
use rusqlite::Connection;
use uuid::Uuid;

/// A directory configuration record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DirConfig {
    pub id: String,
    pub path: String,
    pub alias: Option<String>,
    pub ocr_lang: String,
    pub exclude_patterns: Option<String>,
    pub include_exts: Option<String>,
    pub recursive: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Fields that can be updated on an existing directory config.
/// Only `Some` fields are written; `None` fields are left unchanged.
#[derive(Debug, Clone, Default)]
pub struct DirUpdate {
    pub alias: Option<String>,
    pub ocr_lang: Option<String>,
    pub exclude_patterns: Option<String>,
    pub include_exts: Option<String>,
    pub recursive: Option<bool>,
}

/// Add a new directory configuration. Returns the created config.
///
/// `path` must be unique. `ocr_lang` defaults to `"eng"` when `None`.
pub fn add_dir(
    conn: &Connection,
    path: &str,
    alias: Option<&str>,
    ocr_lang: Option<&str>,
    exclude_patterns: Option<&str>,
    include_exts: Option<&str>,
    recursive: bool,
) -> Result<DirConfig> {
    let now = chrono::Utc::now().timestamp();
    let id = Uuid::new_v4().to_string();
    let lang = ocr_lang.unwrap_or("eng");
    let rec_i64: i64 = if recursive { 1 } else { 0 };

    conn.execute(
        "INSERT INTO dir_config (id, path, alias, ocr_lang, exclude_patterns, include_exts, recursive, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        rusqlite::params![id, path, alias, lang, exclude_patterns, include_exts, rec_i64, now],
    )
    .context("failed to add directory config")?;

Ok(DirConfig {
        id,
        path: path.to_string(),
        alias: alias.map(String::from),
        ocr_lang: lang.to_string(),
        exclude_patterns: exclude_patterns.map(String::from),
        include_exts: include_exts.map(String::from),
        recursive,
        created_at: now,
        updated_at: now,
    })
}

/// Remove a directory configuration by id.
pub fn remove_dir(conn: &Connection, dir_id: &str) -> Result<()> {
    let rows = conn
        .execute("DELETE FROM dir_config WHERE id = ?1", rusqlite::params![dir_id])
        .context("failed to remove directory config")?;
    if rows == 0 {
        anyhow::bail!("directory config not found: {dir_id}");
    }
    Ok(())
}

/// List all directory configurations, ordered by path.
pub fn list_dirs(conn: &Connection) -> Result<Vec<DirConfig>> {
    let mut stmt = conn
        .prepare("SELECT id, path, alias, ocr_lang, exclude_patterns, include_exts, recursive, created_at, updated_at \
                   FROM dir_config ORDER BY path")
        .context("failed to prepare list_dirs")?;
    let rows = stmt
        .query_map([], row_to_dir_config)
        .context("failed to query dir configs")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to collect dir configs")
}

/// Get a single directory configuration by id.
pub fn get_dir(conn: &Connection, dir_id: &str) -> Result<Option<DirConfig>> {
    let mut stmt = conn
        .prepare("SELECT id, path, alias, ocr_lang, exclude_patterns, include_exts, recursive, created_at, updated_at \
                   FROM dir_config WHERE id = ?1")
        .context("failed to prepare get_dir")?;
    let mut rows = stmt
        .query_map(rusqlite::params![dir_id], row_to_dir_config)
        .context("failed to query dir config")?;
    Ok(rows.next().transpose().context("failed to read dir config row")?)
}

/// Update selected fields of a directory configuration.
pub fn update_dir(conn: &Connection, dir_id: &str, updates: DirUpdate) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let mut sets = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(v) = updates.alias {
        sets.push("alias = ?");
        params.push(Box::new(v));
    }
    if let Some(v) = updates.ocr_lang {
        sets.push("ocr_lang = ?");
        params.push(Box::new(v));
    }
    if let Some(v) = updates.exclude_patterns {
        sets.push("exclude_patterns = ?");
        params.push(Box::new(v));
    }
    if let Some(v) = updates.include_exts {
        sets.push("include_exts = ?");
        params.push(Box::new(v));
    }
    if let Some(v) = updates.recursive {
        sets.push("recursive = ?");
        params.push(Box::new(v as i64));
    }

    if sets.is_empty() {
        return Ok(());
    }

    sets.push("updated_at = ?");
    params.push(Box::new(now));
    params.push(Box::new(dir_id.to_string()));

    let sql = format!(
        "UPDATE dir_config SET {} WHERE id = ?",
        sets.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = conn
        .execute(&sql, param_refs.as_slice())
        .context("failed to update directory config")?;
    if rows == 0 {
        anyhow::bail!("directory config not found: {dir_id}");
    }
    Ok(())
}

fn row_to_dir_config(row: &rusqlite::Row) -> rusqlite::Result<DirConfig> {
    Ok(DirConfig {
        id: row.get("id")?,
        path: row.get("path")?,
        alias: row.get("alias")?,
        ocr_lang: row.get("ocr_lang")?,
        exclude_patterns: row.get("exclude_patterns")?,
        include_exts: row.get("include_exts")?,
        recursive: row.get::<_, i64>("recursive")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn test_add_and_list() {
        let conn = setup_conn();
        add_dir(&conn, "/home/docs", Some("Docs"), None, None, None, true).unwrap();
        let dirs = list_dirs(&conn).unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].path, "/home/docs");
        assert_eq!(dirs[0].alias.as_deref(), Some("Docs"));
        assert_eq!(dirs[0].ocr_lang, "eng");
        assert!(dirs[0].recursive);
    }


    #[test]
    fn test_add_custom_ocr() {
        let conn = setup_conn();
        add_dir(&conn, "/home/ocr", None, Some("chi_sim"), None, None, false).unwrap();
        let d = get_dir(&conn, &list_dirs(&conn).unwrap()[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(d.ocr_lang, "chi_sim");
        assert!(!d.recursive);
    }

    #[test]
    fn test_remove_dir() {
        let conn = setup_conn();
        add_dir(&conn, "/home/docs", None, None, None, None, true).unwrap();
        let dirs = list_dirs(&conn).unwrap();
        remove_dir(&conn, &dirs[0].id).unwrap();
        assert!(list_dirs(&conn).unwrap().is_empty());
    }

    #[test]
    fn test_remove_missing_is_error() {
        let conn = setup_conn();
        assert!(remove_dir(&conn, "nonexistent").is_err());
    }

    #[test]
    fn test_update_dir() {
        let conn = setup_conn();
        add_dir(&conn, "/home/docs", Some("Old"), None, None, None, true).unwrap();
        let dirs = list_dirs(&conn).unwrap();
        let id = &dirs[0].id;

        update_dir(
            &conn,
            id,
            DirUpdate {
                alias: Some("New".into()),
                ocr_lang: Some("fra".into()),
                recursive: Some(false),
                ..Default::default()
            },
        )
        .unwrap();

        let d = get_dir(&conn, id).unwrap().unwrap();
        assert_eq!(d.alias, Some("New".into()));
        assert_eq!(d.ocr_lang, "fra");
        assert!(!d.recursive);
    }

    #[test]
    fn test_update_dir_empty_is_noop() {
        let conn = setup_conn();
        add_dir(&conn, "/home/docs", None, None, None, None, true).unwrap();
        let id = list_dirs(&conn).unwrap()[0].id.clone();
        update_dir(&conn, &id, DirUpdate::default()).unwrap();
        let d = get_dir(&conn, &id).unwrap().unwrap();
        assert!(d.recursive);
    }

    #[test]
    fn test_add_dir_path_uniqueness() {
        let conn = setup_conn();
        add_dir(&conn, "/home/docs", None, None, None, None, true).unwrap();
        assert!(add_dir(&conn, "/home/docs", None, None, None, None, true).is_err());
    }
}
