use rusqlite::{Connection, params_from_iter};

#[test]
fn pagination_with_507_rows_returns_correct_pages() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_tracking (
            id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE, dir_id TEXT NOT NULL,
            mtime INTEGER NOT NULL, size INTEGER NOT NULL DEFAULT 0, md5 TEXT,
            status TEXT NOT NULL DEFAULT 'active', indexed INTEGER NOT NULL DEFAULT 0,
            error_msg TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
        )",
    )
    .unwrap();
    for i in 0..507 {
        conn.execute(
            "INSERT INTO file_tracking VALUES (?1,?2,'d',0,100,NULL,'active',2,'err',0,0)",
            rusqlite::params![format!("id{i}"), format!("/tmp/f{i}.pdf")],
        )
        .unwrap();
    }

    // Reproduce exact list_files_db logic: filter=failed, no ext/search
    let mut wheres: Vec<&str> = vec!["status = 'active'"];
    wheres.push("indexed = 2");
    let mut params: Vec<Box<dyn rusqlite::ToSql + Send>> = Vec::new();
    let where_clause = wheres.join(" AND ");
    let ps: usize = 20;
    let p: usize = 11;
    let offset = (p - 1) * ps;

    let count_sql = format!("SELECT COUNT(*) FROM file_tracking WHERE {where_clause}");
    let total: u64 = conn
        .query_row(&count_sql, params_from_iter(params.iter().map(|x| x as &dyn rusqlite::ToSql)), |r| r.get(0))
        .unwrap();

    let data_sql = format!(
        "SELECT id, path FROM file_tracking WHERE {where_clause} ORDER BY path ASC LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2,
    );
    let mut data_params: Vec<Box<dyn rusqlite::ToSql + Send>> = params;
    data_params.push(Box::new(ps as i64));
    data_params.push(Box::new(offset as i64));

    let mut stmt = conn.prepare(&data_sql).unwrap();
    let rows: Vec<_> = stmt
        .query_map(params_from_iter(data_params.iter().map(|x| x as &dyn rusqlite::ToSql)), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    println!("total={total} total_pages={} page11_rows={}", (total as f64 / ps as f64).ceil(), rows.len());
    assert_eq!(total, 507);
    assert_eq!(rows.len(), 20, "page 11 should have 20 rows");
}

#[test]
fn pagination_with_ext_param_binds_correctly() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_tracking (
            id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE, dir_id TEXT NOT NULL,
            mtime INTEGER NOT NULL, size INTEGER NOT NULL DEFAULT 0, md5 TEXT,
            status TEXT NOT NULL DEFAULT 'active', indexed INTEGER NOT NULL DEFAULT 0,
            error_msg TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
        )",
    )
    .unwrap();
    for i in 0..507 {
        let path = if i % 2 == 0 { format!("/tmp/f{i}.pdf") } else { format!("/tmp/f{i}.docx") };
        conn.execute(
            "INSERT INTO file_tracking VALUES (?1,?2,'d',0,100,NULL,'active',2,'err',0,0)",
            rusqlite::params![format!("id{i}"), path],
        )
        .unwrap();
    }

    // filter=failed + ext=pdf
    let mut wheres: Vec<&str> = vec!["status = 'active'"];
    wheres.push("indexed = 2");
    wheres.push("path LIKE ?");
    let mut params: Vec<Box<dyn rusqlite::ToSql + Send>> = Vec::new();
    params.push(Box::new("%.pdf".to_string()));
    let where_clause = wheres.join(" AND ");
    let ps: usize = 20;
    let p: usize = 11;
    let offset = (p - 1) * ps;

    let count_sql = format!("SELECT COUNT(*) FROM file_tracking WHERE {where_clause}");
    let total: u64 = conn
        .query_row(&count_sql, params_from_iter(params.iter().map(|x| x as &dyn rusqlite::ToSql)), |r| r.get(0))
        .unwrap();

    let data_sql = format!(
        "SELECT id, path FROM file_tracking WHERE {where_clause} ORDER BY path ASC LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2,
    );
    let mut data_params: Vec<Box<dyn rusqlite::ToSql + Send>> = params;
    data_params.push(Box::new(ps as i64));
    data_params.push(Box::new(offset as i64));

    let mut stmt = conn.prepare(&data_sql).unwrap();
    let rows: Vec<_> = stmt
        .query_map(params_from_iter(data_params.iter().map(|x| x as &dyn rusqlite::ToSql)), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    println!("ext filter: total={total} page11_rows={}", rows.len());
    assert_eq!(total, 254);
    assert_eq!(rows.len(), 20);
}
