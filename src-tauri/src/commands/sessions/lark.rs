//! 飞书机器人所需的持久化（`docs/LARK_BOT.md` §5.5 阶段三 + §11 多会话）：
//!
//! 1. **p2p `chat_id` ↔ 本库聊天会话 的映射** —— 让"同一个私聊"跨 bot 重启仍然接着上
//!    一轮对话，而不是每次都新建会话。
//!    v10 起**一个私聊可持有多个会话**（`is_active = 1` 的那条是当前活动会话），
//!    支撑 `/sessions` + `/switch` 的多组多轮（见 `db/mod.rs` 的 9 → 10 迁移）。
//! 2. **事件去重环** —— 记住处理过的 `message_id`，忽略重投（§8 幂等）。
//!    官方明言 `event_id` **不可**用于去重（重投会变），所以键是 `message_id`。
//!
//! 为什么存 SQLite 而不是独立文件：事务性 + 跨进程安全（bot 是**独立进程**，经 webapi
//! 读写），不必再造一套文件锁与临时文件（见 §5.6 的教训）。

use rusqlite::{Connection, OptionalExtension};

/// 去重环默认容量：只记住最近这么多条已处理消息。
/// 飞书重投发生在秒级/分钟级，1000 条足以覆盖，同时避免表无限增长。
pub const DEFAULT_SEEN_CAP: u64 = 1000;

/// 单个私聊的会话数量上限（防止 `/new` 被刷爆表；超出时删掉最旧的**非活动**会话）。
pub const MAX_SESSIONS_PER_CHAT: usize = 50;

/// 一个私聊与本地聊天会话的绑定（多会话表里的一行）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LarkBinding {
    /// 飞书的 p2p 会话 id（`oc_…`）。**per-user**，不是 per-device（§5.4 事实 2）。
    pub chat_id: String,
    /// 指向 `chat_history.json` 里 `origin = "lark"` 的会话 id。
    pub session_id: String,
    /// 对方（已通过白名单校验的）`open_id`，留档便于排查。
    #[serde(default)]
    pub user_open_id: String,
    /// 是否当前活动会话（每个 `chat_id` 至多一条）。
    #[serde(default)]
    pub is_active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

const BINDING_COLUMNS: &str =
    "chat_id, session_id, user_open_id, is_active, created_at, updated_at";

