use std::path::PathBuf;
use serde::{Serialize, Deserialize};

const CONFIG_DIR: &str = ".link-searcher";
const CONFIG_FILE: &str = "config.json";

/// 索引子目录名（Tantivy 数据）。用点开头避免与用户 data_dir 名为 "index" 时撞车。
pub const INDEX_DIR_NAME: &str = ".ls-index";

#[derive(Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub lo_binary_path: String,
    /// Legacy single-gateway fields (merged into embedding/llm below).
    #[serde(default)]
    pub ai_api_base: String,
    #[serde(default)]
    pub ai_api_key: String,
    /// Embedding gateway (semantic search). Empty base = feature off.
    #[serde(default)]
    pub embedding_api_base: String,
    #[serde(default)]
    pub embedding_api_key: String,
    #[serde(default)]
    pub embedding_model: String,
    /// LLM gateway (AI summary / RAG Q&A). Empty base = feature off.
    #[serde(default)]
    pub llm_api_base: String,
    #[serde(default)]
    pub llm_api_key: String,
    #[serde(default)]
    pub llm_model: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            language: "zh".to_string(),
            // Empty = auto-detect; macOS resolves brew/App install paths at runtime.
            lo_binary_path: String::new(),
            ai_api_base: String::new(),
            ai_api_key: String::new(),
            embedding_api_base: String::new(),
            embedding_api_key: String::new(),
            embedding_model: "text-embedding-v3-small".to_string(),
            llm_api_base: String::new(),
            llm_api_key: String::new(),
            llm_model: "qwen2.5-7b-instruct".to_string(),
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
            // Migrate the legacy single-gateway config: if the user only ever
            // set ai_api_base/key, apply it to BOTH new gateways so the old
            // behaviour (one gateway for everything) keeps working.
            if config.embedding_api_base.is_empty()
                && config.llm_api_base.is_empty()
                && !config.ai_api_base.is_empty()
            {
                config.embedding_api_base = config.ai_api_base.clone();
                config.embedding_api_key = config.ai_api_key.clone();
                config.llm_api_base = config.ai_api_base.clone();
                config.llm_api_key = config.ai_api_key.clone();
                let _ = save_config(&config);
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