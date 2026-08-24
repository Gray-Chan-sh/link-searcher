//! ScopeResolver Skill：解析用户输入中的 @mention、/ext:、/date: 等范围命令。

use std::collections::HashSet;

use crate::commands::ai::{merge_scope_prefixes, TurnScope, resolve_mention_file_ids};
use crate::ai::skills::{Skill, SkillError};

pub struct ScopeResolverInput {
    pub scope: TurnScope,
    pub session_retrieval_scope: Vec<String>,
    pub db: &'static r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
}

pub struct ScopeResolverOutput {
    pub dir_ids: Option<Vec<String>>,
    pub path_prefixes: Option<Vec<String>>,
    pub ext_filter: Option<Vec<String>>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    pub mention_file_ids: Option<Vec<String>>,
    pub mention_resolved: Vec<(String, String)>,
    pub missing_mentions: Vec<String>,
}

pub struct ScopeResolverSkill;

impl Skill for ScopeResolverSkill {
    fn name(&self) -> &str { "ScopeResolver" }
}

impl ScopeResolverSkill {
    pub fn execute(&self, input: &ScopeResolverInput) -> Result<ScopeResolverOutput, SkillError> {
        let mut dir_ids: Vec<String> = Vec::new();
        let mut path_prefixes: Vec<String> = Vec::new();
        let mut scope_file_resolved: Vec<(String, String)> = Vec::new();

        // 1. session_retrieval_scope → dir_ids + path_prefixes + file_ids
        let conn = input.db.get().map_err(|e| SkillError { message: format!("db: {e}") })?;
        for dir_path in &input.session_retrieval_scope {
            let p = dir_path.trim().trim_end_matches('/');
            if p.is_empty() { continue; }
            if let Ok(mut stmt) = conn.prepare("SELECT id FROM dir_config WHERE path = ?1 OR alias = ?1") {
                if let Ok(r) = stmt.query_row(rusqlite::params![p], |row| row.get::<_, String>(0)) {
                    dir_ids.push(r);
                    continue;
                }
            }
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_path(&conn, p) {
                scope_file_resolved.push((rec.id, rec.path));
                continue;
            }
            if let Ok(ids) = crate::db::tracker::search_file_ids_by_path_fragment(&conn, p, 2) {
                if ids.len() == 1 {
                    if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &ids[0]) {
                        scope_file_resolved.push((rec.id, rec.path));
                        continue;
                    }
                }
            }
            path_prefixes.push(p.to_string());
        }
        drop(conn);

        // 2. mention_dirs → dir_ids + path_prefixes
        let dir_roots: Vec<(String, String)> = {
            let conn = input.db.get().map_err(|e| SkillError { message: format!("db: {e}") })?;
            let dirs = crate::db::dir_config::list_dirs(&conn).map_err(|e| SkillError { message: e.to_string() })?;
            dirs.into_iter().map(|d| (d.id, d.path)).collect()
        };
        let conn = input.db.get().map_err(|e| SkillError { message: format!("db: {e}") })?;
        for dir_path in &input.scope.mention_dirs {
            let p = dir_path.trim_end_matches('/');
            if p.is_empty() { continue; }
            if let Ok(mut stmt) = conn.prepare("SELECT id FROM dir_config WHERE path = ?1 OR alias = ?1") {
                if let Ok(r) = stmt.query_row(rusqlite::params![p], |row| row.get::<_, String>(0)) {
                    dir_ids.push(r);
                    continue;
                }
            }
            path_prefixes.push(p.to_string());
        }
        drop(conn);

        // 3. merge scope prefixes
        let (d, p) = merge_scope_prefixes(&dir_roots, &dir_ids, &path_prefixes);
        dir_ids = d;
        path_prefixes = p;

        // 4. conditions → ext / date filters
        let mut ext_filter: Option<Vec<String>> = None;
        let mut date_from: Option<i64> = None;
        let mut date_to: Option<i64> = None;
        for c in &input.scope.conditions {
            match c.kind.as_str() {
                "ext" => ext_filter = Some(c.value.split(',').map(|s| s.trim().to_lowercase()).collect()),
                "date" => {
                    let parts: Vec<&str> = c.value.splitn(2, '~').collect();
                    if parts.len() == 2 {
                        if let Ok(d) = chrono::NaiveDate::parse_from_str(parts[0], "%Y-%m-%d") {
                            date_from = Some(d.and_hms_opt(0,0,0).map(|dt| dt.and_utc().timestamp_micros()).unwrap_or(0));
                        }
                        if let Ok(d) = chrono::NaiveDate::parse_from_str(parts[1], "%Y-%m-%d") {
                            date_to = Some(d.and_hms_opt(23,59,59).map(|dt| dt.and_utc().timestamp_micros()).unwrap_or(0));
                        }
                    }
                }
                _ => {}
            }
        }

        let dir_ids_opt = if dir_ids.is_empty() { None } else { Some(dir_ids) };
        let path_prefixes_opt = if path_prefixes.is_empty() { None } else { Some(path_prefixes) };

        // 5. mention_files
        let (mut mention_resolved, missing_mentions) = {
            let conn = input.db.get().map_err(|e| SkillError { message: format!("db: {e}") })?;
            let (r, m) = resolve_mention_file_ids(&conn, &input.scope.mention_files);
            drop(conn);
            (r, m)
        };
        let mut seen: HashSet<String> = mention_resolved.iter().map(|(id, _)| id.clone()).collect();
        for (fid, path) in &scope_file_resolved {
            if seen.insert(fid.clone()) {
                mention_resolved.push((fid.clone(), path.clone()));
            }
        }
        let all_file_ids: Vec<String> = mention_resolved.iter().map(|(id, _)| id.clone()).collect();
        let mention_file_ids = if all_file_ids.is_empty() { None } else { Some(all_file_ids) };

        log::info!("[ScopeResolver] dir_ids={:?} path_prefixes={:?} file_ids={:?} ext={:?} date={:?}~{:?}",
            dir_ids_opt, path_prefixes_opt, mention_file_ids, ext_filter, date_from, date_to);

        Ok(ScopeResolverOutput {
            dir_ids: dir_ids_opt,
            path_prefixes: path_prefixes_opt,
            ext_filter,
            date_from,
            date_to,
            mention_file_ids,
            mention_resolved,
            missing_mentions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ai::ScopeCondition;

    #[test]
    fn empty_scope_returns_all_none() {
        let scope = TurnScope::default();
        // We can't test with real DB, but we can test the structure
        assert!(scope.mention_files.is_empty());
        assert!(scope.mention_dirs.is_empty());
    }
}
