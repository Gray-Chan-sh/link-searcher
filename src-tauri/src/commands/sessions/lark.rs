//! 飞书机器人所需的持久化（`docs/LARK_BOT.md` §5.5 阶段三）：
//!
//! 1. **p2p `chat_id` ↔ 本库聊天会话 的映射** —— 让"同一个私聊"跨 bot 重启仍然接着上
//!    一轮对话，而不是每次都新建会话。
//! 2. **事件去重环** —— 记住处理过的 `message_id`，忽略重投（§8 幂等）。
//!    官方明言 `event_id` **不可**用于去重（重投会变），所以键是 `message_id`。
//!
//! 为什么存 SQLite 而不是独立文件：事务性 + 跨进程安全（bot 是**独立进程**，经 webapi
//! 读写），不必再造一套文件锁与临时文件（见 §5.6 的教训）。

use rusqlite::{Connection, OptionalExtension};

/// 去重环默认容量：只记住最近这么多条已处理消息。
/// 飞书重投发生在秒级/分钟级，1000 条足以覆盖，同时避免表无限增长。
pub const DEFAULT_SEEN_CAP: u64 = 1000;

/// 一个私聊与本地聊天会话的绑定。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LarkBinding {
    /// 飞书的 p2p 会话 id（`oc_…`）。**per-user**，不是 per-device（§5.4 事实 2）。
    pub chat_id: String,
    /// 指向 `chat_history.json` 里 `origin = "lark"` 的会话 id。
    pub session_id: String,
    /// 对方（已通过白名单校验的）`open_id`，留档便于排查。
    #[serde(default)]
    pub user_open_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 取某个私聊的绑定。
