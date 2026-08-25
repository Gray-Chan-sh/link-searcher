//! AI event log: append-only structured event trail for chat RAG pipeline.
//!
//! Each AI interaction turn records a sequence of events (query rewrite,
//! scope resolution, retrieval, context assembly, LLM call, turn complete)
//! so the frontend can replay the reasoning process for debugging.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// A single AI event within a chat turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiEvent {
    pub id: i64,
    pub session_id: String,
    pub turn_number: usize,
    pub event_seq: u32,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

/// Payload for a `query_rewrite` event.
#[derive(Debug, Clone, Serialize)]
pub struct QueryRewritePayload {
    pub original: String,
    pub rewritten: String,
    pub was_rewritten: bool,
    pub rewrite_method: String, // "rule" | "llm" | "none"
}

/// Payload for a `scope_resolved` event.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeResolvedPayload {
    pub dir_ids_count: usize,
    pub path_prefixes: Vec<String>,
    pub mention_files_count: usize,
    pub ext_filter: Option<Vec<String>>,
    pub date_range: Option<(String, String)>,
}

/// Payload for a `retrieval` event.
#[derive(Debug, Clone, Serialize)]
pub struct RetrievalPayload {
    pub search_query: String,
    pub bm25_hits: usize,
    pub semantic_fused: bool,
    pub merged_hits: usize,
    pub from_history_count: usize,
}

/// Payload for a `context_assembled` event.
#[derive(Debug, Clone, Serialize)]
pub struct ContextAssembledPayload {
    pub material_count: usize,
    pub total_chars: usize,
    pub strict_docs: bool,
    pub truncated_to: usize,
}

/// Payload for an `llm_call` event.
#[derive(Debug, Clone, Serialize)]
pub struct LlmCallPayload {
    pub model_id: String,
    pub system_prompt_chars: usize,
    pub user_msg_chars: usize,
    pub streaming: bool,
}

/// Payload for a `turn_complete` event.
#[derive(Debug, Clone, Serialize)]
pub struct TurnCompletePayload {
    pub took_ms: u64,
    pub cancelled: bool,
    pub answer_chars: usize,
    pub source_count: usize,
    pub evidence_count: usize,
}

/// Insert a single AI event. `event_seq` should be monotonic within a turn.
pub fn record_event(
    conn: &Connection,
    session_id: &str,
    turn_number: usize,
    event_seq: u32,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO ai_events (session_id, turn_number, event_seq, event_type, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![session_id, turn_number as i64, event_seq as i64, event_type, payload.to_string(), now],
    )
    .context("failed to insert ai_event")?;
    Ok(())
}

/// Read all events for a session, ordered by turn + seq.
pub fn get_session_events(conn: &Connection, session_id: &str) -> Result<Vec<AiEvent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_number, event_seq, event_type, payload_json, created_at
             FROM ai_events WHERE session_id = ?1
             ORDER BY turn_number, event_seq",
        )
        .context("failed to prepare get_session_events")?;
    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            let id: i64 = row.get(0)?;
            let session_id: String = row.get(1)?;
            let turn_number: i64 = row.get(2)?;
            let event_seq: i64 = row.get(3)?;
            let event_type: String = row.get(4)?;
            let payload_json: String = row.get(5)?;
            let created_at: i64 = row.get(6)?;
            let payload: serde_json::Value =
                serde_json::from_str(&payload_json).unwrap_or(serde_json::Value::Null);
            Ok(AiEvent {
                id,
                session_id,
                turn_number: turn_number as usize,
                event_seq: event_seq as u32,
                event_type,
                payload,
                created_at,
            })
        })
        .context("failed to query ai_events")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to collect ai_events")
}

/// Read events for a specific turn.
pub fn get_turn_events(
    conn: &Connection,
    session_id: &str,
    turn_number: usize,
) -> Result<Vec<AiEvent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_number, event_seq, event_type, payload_json, created_at
             FROM ai_events WHERE session_id = ?1 AND turn_number = ?2
             ORDER BY event_seq",
        )
        .context("failed to prepare get_turn_events")?;
    let rows = stmt
        .query_map(rusqlite::params![session_id, turn_number as i64], |row| {
            let id: i64 = row.get(0)?;
            let session_id: String = row.get(1)?;
            let turn_number: i64 = row.get(2)?;
            let event_seq: i64 = row.get(3)?;
            let event_type: String = row.get(4)?;
            let payload_json: String = row.get(5)?;
            let created_at: i64 = row.get(6)?;
            let payload: serde_json::Value =
                serde_json::from_str(&payload_json).unwrap_or(serde_json::Value::Null);
            Ok(AiEvent {
                id,
                session_id,
                turn_number: turn_number as usize,
                event_seq: event_seq as u32,
                event_type,
                payload,
                created_at,
            })
        })
        .context("failed to query ai_events for turn")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to collect ai_events for turn")
}

/// Delete events older than `before_ts` (unix seconds). Returns count deleted.
pub fn cleanup_old_events(conn: &Connection, before_ts: i64) -> Result<u64> {
    let deleted = conn
        .execute("DELETE FROM ai_events WHERE created_at < ?1", [before_ts])
        .context("failed to cleanup old ai_events")?;
    if deleted > 0 {
        log::info!("[DB] cleaned up {deleted} old ai_events (before ts={before_ts})");
    }
    Ok(deleted as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn record_and_get_session_events() {
        let conn = setup();
        let payload = serde_json::json!({"original": "hello", "rewritten": "hello world"});
        record_event(&conn, "s1", 0, 1, "query_rewrite", &payload).unwrap();
        record_event(&conn, "s1", 0, 2, "retrieval", &serde_json::json!({"hits": 5})).unwrap();
        record_event(&conn, "s1", 1, 1, "query_rewrite", &serde_json::json!({})).unwrap();

        let events = get_session_events(&conn, "s1").unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "query_rewrite");
        assert_eq!(events[0].turn_number, 0);
        assert_eq!(events[0].event_seq, 1);
        assert_eq!(events[1].event_seq, 2);
        assert_eq!(events[2].turn_number, 1);
    }

    #[test]
    fn get_turn_events_filters_correctly() {
        let conn = setup();
        record_event(&conn, "s1", 0, 1, "query_rewrite", &serde_json::json!({})).unwrap();
        record_event(&conn, "s1", 1, 1, "query_rewrite", &serde_json::json!({})).unwrap();
        record_event(&conn, "s1", 1, 2, "retrieval", &serde_json::json!({})).unwrap();

        let turn0 = get_turn_events(&conn, "s1", 0).unwrap();
        assert_eq!(turn0.len(), 1);
        let turn1 = get_turn_events(&conn, "s1", 1).unwrap();
        assert_eq!(turn1.len(), 2);
    }

    #[test]
    fn cleanup_old_events_removes_only_stale() {
        let conn = setup();
        let old_ts = 1000;
        let new_ts = 2000;
        // Manually insert with specific timestamps
        conn.execute(
            "INSERT INTO ai_events (session_id, turn_number, event_seq, event_type, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["s1", 0i64, 1i64, "test", "{}", old_ts],
        ).unwrap();
        conn.execute(
            "INSERT INTO ai_events (session_id, turn_number, event_seq, event_type, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["s1", 1i64, 1i64, "test", "{}", new_ts],
        ).unwrap();

        let deleted = cleanup_old_events(&conn, 1500).unwrap();
        assert_eq!(deleted, 1);
        let remaining = get_session_events(&conn, "s1").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].created_at, new_ts);
    }
}
