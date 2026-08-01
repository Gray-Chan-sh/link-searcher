use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use crate::config::{AppConfig, INDEX_DIR_NAME, load_config, save_config};

#[derive(Serialize, Deserialize, Clone)]
pub struct ConfigInfo {
    pub data_dir: String,
    pub language: String,
    pub lo_binary_path: String,
}

#[tauri::command]
pub fn get_config() -> Result<ConfigInfo, String> {
    let config = load_config();
    Ok(ConfigInfo {
        data_dir: config.data_dir.to_string_lossy().to_string(),
        language: config.language,
        lo_binary_path: config.lo_binary_path,
    })
}

#[tauri::command]
pub fn update_config(new_config: ConfigInfo) -> Result<(), String> {
    let config = AppConfig {
        data_dir: new_config.data_dir.into(),
        language: new_config.language,
        lo_binary_path: new_config.lo_binary_path,
    };
    save_config(&config)
}

#[tauri::command]
pub fn migrate_data(old_path: String, new_path: String) -> Result<String, String> {
    let old = std::path::Path::new(&old_path);
    let new = std::path::Path::new(&new_path);

    if !old.exists() {
        return Err("当前数据目录不存在".to_string());
    }
    // Allow migration to existing directory, but refuse if it already has index data
    let existing_db = new.join("data.db");
    if existing_db.exists() {
        return Err("目标目录已包含 data.db，请选择空目录或新目录".to_string());
    }
    let existing_index = new.join(INDEX_DIR_NAME);
    if existing_index.exists() {
        return Err("目标目录已包含索引文件夹，请选择空目录或新目录".to_string());
    }

    std::fs::create_dir_all(new).map_err(|e| format!("无法创建目标目录: {e}"))?;

    // Copy SQLite（在线备份 API，保证 WAL 一致性）
    let db_name = "data.db";
    let old_db = old.join(db_name);
    let new_db = new.join(db_name);
    if old_db.exists() {
        let src_conn = Connection::open(&old_db)
            .map_err(|e| format!("无法打开源数据库: {e}"))?;
        let mut dst_conn = Connection::open(&new_db)
            .map_err(|e| format!("无法打开目标数据库: {e}"))?;
        let backup = Backup::new(&src_conn, &mut dst_conn)
            .map_err(|e| format!("初始化备份失败: {e}"))?;
        let mut r = backup.step(-1).map_err(|e| format!("备份数据库失败: {e}"))?;
        let mut busy = 0;
        while r == StepResult::Busy || r == StepResult::Locked {
            busy += 1;
            if busy >= 3 {
                return Err("数据库繁忙，迁移未完成，请重试".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            r = backup.step(-1).map_err(|e| format!("备份数据库失败: {e}"))?;
        }
        if r != StepResult::Done {
            return Err("数据库繁忙，迁移未完成，请重试".to_string());
        }
    }

    // Copy index directory
    let old_index = old.join(INDEX_DIR_NAME);
    let new_index = new.join(INDEX_DIR_NAME);
    if old_index.exists() {
        copy_dir_recursive(&old_index, &new_index)?;
    }

    // Copy log file
    let log_name = "app.log";
    let old_log = old.join(log_name);
    let new_log = new.join(log_name);
    if old_log.exists() {
        let _ = std::fs::copy(&old_log, &new_log);
    }

    // Update config
    let mut loaded = load_config();
    loaded.data_dir = new_path.into();
    save_config(&loaded)?;

    Ok("数据已迁移到新目录，即将自动重启".to_string())
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("{e}"))? {
        let entry = entry.map_err(|e| format!("{e}"))?;
        let file_type = entry.file_type().map_err(|e| format!("{e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| format!("{e}"))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}