pub fn get_binding(conn: &Connection, chat_id: &str) -> Result<Option<LarkBinding>, String> {
    conn.query_row(
        "SELECT chat_id, session_id, user_open_id, created_at, updated_at
           FROM lark_sessions WHERE chat_id = ?1",
        rusqlite::params![chat_id],
        |r| {
            Ok(LarkBinding {
                chat_id: r.get(0)?,
                session_id: r.get(1)?,
                user_open_id: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("读取飞书会话绑定失败: {e}"))
}

/// 写入/更新绑定。`created_at` 在更新时**保持原值**（只有 `session_id` /
/// `user_open_id` / `updated_at` 会变）。
pub fn upsert_binding(conn: &Connection, b: &LarkBinding) -> Result<(), String> {
    conn.execute(
        "INSERT INTO lark_sessions (chat_id, session_id, user_open_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(chat_id) DO UPDATE SET
             session_id   = excluded.session_id,
             user_open_id = excluded.user_open_id,
             updated_at   = excluded.updated_at",
        rusqlite::params![b.chat_id, b.session_id, b.user_open_id, b.created_at, b.updated_at],
    )
    .map_err(|e| format!("写入飞书会话绑定失败: {e}"))?;
    Ok(())
}

/// 删除绑定。返回是否真的删掉了一行。
pub fn delete_binding(conn: &Connection, chat_id: &str) -> Result<bool, String> {
    let n = conn
        .execute(
            "DELETE FROM lark_sessions WHERE chat_id = ?1",
            rusqlite::params![chat_id],
        )
        .map_err(|e| format!("删除飞书会话绑定失败: {e}"))?;
    Ok(n > 0)
}

/// 列出全部绑定（诊断 / 将来的"统一管理"视图用）。
pub fn list_bindings(conn: &Connection) -> Result<Vec<LarkBinding>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT chat_id, session_id, user_open_id, created_at, updated_at
               FROM lark_sessions ORDER BY updated_at DESC",
        )
        .map_err(|e| format!("查询飞书会话绑定失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LarkBinding {
                chat_id: r.get(0)?,
                session_id: r.get(1)?,
                user_open_id: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .map_err(|e| format!("查询飞书会话绑定失败: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("查询飞书会话绑定失败: {e}"))
}

/// 记录一条已处理的消息。**返回 `true` ＝ 首次见到（应当处理）**，
/// `false` ＝ 重投（应当忽略）。
///
/// 只在首次插入时修剪，把表的规模压在 `cap` 附近。
pub fn mark_seen(
    conn: &Connection,
    message_id: &str,
    now: i64,
    cap: u64,
) -> Result<bool, String> {
    let first = conn
        .execute(
            "INSERT OR IGNORE INTO lark_seen_messages (message_id, seen_at) VALUES (?1, ?2)",
            rusqlite::params![message_id, now],
        )
        .map_err(|e| format!("记录消息去重失败: {e}"))?
        == 1;

    if first && cap > 0 {
        // 保留最近 cap 条：删掉 offset 之后的部分（`LIMIT -1 OFFSET n` = 第 n 条起的全部）。
        conn.execute(
            "DELETE FROM lark_seen_messages WHERE message_id IN (
                 SELECT message_id FROM lark_seen_messages
                 ORDER BY seen_at DESC, message_id DESC
                 LIMIT -1 OFFSET ?1
             )",
            rusqlite::params![cap as i64],
        )
        .map_err(|e| format!("修剪消息去重环失败: {e}"))?;
    }
    Ok(first)
}

/// 去重环当前规模（诊断 / 测试用）。
pub fn seen_count(conn: &Connection) -> Result<u64, String> {
    conn.query_row("SELECT COUNT(*) FROM lark_seen_messages", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|n| n.max(0) as u64)
    .map_err(|e| format!("统计消息去重环失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用真实文件库（`init_db` 会跑 `PRAGMA wal_checkpoint`，内存库上不适用）。
    fn test_conn(tag: &str) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ls_lark_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.db");
        let conn = Connection::open(&path).unwrap();
        crate::db::init_db(&conn).unwrap();
        (conn, dir)
    }

    fn binding(chat: &str, session: &str, now: i64) -> LarkBinding {
        LarkBinding {
            chat_id: chat.into(),
            session_id: session.into(),
            user_open_id: "ou_x".into(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn binding_round_trips_and_upsert_keeps_created_at() {
        let (conn, dir) = test_conn("bind");
        assert!(get_binding(&conn, "oc_1").unwrap().is_none(), "初始无绑定");

        upsert_binding(&conn, &binding("oc_1", "s-1", 100)).unwrap();
        let got = get_binding(&conn, "oc_1").unwrap().unwrap();
        assert_eq!(got.session_id, "s-1");
        assert_eq!(got.created_at, 100);

        // 再绑一次（换成新会话）→ session_id 更新，created_at **保持**
        upsert_binding(&conn, &binding("oc_1", "s-2", 200)).unwrap();
        let got = get_binding(&conn, "oc_1").unwrap().unwrap();
        assert_eq!(got.session_id, "s-2");
        assert_eq!(got.created_at, 100, "upsert 不得重置 created_at");
        assert_eq!(got.updated_at, 200);

        // 不同私聊互不影响
        upsert_binding(&conn, &binding("oc_2", "s-3", 300)).unwrap();
        assert_eq!(list_bindings(&conn).unwrap().len(), 2);

        assert!(delete_binding(&conn, "oc_1").unwrap());
        assert!(!delete_binding(&conn, "oc_1").unwrap(), "重复删除返回 false");
        assert!(get_binding(&conn, "oc_1").unwrap().is_none());
        assert_eq!(list_bindings(&conn).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_seen_reports_duplicates() {
        let (conn, dir) = test_conn("seen");
        assert!(mark_seen(&conn, "om_1", 10, DEFAULT_SEEN_CAP).unwrap(), "首次应处理");
        assert!(!mark_seen(&conn, "om_1", 11, DEFAULT_SEEN_CAP).unwrap(), "重投应忽略");
        assert!(mark_seen(&conn, "om_2", 12, DEFAULT_SEEN_CAP).unwrap(), "另一条仍应处理");
        assert_eq!(seen_count(&conn).unwrap(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 去重环有上限：只保留最近 `cap` 条，且**最旧的先被淘汰**。
    #[test]
    fn seen_ring_keeps_only_the_newest_cap() {
        let (conn, dir) = test_conn("ring");
        for i in 0..10 {
            assert!(mark_seen(&conn, &format!("om_{i}"), 100 + i as i64, 5).unwrap());
        }
        assert!(seen_count(&conn).unwrap() <= 5, "环规模必须压在上限内");

        // 最新的 5 条仍在；最旧的已被淘汰 —— 淘汰掉的那条会被当成"首次"再次处理。
        assert!(!mark_seen(&conn, "om_9", 999, 5).unwrap(), "最新一条仍在环内");
        assert!(mark_seen(&conn, "om_0", 1000, 5).unwrap(), "最旧一条已淘汰 → 视作首次");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
