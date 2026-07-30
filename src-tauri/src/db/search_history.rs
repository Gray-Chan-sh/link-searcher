//! Search history read/write operations.
//!
//! Records user queries, allows pinning important searches, and supports
//! clearing the history.

use anyhow::{Context, Result};
use rusqlite::Connection;
use uuid::Uuid;

/// A single search history entry.
#[derive(Debug, Clone)]
pub struct SearchHistoryEntry {
    pub id: String,
    pub query: String,
    pub dir_ids: Option<String>,
    pub filters: Option<String>,
    pub result_count: u64,
    pub pinned: bool,
    pub created_at: i64,
}

/// Record a new search query. Returns the entry id.
pub fn add_entry(
    conn: &Connection,
    query: &str,
    dir_ids: Option<&str>,
    filters: Option<&str>,
    result_count: u64,
) -> Result<String> {
    let now = chrono::Utc::now().timestamp_millis();
    let id = Uuid::new_v4().to_string();
    let rc = result_count as i64;

    conn.execute(
        "INSERT INTO search_history (id, query, dir_ids, filters, result_count, pinned, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
        rusqlite::params![id, query, dir_ids, filters, rc, now],
    )
    .context("failed to add search history entry")?;

    Ok(id)
}

/// List recent search history entries, most recent first.
///
/// Pinned entries are always included regardless of recency.
pub fn list_recent(conn: &Connection, limit: usize) -> Result<Vec<SearchHistoryEntry>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, query, dir_ids, filters, result_count, pinned, created_at \
             FROM search_history ORDER BY created_at DESC LIMIT ?1",
        )
        .context("failed to prepare list_recent")?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], row_to_entry)
        .context("failed to query search history")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to collect search history entries")
}

/// Pin a history entry so it surfaces first in queries.
pub fn pin_entry(conn: &Connection, entry_id: &str) -> Result<()> {
    let rows = conn
        .execute(
            "UPDATE search_history SET pinned = 1 WHERE id = ?1",
            rusqlite::params![entry_id],
        )
        .context("failed to pin search history entry")?;
    if rows == 0 {
        anyhow::bail!("search history entry not found: {entry_id}");
    }
    Ok(())
}

/// Delete a single history entry by id.
pub fn delete_entry(conn: &Connection, entry_id: &str) -> Result<()> {
    let rows = conn
        .execute(
            "DELETE FROM search_history WHERE id = ?1",
            rusqlite::params![entry_id],
        )
        .context("failed to delete search history entry")?;
    if rows == 0 {
        anyhow::bail!("search history entry not found: {entry_id}");
    }
    Ok(())
}

/// Delete all search history entries.
pub fn clear_history(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM search_history", [])
        .context("failed to clear search history")?;
    Ok(())
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<SearchHistoryEntry> {
    Ok(SearchHistoryEntry {
        id: row.get("id")?,
        query: row.get("query")?,
        dir_ids: row.get("dir_ids")?,
        filters: row.get("filters")?,
        result_count: row.get::<_, i64>("result_count")? as u64,
        pinned: row.get::<_, i64>("pinned")? != 0,
        created_at: row.get("created_at")?,
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
        add_entry(&conn, "hello world", None, None, 5).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        add_entry(&conn, "foo bar", Some("d1,d2"), Some("ext:pdf"), 3).unwrap();

        let entries = list_recent(&conn, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].query, "foo bar");
        assert_eq!(entries[1].query, "hello world");
    }

    #[test]
    fn test_list_respects_limit() {
        let conn = setup_conn();
        for i in 0..5 {
            add_entry(&conn, &format!("query {i}"), None, None, 0).unwrap();
        }
        assert_eq!(list_recent(&conn, 3).unwrap().len(), 3);
    }

    #[test]
    fn test_pin_entry() {
        let conn = setup_conn();
        let id = add_entry(&conn, "important", None, None, 0).unwrap();
        pin_entry(&conn, &id).unwrap();

        let entries = list_recent(&conn, 10).unwrap();
        let entry = entries.iter().find(|e| e.id == id).unwrap();
        assert!(entry.pinned);
    }

    #[test]
    fn test_delete_entry() {
        let conn = setup_conn();
        let id = add_entry(&conn, "delete me", None, None, 0).unwrap();
        delete_entry(&conn, &id).unwrap();
        assert!(list_recent(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn test_clear_history() {
        let conn = setup_conn();
        add_entry(&conn, "q1", None, None, 0).unwrap();
        add_entry(&conn, "q2", None, None, 0).unwrap();
        clear_history(&conn).unwrap();
        assert!(list_recent(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn test_delete_missing_is_error() {
        let conn = setup_conn();
        assert!(delete_entry(&conn, "nonexistent").is_err());
    }
}