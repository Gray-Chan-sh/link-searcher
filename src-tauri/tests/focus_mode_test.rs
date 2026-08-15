//! 专注模式后端解析逻辑自动化测试。
//!
//! 专注模式（focus_file）的生效链路：
//! 1. `handleSend` 将 `session.focus_file` 写入 `scope.mention_files`
//! 2. `prepare_conversation_prompt` 将 `mention_files` 路径解析为 `file_ids`
//! 3. `bm25_relevant_hits` 用 `file_ids` 精确过滤搜索
//!
//! 本测试验证第 2 步的核心：路径 → file_id 的解析（精确匹配 + LIKE 回退）。

use std::path::PathBuf;

use link_searcher_lib::db;
use link_searcher_lib::db::tracker;

struct TempDir(PathBuf);
impl TempDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ls_focus_{prefix}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &std::path::Path { &self.0 }
}
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// 初始化临时 DB 并插入一个测试文件记录（毛莹.pdf）。
fn setup_db(tmp: &TempDir) -> rusqlite::Connection {
    let db_path = tmp.path().join("test.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    db::init_db(&conn).unwrap();

    tracker::upsert_file(&conn, "毛莹.pdf", "dir-1", 0, 1024, Some("md5-1")).unwrap();
    // 再插入一个路径含"毛莹"但不同名的文件，验证 LIKE 不会误匹配。
    tracker::upsert_file(&conn, "案件/CH 常宏案/聘请律师合同陈坚锋毛莹律师2024.7.7.pdf", "dir-1", 0, 2048, Some("md5-2")).unwrap();
    conn
}

#[test]
fn focus_exact_path_resolves_to_file_id() {
    let tmp = TempDir::new("exact");
    let conn = setup_db(&tmp);
    let _ = conn;

    // 精确匹配：路径与 DB 完全一致
    let rec = tracker::get_file_by_path(&conn, "毛莹.pdf").unwrap();
    assert!(rec.is_some(), "精确路径应命中 file_tracking");
    assert_eq!(rec.unwrap().path, "毛莹.pdf");
}

#[test]
fn focus_like_fallback_resolves_filename_only() {
    let tmp = TempDir::new("like");
    let conn = setup_db(&tmp);

    // LIKE 回退：用户输入 `@毛莹.pdf`（仅文件名，DB 里正好有精确路径）
    // 应先精确匹配成功，不需要 LIKE。
    let rec = tracker::get_file_by_path(&conn, "毛莹.pdf").unwrap();
    assert!(rec.is_some());

    // 模拟 prepare_conversation_prompt 的解析逻辑：
    let ids: Vec<String> = ["毛莹.pdf"].iter().filter_map(|path| {
        if let Ok(Some(r)) = tracker::get_file_by_path(&conn, path) {
            return Some(r.id);
        }
        if let Ok(mut ids) = tracker::search_file_ids_by_path_fragment(&conn, path, 1) {
            return ids.pop();
        }
        None
    }).collect();
    assert_eq!(ids.len(), 1, "应恰好解析出 1 个 file_id");
}

#[test]
fn focus_like_returns_correct_file_not_similar_names() {
    let tmp = TempDir::new("like_correct");
    let conn = setup_db(&tmp);

    // 用 LIKE 搜索 `毛莹.pdf`（精确路径不存在时用片段匹配）
    let ids = tracker::search_file_ids_by_path_fragment(&conn, "毛莹.pdf", 1).unwrap();
    assert_eq!(ids.len(), 1);

    // 验证取回的是 毛莹.pdf，而不是 律师合同...毛莹律师...pdf
    let rec = tracker::get_file_by_id(&conn, &ids[0]).unwrap().unwrap();
    assert_eq!(rec.path, "毛莹.pdf", "LIKE 应匹配恰好名为毛莹.pdf 的文件");
}

#[test]
fn focus_file_in_scope_is_resolved_to_ids() {
    // 模拟 handleSend 后的 scope.mention_files = [session.focus_file]
    let tmp = TempDir::new("scope");
    let conn = setup_db(&tmp);

    // 专注模式：scope.mention_files = ["毛莹.pdf"]（来自 session.focus_file）
    let scope_mention_files = vec!["毛莹.pdf".to_string()];

    let ids: Vec<String> = scope_mention_files.iter().filter_map(|path| {
        if let Ok(Some(r)) = tracker::get_file_by_path(&conn, path) {
            return Some(r.id);
        }
        if let Ok(mut ids) = tracker::search_file_ids_by_path_fragment(&conn, path, 1) {
            return ids.pop();
        }
        None
    }).collect();

    assert!(!ids.is_empty(), "专注文件的路径应解析出 file_id");
    assert_eq!(ids.len(), 1);
    let rec = tracker::get_file_by_id(&conn, &ids[0]).unwrap().unwrap();
    assert_eq!(rec.path, "毛莹.pdf");
}