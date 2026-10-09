//! Cross-document watermark fragment dictionary (`watermark_tokens` table).
//!
//! A fragment that shows up in many *different* documents is almost certainly a
//! stamp/watermark, not real content. The indexer loads this dictionary before
//! extraction (so detection can use it) and records fragments found afterwards.
//! Only fragments seen in **two or more** documents are loaded, which keeps a
//! one-off legitimate token from being treated as a watermark.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

/// Minimum number of distinct documents a fragment must appear in to be
/// considered a watermark fragment.
const MIN_DOCS: i64 = 2;

/// Load the corpus-wide watermark fragments (seen in `>= MIN_DOCS` documents).
pub fn load_tokens(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT token FROM watermark_tokens WHERE doc_count >= ?1")
        .context("prepare load watermark_tokens")?;
    let rows = stmt
        .query_map(params![MIN_DOCS], |row| row.get::<_, String>(0))
        .context("query watermark_tokens")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect watermark_tokens")
}

/// Record watermark fragments discovered in one document. Increments the
/// per-fragment document counter so a fragment reaches `MIN_DOCS` once it has
/// been seen in enough documents.
pub fn record_tokens(conn: &Connection, tokens: &[String]) -> Result<()> {
    if tokens.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn
        .prepare(
            "INSERT INTO watermark_tokens (token, doc_count, updated_at) VALUES (?1, 1, ?2)
             ON CONFLICT(token) DO UPDATE SET doc_count = doc_count + 1, updated_at = excluded.updated_at",
        )
        .context("prepare record watermark_tokens")?;
    for t in tokens {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        stmt.execute(params![t, now])
            .context("execute record watermark_tokens")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE watermark_tokens (token TEXT PRIMARY KEY, doc_count INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL);",
        )
        .expect("schema");
        conn
    }

    #[test]
    fn token_needs_two_documents() {
        let conn = mem();
        record_tokens(&conn, &["X".into(), "陈骥".into()]).expect("record 1");
        assert!(load_tokens(&conn).expect("load").is_empty(), "one doc is not enough");
        record_tokens(&conn, &["陈骥".into()]).expect("record 2");
        let toks = load_tokens(&conn).expect("load 2");
        assert_eq!(toks, vec!["陈骥".to_string()]);
    }

    #[test]
    fn empty_tokens_are_ignored() {
        let conn = mem();
        record_tokens(&conn, &[]).expect("noop");
        record_tokens(&conn, &["  ".into()]).expect("blank");
        assert!(load_tokens(&conn).expect("load").is_empty());
    }
}
