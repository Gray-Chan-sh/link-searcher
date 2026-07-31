use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use crate::config::{AppConfig, load_config, save_config};

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
    let existing_index = new.join("index");
    if existing_index.exists() {
        return Err("目标目录已包含 index 文件夹，请选择空目录或新目录".to_string());
    }

    std::fs::create_dir_all(new).map_err(|e| format!("无法创建目标目录: {e}"))?;

    // Copy SQLite
    let db_name = "data.db";
    let old_db = old.join(db_name);
    let new_db = new.join(db_name);
    if old_db.exists() {
        std::fs::copy(&old_db, &new_db).map_err(|e| format!("无法复制数据库: {e}"))?;
    }

    // Copy index directory
    let index_name = "index";
    let old_index = old.join(index_name);
    let new_index = new.join(index_name);
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