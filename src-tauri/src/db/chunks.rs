//! Long-document chunk storage + retrieval for RAG injection.
//!
//! Content longer than [`CHUNK_THRESHOLD`] chars is split into overlapping
//! sentence-aligned chunks at index time and stored keyed by **md5**
//! (`content_index` identity), so duplicate files share one chunk set.
//! At injection time, RAG selects the top-K chunks lexically relevant to the
//! query instead of truncating the full document.

use anyhow::Result;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};

/// Documents at or below this many chars stay full-text; only longer ones are
/// chunked.
pub const CHUNK_THRESHOLD: usize = 10_000;

const CHUNK_SIZE: usize = 1_500;
const CHUNK_OVERLAP: usize = 200;

#[derive(Debug, Clone)]
pub struct DocChunk {
    pub chunk_index: i64,
    pub start_char: i64,
    pub end_char: i64,
    pub text: String,
}

fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '\n' | '；' | ';')
}

/// Split `text` into `(start_char, end_char, chunk)` windows of at most
/// [`CHUNK_SIZE`] chars with [`CHUNK_OVERLAP`] overlap, preferring sentence
/// boundaries in the tail of each window. Empty when `text` needs no chunking.
pub fn chunk_text(text: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= CHUNK_THRESHOLD {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut end = (start + CHUNK_SIZE).min(chars.len());
        if end < chars.len() {
            // Search backwards from the window tail for a sentence break, but
            // never below 70% of the window so every chunk stays substantial.
            let floor = start + CHUNK_SIZE * 70 / 100;
            let mut i = end - 1;
            while i > floor {
                if is_sentence_end(chars[i]) {
                    end = i + 1;
                    break;
                }
                i -= 1;
            }
        }
        out.push((start, end, chars[start..end].iter().collect()));
        if end >= chars.len() {
            break;
        }
        start = (end - CHUNK_OVERLAP).max(start + 1);
    }
    out
}

/// Replace all stored chunks for `md5` (no-op delete when `chunks` empty).
pub fn replace_chunks(conn: &Connection, md5: &str, chunks: &[(usize, usize, String)]) -> Result<()> {
    conn.execute("DELETE FROM doc_chunks WHERE md5 = ?1", [md5])?;
    if chunks.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "INSERT INTO doc_chunks (md5, chunk_index, start_char, end_char, text) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (i, (s, e, t)) in chunks.iter().enumerate() {
        stmt.execute(params![md5, i as i64, *s as i64, *e as i64, t])?;
    }
    Ok(())
}

