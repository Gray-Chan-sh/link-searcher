use std::path::PathBuf;
use serde::{Serialize, Deserialize};

const CONFIG_DIR: &str = ".link-searcher";
const CONFIG_FILE: &str = "config.json";

/// 索引子目录名（Tantivy 数据）。用点开头避免与用户 data_dir 名为 "index" 时撞车。
pub const INDEX_DIR_NAME: &str = ".ls-index";

#[derive(Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub language: String,
    pub lo_binary_path: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            language: "zh".to_string(),
            // Empty = auto-detect; macOS resolves brew/App install paths at runtime.
            lo_binary_path: String::new(),
        }
    }
}

pub fn config_file_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR)
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("link-searcher")
}

pub fn load_config() -> AppConfig {
    let path = config_dir().join(CONFIG_FILE);
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(mut config) = serde_json::from_str::<AppConfig>(&content) {
            if config.data_dir.as_os_str().is_empty() {
                config.data_dir = default_data_dir();
            }
            return config;
        }
    }
    let config = AppConfig::default();
    let _ = save_config(&config);
    config
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
    let path = dir.join(CONFIG_FILE);
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("{e}"))?;
    std::fs::write(&path, &content).map_err(|e| format!("{e}"))?;
    Ok(())
}