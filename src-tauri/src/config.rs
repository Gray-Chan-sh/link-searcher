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
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
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
    /// User opted this model into the enabled set: it shows in the quick
    /// list and becomes eligible for the active embedding/LLM selection.
    #[serde(default)]
    pub enabled: bool,
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

/// Out-of-the-box default: enable the first Embedding and the first Llm
/// model of a freshly pulled list, so a new provider is usable without
/// hunting through the model list. Order of the input list is preserved.
pub fn auto_enable_first_per_type(models: Vec<ModelConfig>) -> Vec<ModelConfig> {
    let mut enabled = std::collections::HashSet::new();
    models
        .into_iter()
        .map(|mut m| {
            if matches!(m.model_type, ModelType::Embedding | ModelType::Llm)
                && enabled.insert(m.model_type)
            {
                m.enabled = true;
            }
            m
        })
        .collect()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub language: String,
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
    /// 语义 vs 关键词检索权重（0~1，默认 0.3 = 语义30%/关键词70%）。
    /// 语义搜索融合时：score = w×cosine + (1-w)×bm25_norm。
    #[serde(default = "default_semantic_weight")]
    pub semantic_weight: f64,
}

fn default_semantic_weight() -> f64 {
    0.3
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            language: "zh".to_string(),
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
            semantic_weight: 0.3,
        }
    }
}

pub fn config_file_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

pub fn is_local_embedding_model(model_id: &str) -> bool {
    model_id.starts_with("local:")
}

fn config_dir() -> PathBuf {
    // 测试后门：LS_CONFIG_DIR 覆盖配置目录，避免 wire 测试读写真实用户配置。
    if let Ok(dir) = std::env::var("LS_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut file = opts.open(&path).map_err(|e| format!("{e}"))?;
        std::io::Write::write_all(&mut file, content.as_bytes()).map_err(|e| format!("{e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &content).map_err(|e| format!("{e}"))?;
    }
    Ok(())
}

pub fn load_config() -> AppConfig {
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = config_dir().join(CONFIG_FILE);
    if let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(mut config) = serde_json::from_str::<AppConfig>(&content) {
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
            reconcile_enabled_active(&mut config);
            return config;
        }
    let config = AppConfig::default();
    let _ = write_config_file(&config);
    config
}

/// Ensure the models referenced by the active ids are enabled. Configs
/// saved before the `enabled` flag existed deserialize with everything
/// disabled; the active selection must stay visible in the enabled set.
fn reconcile_enabled_active(config: &mut AppConfig) {
    for active in [&config.active_embedding_model_id, &config.active_llm_model_id] {
        if let Some((pid, mid)) = active.split_once(':')
            && let Some(p) = config.providers.iter_mut().find(|p| p.id == pid)
                && let Some(m) = p.models.iter_mut().find(|m| m.id == mid) {
                    m.enabled = true;
                }
    }
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
                enabled: true,
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
                enabled: true,
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
        assert!(config.providers[0].models[0].enabled, "legacy active model must be enabled");
        assert_eq!(config.providers[1].models[0].model_type, ModelType::Llm);
        assert!(config.providers[1].models[0].enabled, "legacy active model must be enabled");
        assert!(config.active_embedding_model_id.ends_with(":bge-m3"));
        assert!(config.active_llm_model_id.ends_with(":qwen-7b-instruct"));
    }

    #[test]
    fn auto_enable_first_per_type_enables_one_embedding_and_one_llm() {
        let mk = |id: &str, ty: ModelType| ModelConfig { id: id.into(), model_type: ty, enabled: false };
        let models = vec![
            mk("llm-a", ModelType::Llm),
            mk("emb-a", ModelType::Embedding),
            mk("llm-b", ModelType::Llm),
            mk("unknown-a", ModelType::Unknown),
            mk("emb-b", ModelType::Embedding),
        ];
        let out = auto_enable_first_per_type(models);
        let enabled: Vec<&str> = out.iter().filter(|m| m.enabled).map(|m| m.id.as_str()).collect();
        assert_eq!(enabled, vec!["llm-a", "emb-a"], "first of each role only");
    }

    #[test]
    fn model_config_deserializes_legacy_json_without_enabled() {
        let legacy = r#"{"id":"m1","model_type":"Llm"}"#;
        let m: ModelConfig = serde_json::from_str(legacy).unwrap();
        assert!(!m.enabled, "legacy models default to disabled");
    }

    #[test]
    fn semantic_weight_defaults_to_0_3_and_round_trips() {
        let mut c = AppConfig::default();
        assert_eq!(c.semantic_weight, 0.3, "default must be 0.3");
        c.semantic_weight = 0.7;
        let json = serde_json::to_string(&c).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.semantic_weight, 0.7, "round-trip must preserve");
        // 旧配置无该字段 → 默认 0.3
        let legacy = r#"{"data_dir":"/tmp/x"}"#;
        let old: AppConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(old.semantic_weight, 0.3, "legacy config defaults to 0.3");
    }

    #[test]
    fn reconcile_enabled_active_keeps_active_models_visible() {
        let mut config = AppConfig {
            providers: vec![ProviderConfig {
                id: "p1".into(),
                name: "x".into(),
                base_url: "http://x/v1".into(),
                api_key: String::new(),
                models: vec![
                    ModelConfig { id: "m1".into(), model_type: ModelType::Embedding, enabled: false },
                    ModelConfig { id: "m2".into(), model_type: ModelType::Llm, enabled: false },
                    ModelConfig { id: "m3".into(), model_type: ModelType::Llm, enabled: true },
                ],
            }],
            active_embedding_model_id: "p1:m1".into(),
            active_llm_model_id: "p1:ghost".into(),
            ..AppConfig::default()
        };
        reconcile_enabled_active(&mut config);
        let models = &config.providers[0].models;
        assert!(models[0].enabled, "active embedding model must be enabled");
        assert!(!models[1].enabled, "unrelated model stays disabled");
        assert!(models[2].enabled, "already-enabled model unchanged");
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
            models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Embedding, enabled: false }],
        };
        assert!(provider.find_model("m1").is_some());
        assert!(provider.find_model("ghost").is_none());
    }
}