pub fn get_chunks(conn: &Connection, md5: &str) -> Result<Vec<DocChunk>> {
    let mut stmt = conn.prepare(
        "SELECT chunk_index, start_char, end_char, text FROM doc_chunks WHERE md5 = ?1 ORDER BY chunk_index",
    )?;
    let rows = stmt.query_map([md5], |r| {
        Ok(DocChunk {
            chunk_index: r.get(0)?,
            start_char: r.get(1)?,
            end_char: r.get(2)?,
            text: r.get(3)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn delete_chunks(conn: &Connection, md5: &str) -> Result<()> {
    conn.execute("DELETE FROM doc_chunks WHERE md5 = ?1", [md5])?;
    Ok(())
}

/// Deterministic lexical tokenizer for chunk scoring: ASCII alphanumeric
/// runs kept whole; longer mixed/CJK segments reduced to 2-char windows.
fn lexical_tokens(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in query.to_lowercase().split_whitespace() {
        let cs: Vec<char> = seg.chars().collect();
        if cs.is_empty() {
            continue;
        }
        if cs.len() <= 2 || cs.iter().all(|c| c.is_ascii_alphanumeric()) {
            out.push(seg.to_string());
            continue;
        }
        for w in cs.windows(2) {
            out.push(w.iter().collect());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Pick the top-K chunks lexically relevant to `query` (term overlap),
/// returned in original reading order. Zero-overlap fallback: the first K
/// chunks (document head).
pub fn select_relevant_chunks<'a>(chunks: &'a [DocChunk], query: &str, k: usize) -> Vec<&'a DocChunk> {
    if chunks.len() <= k {
        return chunks.iter().collect();
    }
    let tokens = lexical_tokens(query);
    let score = |text: &str| -> usize {
        let lower = text.to_lowercase();
        tokens.iter().map(|t| lower.matches(t.as_str()).count()).sum()
    };
    let mut scored: Vec<(usize, &DocChunk)> =
        chunks.iter().map(|c| (score(&c.text), c)).collect();
    if scored.iter().all(|(s, _)| *s == 0) {
        return chunks.iter().take(k).collect();
    }
    // Stable sort keeps original order among ties → earlier chunks win ties.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let mut picked: Vec<&DocChunk> = scored.into_iter().take(k).map(|(_, c)| c).collect();
    picked.sort_by_key(|c| c.chunk_index);
    picked
}

/// Chunk indexed contents that exceed the threshold but have no chunks yet.
/// Capped per run; repeated scans converge. Returns number of docs chunked.
pub fn run_backfill_chunks(db: &Pool<SqliteConnectionManager>) -> Result<u64> {
    const MAX_PER_RUN: usize = 500;
    let conn = db.get()?;
    let md5s: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT ci.md5 FROM content_index ci WHERE length(ci.text_content) > ?1 \
             AND ci.md5 NOT IN (SELECT md5 FROM doc_chunks) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![CHUNK_THRESHOLD as i64, MAX_PER_RUN as i64], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut n = 0u64;
    for md5 in md5s {
        let Some(text) = crate::db::tracker::get_content(&conn, &md5)? else {
            continue;
        };
        let chunks = chunk_text(&text);
        if chunks.is_empty() {
            continue;
        }
        replace_chunks(&conn, &md5, &chunks)?;
        n += 1;
    }
    if n > 0 {
        log::info!("[CHUNK] backfilled {n} long documents");
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn short_text_is_not_chunked() {
        assert!(chunk_text("短文本").is_empty());
        assert!(chunk_text(&"a".repeat(CHUNK_THRESHOLD)).is_empty());
    }

    #[test]
    fn long_text_produces_overlapping_sentence_aligned_chunks() {
        let para = "这是一段测试文字，用于验证分块逻辑。\n";
        let text = para.repeat(800); // ~15k chars
        let chunks = chunk_text(&text);
        assert!(chunks.len() >= 10, "expected many chunks, got {}", chunks.len());

        let total: usize = text.chars().count();
        for (idx, (_s, e, t)) in chunks.iter().enumerate() {
            assert!(e - _s <= CHUNK_SIZE, "chunk {idx} too large: {e}-{_s}");
            assert_eq!(t.chars().count(), e - _s);
            assert!(*e <= total);
            if idx + 1 == chunks.len() {
                assert_eq!(*e, total, "last chunk must reach EOF");
            } else {
                // Overlap with the next window.
                assert!(*e > chunks[idx + 1].0, "no overlap between chunk {idx} and next");
            }
        }
        let aligned = chunks.iter()
            .filter(|(_, _, t)| t.chars().last().is_some_and(is_sentence_end))
            .count();
        assert!(aligned >= chunks.len() / 2, "too few sentence-aligned cuts: {aligned}/{}", chunks.len());
    }

    #[test]
    fn select_prefers_lexical_match_and_keeps_reading_order() {
        let mk = |i: i64, t: &str| DocChunk {
            chunk_index: i,
            start_char: i * 1000,
            end_char: i * 1000 + 999,
            text: t.to_string(),
        };
        let chunks = vec![
            mk(0, "开场介绍 案件背景说明"),
            mk(1, "程序性事项与送达记录"),
            mk(2, "关键证据：6,100万资金流向明细"),
            mk(3, "证人证言整理"),
            mk(4, "结论与判决结果"),
        ];
        let sel = select_relevant_chunks(&chunks, "6,100万 资金流向", 2);
        assert_eq!(sel.len(), 2);
        // Reading order: lowest index first; chunk 2 must be among picks.
        assert_eq!(sel[0].chunk_index, 0);
        assert_eq!(sel[1].chunk_index, 2, "matching middle chunk must be selected");
    }

    #[test]
    fn select_zero_overlap_falls_back_to_head() {
        let mk = |i: i64| DocChunk {
            chunk_index: i,
            start_char: 0,
            end_char: 10,
            text: format!("内容{i}"),
        };
        let chunks: Vec<DocChunk> = (0..6).map(mk).collect();
        let sel = select_relevant_chunks(&chunks, "完全无关的查询词", 3);
        assert_eq!(sel.len(), 3);
        assert_eq!(sel[0].chunk_index, 0);
    }

    #[test]
    fn replace_get_roundtrip_and_delete() {
        let conn = setup_db();
        let text = "句子。".repeat(5000); // 15k chars
        let chunks = chunk_text(&text);
        assert!(!chunks.is_empty());
        replace_chunks(&conn, "abc123", &chunks).unwrap();
        let loaded = get_chunks(&conn, "abc123").unwrap();
        assert_eq!(loaded.len(), chunks.len());
        assert_eq!(loaded[0].start_char, 0);
        assert_eq!(loaded.last().unwrap().end_char as usize, text.chars().count());

        // Re-replace overwrites (idempotent).
        replace_chunks(&conn, "abc123", &chunks).unwrap();
        assert_eq!(get_chunks(&conn, "abc123").unwrap().len(), chunks.len());

        // Below-threshold replacement clears stored chunks.
        replace_chunks(&conn, "abc123", &[]).unwrap();
        assert!(get_chunks(&conn, "abc123").unwrap().is_empty());
    }

    #[test]
    fn backfill_runs_over_pool_and_skips_short_or_chunked() {
        let dir = std::env::temp_dir().join(format!("chunks_bf_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            db::run_migrations(&conn).unwrap();
            let long_text = "长文档内容测试。".repeat(1300);
            crate::db::tracker::store_content(&conn, "long_md5", &long_text, false, None).unwrap();
            crate::db::tracker::store_content(&conn, "short_md5", "很短", false, None).unwrap();
        }
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(SqliteConnectionManager::file(&db_path))
            .unwrap();

        assert_eq!(run_backfill_chunks(&pool).unwrap(), 1, "first run chunks the long doc");
        assert_eq!(run_backfill_chunks(&pool).unwrap(), 0, "backfill must be idempotent");

        let conn = pool.get().unwrap();
        assert!(get_chunks(&conn, "long_md5").unwrap().len() > 1);
        assert!(get_chunks(&conn, "short_md5").unwrap().is_empty());
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
