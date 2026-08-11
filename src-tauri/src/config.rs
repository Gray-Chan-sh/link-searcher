use std::path::PathBuf;
use std::sync::Mutex;
use serde::{Serialize, Deserialize};

const CONFIG_DIR: &str = ".link-searcher";
const CONFIG_FILE: &str = "config.json";

/// Serializes full-file config read-modify-write cycles. Multiple Tauri
/// commands (provider CRUD) can run on different threads; without this,
/// concurrent `load_config`→`save_config` pairs lose updates (last-write-wins).
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// 索引子目录名（Tantivy 数据）。用点开头避免与用户 data_dir 名为 "index" 时撞车。
pub const INDEX_DIR_NAME: &str = ".ls-index";

/// Model role. Classified by name heuristics on pull; user can override.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ModelType {
    Embedding,
    Llm,
    #[default]
    Unknown,
}

impl ModelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Llm => "llm",
            Self::Unknown => "unknown",
        }
    }
}

/// A model cached from a provider's `/v1/models` listing.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ModelConfig {
    pub id: String,
    #[serde(default)]
    pub model_type: ModelType,
}

/// An AI gateway the user manages (base_url + optional api_key). Models are
/// pulled from `GET {base_url}/models` and cached under `models`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

impl ProviderConfig {
    pub fn find_model(&self, model_id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.id == model_id)
    }
}

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
    /// Model-management data: providers + active model selection.
    /// Legacy single fields above are kept for migration; new code resolves
    /// the active model via `active_*_model_id` into a provider.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// `provider_id:model_id` of the embedding model in use.
    #[serde(default)]
    pub active_embedding_model_id: String,
    /// `provider_id:model_id` of the LLM model in use.
    #[serde(default)]
    pub active_llm_model_id: String,
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
            providers: Vec::new(),
            active_embedding_model_id: String::new(),
            active_llm_model_id: String::new(),
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

/// Write config without taking the lock — callers must hold [`CONFIG_LOCK`].
fn write_config_file(config: &AppConfig) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
    let path = dir.join(CONFIG_FILE);
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("{e}"))?;
    std::fs::write(&path, &content).map_err(|e| format!("{e}"))?;
    Ok(())
}

pub fn load_config() -> AppConfig {
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
                let _ = write_config_file(&config);
            }
            if config.providers.is_empty() && migrate_legacy_gateways(&mut config) {
                let _ = write_config_file(&config);
            }
            return config;
        }
    }
    let config = AppConfig::default();
    let _ = write_config_file(&config);
    config
}

/// Seed `providers` from legacy `embedding_*/llm_*` field pairs and point the
/// active ids at them. No-op when neither gateway is set. Runs once on
/// startup; legacy fields stay in the file for old-code compatibility.
fn migrate_legacy_gateways(config: &mut AppConfig) -> bool {
    let mut changed = false;
    if !config.embedding_api_base.is_empty() {
        let provider_id = uuid::Uuid::new_v4().to_string();
        let mut provider = ProviderConfig {
            id: provider_id.clone(),
            name: "默认 Embedding 网关".to_string(),
            base_url: config.embedding_api_base.clone(),
            api_key: config.embedding_api_key.clone(),
            models: Vec::new(),
        };
        if !config.embedding_model.is_empty() {
            provider.models.push(ModelConfig {
                id: config.embedding_model.clone(),
                model_type: ModelType::Embedding,
            });
            config.active_embedding_model_id = format!("{provider_id}:{}", config.embedding_model);
        }
        config.providers.push(provider);
        changed = true;
    }
    if !config.llm_api_base.is_empty() {
        let provider_id = uuid::Uuid::new_v4().to_string();
        let mut provider = ProviderConfig {
            id: provider_id.clone(),
            name: "默认 LLM 网关".to_string(),
            base_url: config.llm_api_base.clone(),
            api_key: config.llm_api_key.clone(),
            models: Vec::new(),
        };
        if !config.llm_model.is_empty() {
            provider.models.push(ModelConfig {
                id: config.llm_model.clone(),
                model_type: ModelType::Llm,
            });
            config.active_llm_model_id = format!("{provider_id}:{}", config.llm_model);
        }
        config.providers.push(provider);
        changed = true;
    }
    changed
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_config_file(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_legacy_gateways_seeds_providers_and_active() {
        let mut config = AppConfig::default();
        config.embedding_api_base = "http://e/v1".into();
        config.embedding_api_key = "k-e".into();
        config.embedding_model = "bge-m3".into();
        config.llm_api_base = "http://l/v1".into();
        config.llm_model = "qwen-7b-instruct".into();

        assert!(migrate_legacy_gateways(&mut config));
        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.providers[0].models.len(), 1);
        assert_eq!(config.providers[0].models[0].model_type, ModelType::Embedding);
        assert_eq!(config.providers[1].models[0].model_type, ModelType::Llm);
        assert!(config.active_embedding_model_id.ends_with(":bge-m3"));
        assert!(config.active_llm_model_id.ends_with(":qwen-7b-instruct"));
    }

    #[test]
    fn migrate_legacy_gateways_noop_when_empty() {
        let mut config = AppConfig::default();
        assert!(!migrate_legacy_gateways(&mut config));
        assert!(config.providers.is_empty());
    }

    #[test]
    fn provider_find_model_matches_by_id() {
        let provider = ProviderConfig {
            id: "p1".into(),
            name: "x".into(),
            base_url: "http://x/v1".into(),
            api_key: String::new(),
            models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Embedding }],
        };
        assert!(provider.find_model("m1").is_some());
        assert!(provider.find_model("ghost").is_none());
    }
}