fn row_to_binding(r: &rusqlite::Row) -> rusqlite::Result<LarkBinding> {
    Ok(LarkBinding {
        chat_id: r.get(0)?,
        session_id: r.get(1)?,
        user_open_id: r.get(2)?,
        is_active: r.get::<_, i64>(3)? != 0,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

/// 取某个私聊的**活动**绑定（没有绑定时 `None`）。
pub fn get_binding(conn: &Connection, chat_id: &str) -> Result<Option<LarkBinding>, String> {
    conn.query_row(
        &format!("SELECT {BINDING_COLUMNS} FROM lark_chat_sessions WHERE chat_id = ?1 AND is_active = 1"),
        rusqlite::params![chat_id],
        row_to_binding,
    )
    .optional()
    .map_err(|e| format!("读取飞书会话绑定失败: {e}"))
}

/// 列出某个私聊的全部会话（**活动在前，其余按最近使用倒序**）。
pub fn list_chat_sessions(conn: &Connection, chat_id: &str) -> Result<Vec<LarkBinding>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {BINDING_COLUMNS} FROM lark_chat_sessions
              WHERE chat_id = ?1
              ORDER BY is_active DESC, updated_at DESC, session_id"
        ))
        .map_err(|e| format!("查询飞书会话列表失败: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![chat_id], row_to_binding)
        .map_err(|e| format!("查询飞书会话列表失败: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("查询飞书会话列表失败: {e}"))
}

/// 写入/更新绑定并**置为活动**（同私聊其余行取消活动）。
///
/// `created_at` 在更新时**保持原值**（只有 `user_open_id` / `updated_at` / `is_active` 会变）。
/// 两条语句走同一事务：否则中途失败会留下"零个活动会话"的私聊（下一次消息落到新建的
/// 陌生会话上，且旧会话从列表里"消失"—— 最难排查的那种）。
pub fn upsert_binding(conn: &Connection, b: &LarkBinding) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启飞书绑定事务失败: {e}"))?;
    tx.execute(
        "UPDATE lark_chat_sessions SET is_active = 0 WHERE chat_id = ?1 AND is_active = 1",
        rusqlite::params![b.chat_id],
    )
    .map_err(|e| format!("切换活动会话失败: {e}"))?;
    tx.execute(
        "INSERT INTO lark_chat_sessions (chat_id, session_id, user_open_id, is_active, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?5)
         ON CONFLICT(chat_id, session_id) DO UPDATE SET
             user_open_id = excluded.user_open_id,
             is_active    = 1,
             updated_at   = excluded.updated_at",
        rusqlite::params![b.chat_id, b.session_id, b.user_open_id, b.created_at, b.updated_at],
    )
    .map_err(|e| format!("写入飞书会话绑定失败: {e}"))?;
    enforce_session_cap(&tx, &b.chat_id)?;
    tx.commit().map_err(|e| format!("提交飞书绑定事务失败: {e}"))?;
    Ok(())
}

/// 把单个私聊的会话数压回 [`MAX_SESSIONS_PER_CHAT`]：优先删**最旧的、非活动的**那一行。
///
/// 只在超出上限时动刀（正常使用永远碰不到）；活动会话**永不被自动删**。
fn enforce_session_cap(tx: &Connection, chat_id: &str) -> Result<(), String> {
    tx.execute(
        "DELETE FROM lark_chat_sessions
          WHERE chat_id = ?1 AND is_active = 0
            AND session_id IN (
                SELECT session_id FROM lark_chat_sessions
                 WHERE chat_id = ?1 AND is_active = 0
                 ORDER BY updated_at DESC, session_id DESC
                 LIMIT -1 OFFSET ?2
            )",
        rusqlite::params![chat_id, MAX_SESSIONS_PER_CHAT.saturating_sub(1) as i64],
    )
    .map_err(|e| format!("修剪飞书会话列表失败: {e}"))?;
    Ok(())
}

/// 切换活动会话。返回 `false` ＝ 目标会话**不属于**该私聊（调用方据此拒绝，
/// 而不是"没找到就当成功"）。
pub fn set_active(conn: &Connection, chat_id: &str, session_id: &str, now: i64) -> Result<bool, String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启切换事务失败: {e}"))?;
    let exists: Option<String> = tx
        .query_row(
            "SELECT session_id FROM lark_chat_sessions WHERE chat_id = ?1 AND session_id = ?2",
            rusqlite::params![chat_id, session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("查询目标会话失败: {e}"))?;
    if exists.is_none() {
        return Ok(false);
    }
    tx.execute(
        "UPDATE lark_chat_sessions SET is_active = 0 WHERE chat_id = ?1 AND is_active = 1",
        rusqlite::params![chat_id],
    )
    .map_err(|e| format!("切换活动会话失败: {e}"))?;
    tx.execute(
        "UPDATE lark_chat_sessions SET is_active = 1, updated_at = ?3
          WHERE chat_id = ?1 AND session_id = ?2",
        rusqlite::params![chat_id, session_id, now],
    )
    .map_err(|e| format!("切换活动会话失败: {e}"))?;
    tx.commit().map_err(|e| format!("提交切换事务失败: {e}"))?;
    Ok(true)
}

/// 删除某私聊的一条会话映射。返回是否真的删掉了一行。
///
/// 只删**映射**：`chat_history.json` 里的内容保留，接着建的会话不受影响
/// （"删除映射"丢了对话内容才是意外，参见 `/sessions` 的确认交互）。
pub fn delete_session(conn: &Connection, chat_id: &str, session_id: &str) -> Result<bool, String> {
    let n = conn
        .execute(
            "DELETE FROM lark_chat_sessions WHERE chat_id = ?1 AND session_id = ?2",
            rusqlite::params![chat_id, session_id],
        )
        .map_err(|e| format!("删除飞书会话绑定失败: {e}"))?;
    Ok(n > 0)
}

/// 删除某私聊的**全部**绑定（诊断 / 重置用）。
pub fn delete_binding(conn: &Connection, chat_id: &str) -> Result<bool, String> {
    let n = conn
        .execute(
            "DELETE FROM lark_chat_sessions WHERE chat_id = ?1",
            rusqlite::params![chat_id],
        )
        .map_err(|e| format!("删除飞书会话绑定失败: {e}"))?;
    Ok(n > 0)
}

/// 列出全部**活动**绑定（诊断 / "统一管理"视图用）。
pub fn list_bindings(conn: &Connection) -> Result<Vec<LarkBinding>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {BINDING_COLUMNS} FROM lark_chat_sessions
              WHERE is_active = 1 ORDER BY updated_at DESC"
        ))
        .map_err(|e| format!("查询飞书会话绑定失败: {e}"))?;
    let rows = stmt
        .query_map([], row_to_binding)
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
            is_active: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// 基础契约：upsert 置活动且**保留 created_at**；不同私聊互不影响。
    #[test]
    fn binding_round_trips_and_upsert_keeps_created_at() {
        let (conn, dir) = test_conn("bind");
        assert!(get_binding(&conn, "oc_1").unwrap().is_none(), "初始无绑定");

        upsert_binding(&conn, &binding("oc_1", "s-1", 100)).unwrap();
        let got = get_binding(&conn, "oc_1").unwrap().unwrap();
        assert_eq!(got.session_id, "s-1");
        assert_eq!(got.created_at, 100);
        assert!(got.is_active);

        // 再绑一个新会话 → 它成为活动；旧会话**仍在**（多会话：只是不再活动）
        upsert_binding(&conn, &binding("oc_1", "s-2", 200)).unwrap();
        let got = get_binding(&conn, "oc_1").unwrap().unwrap();
        assert_eq!(got.session_id, "s-2");
        assert_eq!(got.created_at, 200);
        assert_eq!(got.updated_at, 200);
        let all = list_chat_sessions(&conn, "oc_1").unwrap();
        assert_eq!(all.len(), 2, "旧的 s-1 不应被 upsert 覆盖掉");
        assert_eq!(all[0].session_id, "s-2", "活动会话排最前");
        assert!(all[0].is_active && !all[1].is_active);

        // 回到 s-1：created_at 保持原值（100），新的 updated_at 生效
        upsert_binding(&conn, &binding("oc_1", "s-1", 300)).unwrap();
        let back = get_binding(&conn, "oc_1").unwrap().unwrap();
        assert_eq!(back.session_id, "s-1");
        assert_eq!(back.created_at, 100, "upsert 不得重置 created_at");
        assert_eq!(back.updated_at, 300);
        assert_eq!(
            list_chat_sessions(&conn, "oc_1").unwrap().iter().filter(|b| b.is_active).count(),
            1,
            "任何时刻至多一个活动会话"
        );

        // 不同私聊互不影响
        upsert_binding(&conn, &binding("oc_2", "s-3", 400)).unwrap();
        assert_eq!(list_bindings(&conn).unwrap().len(), 2, "list_bindings 只列活动行");

        assert!(delete_binding(&conn, "oc_1").unwrap());
        assert!(!delete_binding(&conn, "oc_1").unwrap(), "重复删除返回 false");
        assert!(get_binding(&conn, "oc_1").unwrap().is_none());
        assert_eq!(list_bindings(&conn).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多会话核心：切换 / 删除 / 跨私聊隔离 / 不属于本私聊的会话必须被拒。
    #[test]
    fn multi_session_switch_delete_and_ownership() {
        let (conn, dir) = test_conn("multi");
        for (i, s) in ["s-1", "s-2", "s-3"].iter().enumerate() {
            upsert_binding(&conn, &binding("oc_1", s, 100 + i as i64)).unwrap();
        }
        assert_eq!(get_binding(&conn, "oc_1").unwrap().unwrap().session_id, "s-3");

        // 切到 s-1
        assert!(set_active(&conn, "oc_1", "s-1", 200).unwrap());
        let now = get_binding(&conn, "oc_1").unwrap().unwrap();
        assert_eq!(now.session_id, "s-1");
        assert_eq!(now.updated_at, 200, "切换会刷新 updated_at");
        assert_eq!(list_chat_sessions(&conn, "oc_1").unwrap()[0].session_id, "s-1");

        // 目标不属于本私聊 → false，且**不改动任何活动指针**
        upsert_binding(&conn, &binding("oc_2", "s-9", 300)).unwrap();
        assert!(!set_active(&conn, "oc_1", "s-9", 400).unwrap());
        assert!(!set_active(&conn, "oc_1", "nope", 400).unwrap());
        assert_eq!(
            get_binding(&conn, "oc_1").unwrap().unwrap().session_id,
            "s-1",
            "被拒的切换不得扰动当前活动会话"
        );

        // 删一条非活动会话：活动指针不受影响
        assert!(delete_session(&conn, "oc_1", "s-2").unwrap());
        assert!(!delete_session(&conn, "oc_1", "s-2").unwrap(), "重复删除返回 false");
        assert_eq!(get_binding(&conn, "oc_1").unwrap().unwrap().session_id, "s-1");
        assert_eq!(list_chat_sessions(&conn, "oc_1").unwrap().len(), 2);

        // 删掉活动会话：该私聊**暂时没有活动行**（bot 侧负责兜底重建）
        assert!(delete_session(&conn, "oc_1", "s-1").unwrap());
        assert!(get_binding(&conn, "oc_1").unwrap().is_none());
        assert!(!list_chat_sessions(&conn, "oc_1").unwrap().is_empty(), "非活动行仍在");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话数量上限：超出时淘汰**最旧的、非活动的**行，活动行永不被自动删。
    #[test]
    fn session_cap_evicts_oldest_inactive_only() {
        let (conn, dir) = test_conn("cap");
        // 造 MAX+3 条，时间戳递减：最后插入的 s-52 是**活动**会话；编号越大时间戳越小（越旧）
        let total = MAX_SESSIONS_PER_CHAT + 3;
        for i in 0..total {
            upsert_binding(&conn, &binding("oc_1", &format!("s-{i}"), 10_000 - i as i64)).unwrap();
            let active = get_binding(&conn, "oc_1").unwrap().unwrap();
            assert_eq!(
                active.session_id,
                format!("s-{i}"),
                "第 {i} 次 upsert 后活动会话必须落在刚插入的行上（活动行不得被容量淘汰）"
            );
        }
        let rows = list_chat_sessions(&conn, "oc_1").unwrap();
        assert_eq!(
            rows.len(),
            MAX_SESSIONS_PER_CHAT,
            "总数（含活动）必须压在上限内"
        );
        assert!(rows[0].is_active, "活动会话必须排最前");
        assert_eq!(
            rows[0].session_id,
            format!("s-{}", total - 1),
            "活动会话 = 最后 upsert 的那条"
        );
        // 时间戳递减 ⇒ **编号越大越旧**：最旧的 3 条（s-49/s-50/s-51）已被淘汰，
        // 第 4 旧（s-48）仍在 —— 边界必须精确（多删会看不见会话，少删会超上限）。
        assert!(
            !rows.iter().any(|r| r.session_id == "s-49"),
            "最旧的行（s-49）应已被淘汰"
        );
        assert!(
            rows.iter().any(|r| r.session_id == "s-48"),
            "最早保留的行（s-48）不得被多删"
        );
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
