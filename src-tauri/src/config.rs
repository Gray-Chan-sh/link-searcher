use std::path::{Path, PathBuf};
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

/// Default built-in embedding model: BGE-large-zh-v1.5 (1024-dim). Seeded as
/// the active embedding model once its ONNX files are present in the data dir.
pub const DEFAULT_LOCAL_EMBEDDING_MODEL: &str = "bge-large-zh-v1.5";

/// Model role. Classified by name heuristics on pull; user can override.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ModelType {
    Embedding,
    Llm,
    Reranker,
    #[default]
    Unknown,
}

impl ModelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Llm => "llm",
            Self::Reranker => "reranker",
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
    /// Max output tokens reported by the gateway's `/v1/models` endpoint.
    /// `None` means the value hasn't been detected yet (fallback to default).
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
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

/// Learned remote-embedding plan for one endpoint+model: how many inputs to
/// send per request (`batch`) and how many requests to keep in flight
/// (`conc`). Persisted so a later run starts near the best-known point; the
/// tuner keeps probing at low frequency and backs off on errors.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct EmbedPlan {
    /// Inputs per request.
    pub batch: usize,
    /// Concurrent in-flight requests.
    pub conc: usize,
    /// Best observed throughput (tokens/s) for this endpoint+model.
    #[serde(default)]
    pub tok_per_s: f32,
    /// Unix seconds of the last update.
    #[serde(default)]
    pub updated: u64,
}

impl Default for EmbedPlan {
    fn default() -> Self {
        Self { batch: 64, conc: 1, tok_per_s: 0.0, updated: 0 }
    }
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
    /// `provider_id:model_id` of the reranker model in use. Empty (default)
    /// disables the rerank stage — behaviour is unchanged.
    #[serde(default)]
    pub active_reranker_model_id: String,
    /// `provider_id:model_id` of the LLM model in use.
    #[serde(default)]
    pub active_llm_model_id: String,
    /// 语义 vs 关键词检索权重（0~1，默认 0.3 = 语义30%/关键词70%）。
    /// 语义搜索融合时：score = w×cosine + (1-w)×bm25_norm。
    #[serde(default = "default_semantic_weight")]
    pub semantic_weight: f64,
    /// 迁移后遗留的旧数据目录。迁移时进程仍持有其文件句柄（日志/数据库），
    /// Windows 下无法立即删除，故记录路径，待下次启动（句柄已释放）再清理。
    /// 删除成功后清空；仍失败则保留，下次启动重试。
    #[serde(default)]
    pub pending_cleanup_dir: Option<PathBuf>,
    /// Learned remote-embedding plans, keyed by `"<base_url>|<model_id>"`.
    /// Written by the adaptive tuner (`ai::embed_batched` remote path).
    #[serde(default)]
    pub embed_plans: std::collections::HashMap<String, EmbedPlan>,
    /// Web API Bearer token。空 = 尚未初始化：首次启动生成一次后写回本文件，
    /// 之后每次启动都从这里读（`webapi::resolve_token`），保证 token 跨重启稳定、
    /// 用户可查看/分发。运行期由设置页 / Web 端修改（`set_web_api_token` 同步回写）。
    #[serde(default)]
    pub web_api_token: String,
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
            active_reranker_model_id: String::new(),
            active_llm_model_id: String::new(),
            semantic_weight: 0.3,
            pending_cleanup_dir: None,
            embed_plans: std::collections::HashMap::new(),
            web_api_token: String::new(),
        }
    }
}

pub fn config_file_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

pub fn is_local_embedding_model(model_id: &str) -> bool {
    model_id.starts_with("local:")
}

/// Learned adaptive plan for a remote embedding endpoint, if one was persisted.
pub fn load_embed_plan(key: &str) -> Option<EmbedPlan> {
    load_config().embed_plans.get(key).copied()
}

/// Persist (or refresh) the learned plan for `key`. Full read-modify-write
/// under [`CONFIG_LOCK`] so plan updates and provider CRUD serialize instead of
/// clobbering each other. Best-effort — a failure only logs.
pub fn save_embed_plan(key: &str, plan: EmbedPlan) {
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = config_dir().join(CONFIG_FILE);
    let mut cfg: AppConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    cfg.embed_plans.insert(key.to_string(), plan);
    if let Err(e) = write_config_file(&cfg) {
        log::warn!("[AI] 保存嵌入调优计划失败: {e}");
    }
}

/// Read the Web API token from `config.json`（不做迁移/默认填充，直接读文件）。
pub fn web_api_token() -> String {
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_config_file().web_api_token
}

/// 把 Web API token 写回 `config.json`（加锁的整文件读-改-写，避免与
/// provider CRUD / embed plan 更新互相覆盖）。值未变化时不落盘。
/// 设置页与 Web 端改 token 都会调用，保证配置文件与 DB 中的 token 一致。
pub fn set_web_api_token(token: &str) {
    if token.is_empty() {
        return;
    }
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cfg = read_config_file();
    if cfg.web_api_token == token {
        return;
    }
    cfg.web_api_token = token.to_string();
    if let Err(e) = write_config_file(&cfg) {
        log::warn!("[CONFIG] 写入 web_api_token 失败: {e}");
    }
}

/// 持锁读取配置文件；文件缺失/损坏时返回默认配置。
///
/// 与 [`load_config`] 的区别：这里**不跑迁移、不写盘、不告警**，纯粹给
/// [`web_api_token`] / [`set_web_api_token`] 这类"已经持锁、只要值"的调用方用
/// （走 `load_config` 会在迁移分支里再写一次文件，既浪费也可能自递归）。
fn read_config_file() -> AppConfig {
    let path = config_dir().join(CONFIG_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
        .unwrap_or_default()
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

/// Default data directory (`data.db`, `.ls-index`, `models/`, …).
///
/// Uses the *local* per-user data dir rather than the roaming one: on Windows
/// this is `%LOCALAPPDATA%`, so the multi-GB model files are not synced by
/// domain roaming profiles. macOS/Linux resolve to the same path as
/// `data_dir()`.
fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("link-searcher")
}

/// Write config without taking the lock — callers must hold [`CONFIG_LOCK`].
fn write_config_file(config: &AppConfig) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
    let path = dir.join(CONFIG_FILE);
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("{e}"))?;
    write_config_atomic(&path, &content)
}

/// 原子写：写同目录临时文件 → `fsync` → `rename` 覆盖目标。
///
/// 与 `commands/ai/session.rs::write_history_unlocked` 同理：读者要么看到旧的完整
/// 文件、要么看到新的完整文件，**永远不会读到半截 JSON**。旧实现是
/// `truncate(true)`（先清空再写），写入期间文件是不完整的 —— 此刻进程被杀/磁盘写满，
/// 旧数据也一并丢失，而 `load_config` 会把解析失败当成"配置不存在"。
///
/// 四个与直觉不同、必须守住的细节：
///
/// 1. **临时文件必须与目标同目录**：跨文件系统的 `rename` 会失败或退化成
///    "复制 + 删除"，原子性随之消失（所以不能用系统临时目录）。
/// 2. **临时文件名带 pid**：进程内的 [`CONFIG_LOCK`] 挡不住第二个进程
///    （CLI / headless / bot sidecar），固定名会让两个进程写同一个临时文件而互相撕碎。
/// 3. **unix 权限必须在创建临时文件时就设 0600**：否则 `rename` 之后
///    `config.json` 会从 0600 变成默认的 0644 —— 而它装着 AI `api_key` 与
///    `web_api_token`。这是"修一个洞、开一个洞"的典型，务必守住。
/// 4. **Windows 上退避重试**：AV/索引器可能短暂持有目标文件，`rename` 会以
///    sharing violation 失败；重试期间旧文件始终完整，最终失败也不破坏它。
fn write_config_atomic(path: &Path, content: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(CONFIG_FILE);
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));

    // ① 写临时文件（同目录、0600、落盘）
    let written = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        std::io::Write::write_all(&mut file, content.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("写入临时配置文件失败: {e}"));
    }

    // ② rename 覆盖（Windows：退避重试，见上文第 4 点）
    let mut last_err: Option<String> = None;
    for attempt in 0..5u32 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                last_err = Some(e.to_string());
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt + 1)));
                }
            }
        }
    }
    if let Some(e) = last_err {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("替换配置文件失败: {e}"));
    }

    // ③ 目录 fsync（仅 POSIX）：让 rename 本身也持久化；Windows 打不开目录。
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// `AppConfig` 的顶层键，用于在把手写错的键名**告警**而不是静默忽略。
///
/// 不硬用 `#[serde(deny_unknown_fields)]`：那会让一个拼错的键导致**整份配置被拒**，
/// 而当前没有 `doctor --fix` 之类的修复入口，用户会看到"配置全没了"。
/// 等有了修复路径再切换成硬拒绝（见 `docs/LARK_BOT.md`）。
/// 与 `AppConfig` 字段的同步由一个测试守着。
const KNOWN_CONFIG_KEYS: &[&str] = &[
    "data_dir",
    "language",
    "ai_api_base",
    "ai_api_key",
    "embedding_api_base",
    "embedding_api_key",
    "embedding_model",
    "llm_api_base",
    "llm_api_key",
    "llm_model",
    "providers",
    "active_embedding_model_id",
    "active_reranker_model_id",
    "active_llm_model_id",
    "semantic_weight",
    "pending_cleanup_dir",
    "embed_plans",
    "web_api_token",
];

/// 手改了 `config.json` 但键名拼错时给出告警。`$schema` 是唯一允许的例外
/// （方便编辑器挂 JSON Schema）。
fn warn_unknown_config_keys(content: &str) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(content)
    else {
        return;
    };
    let unknown: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "$schema" && !KNOWN_CONFIG_KEYS.contains(k))
        .collect();
    if !unknown.is_empty() {
        log::warn!(
            "[CONFIG] config.json 有 {} 个未知键，将被忽略（键名拼错了？）: {}",
            unknown.len(),
            unknown.join(", ")
        );
    }
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 读配置的结果。
enum ConfigRead {
    /// 解析成功。
    Loaded(AppConfig),
    /// 没有可用配置，但**可以**安全写出一份默认配置：文件不存在（首次运行），
    /// 或者损坏件已经另存成功。
    Recreatable,
    /// 没有可用配置，且**原文件仍在**（读取失败 / 另存失败）——**绝不要覆盖它**，
    /// 否则会把用户唯一的一份现场销毁掉。
    KeepOriginal,
}

/// 读并解析 `config.json`。
///
/// - **文件不存在** → [`ConfigRead::Recreatable`]（首次运行，正常路径）
/// - **解析失败** → 另存为 `config.json.corrupt.<unix_ts>` 后 [`Recreatable`]；
///   另存失败则 [`KeepOriginal`]。旧实现是直接 `write_config_file(&default)`，
///   等于**当场销毁现场** —— 用户连"我原来写了什么"都看不到。
/// - **读取失败**（权限/IO）→ [`KeepOriginal`]，且**绝不触碰**原文件。
fn read_config_checked(path: &Path) -> ConfigRead {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ConfigRead::Recreatable,
        Err(e) => {
            log::error!(
                "[CONFIG] 读取 {} 失败: {e}；本次以默认配置运行，且不会覆盖该文件",
                path.display()
            );
            return ConfigRead::KeepOriginal;
        }
    };

    match serde_json::from_str::<AppConfig>(&content) {
        Ok(config) => {
            warn_unknown_config_keys(&content);
            ConfigRead::Loaded(config)
        }
        Err(e) => {
            let backup = path.with_extension(format!("json.corrupt.{}", unix_ts()));
            match std::fs::rename(path, &backup) {
                Ok(()) => {
                    log::error!(
                        "[CONFIG] 解析 {} 失败: {e}；已另存为 {} 并以默认配置继续",
                        path.display(),
                        backup.display()
                    );
                    ConfigRead::Recreatable
                }
                Err(rename_err) => {
                    log::error!(
                        "[CONFIG] 解析 {} 失败: {e}；另存为 {} 也失败: {rename_err}；保持原文件不动",
                        path.display(),
                        backup.display()
                    );
                    ConfigRead::KeepOriginal
                }
            }
        }
    }
}

pub fn load_config() -> AppConfig {
    let _g = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = config_dir().join(CONFIG_FILE);

    let mut config = match read_config_checked(&path) {
        ConfigRead::Loaded(config) => config,
        ConfigRead::Recreatable => {
            // 首次运行，或损坏件已另存 → 安全地写出一份默认配置。
            let fresh = AppConfig::default();
            let _ = write_config_file(&fresh);
            return fresh;
        }
        // 读不到原文件但也不能安全重建（权限 / 另存失败）：只用内存默认值，
        // 一个字都不写，把用户的文件原样留着。
        ConfigRead::KeepOriginal => return AppConfig::default(),
    };

    if let Ok(override_dir) = std::env::var("LINK_SEARCHER_DATA_DIR") {
        config.data_dir = override_dir.into();
    } else if config.data_dir.as_os_str().is_empty() {
        config.data_dir = default_data_dir();
    }
    // 评测用开关：允许环境变量覆盖语义权重，便于对 semantic_weight
    // 跑 A/B（scripts/eval/run_rag_eval.sh），无需改动用户 config.json。
    if let Ok(w) = std::env::var("LINK_SEARCHER_SEMANTIC_WEIGHT")
        && let Ok(v) = w.trim().parse::<f64>()
    {
        config.semantic_weight = v.clamp(0.0, 1.0);
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
    if seed_default_embedding_model(&mut config) {
        let _ = write_config_file(&config);
    }
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

/// Seed the built-in BGE-large embedding model as the active embedding model
/// for a fresh install (or any config with no embedding selected) once its
/// files exist on disk. Only fills an *empty* selection, so an explicit remote
/// gateway choice is never overwritten. Returns true when it wrote a change.
fn seed_default_embedding_model(config: &mut AppConfig) -> bool {
    // Skip under the test backdoor: tests point LS_CONFIG_DIR at a temp dir but
    // keep the real data_dir, so a locally-installed model would leak in.
    if std::env::var("LS_CONFIG_DIR").is_ok() {
        return false;
    }
    if !config.active_embedding_model_id.is_empty() {
        return false;
    }
    let ready = config
        .data_dir
        .join("models")
        .join(DEFAULT_LOCAL_EMBEDDING_MODEL)
        .join("model.onnx")
        .is_file();
    if !ready {
        return false;
    }
    config.active_embedding_model_id = format!("local:{DEFAULT_LOCAL_EMBEDDING_MODEL}");
    log::info!("[AI] 默认内置嵌入模型已启用: {}", config.active_embedding_model_id);
    true
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
                max_output_tokens: None,
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
                max_output_tokens: None,
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

/// 删除一个迁移遗留目录（纯逻辑，便于测试）。返回仍需下次重试的路径：
/// `None` 表示已删除 / 无需删除；`Some` 表示删除失败需保留。
/// 安全护栏：绝不删除当前活动数据目录，也不删除其父目录。
fn try_cleanup(pending: &Path, current: &Path) -> Option<PathBuf> {
    if pending == current || current.starts_with(pending) || !pending.exists() {
        return None;
    }
    match std::fs::remove_dir_all(pending) {
        Ok(()) => {
            log::info!("[MIGRATE] cleaned up old data dir {:?}", pending);
            None
        }
        Err(e) => {
            log::warn!(
                "[MIGRATE] still failed to clean up old data dir {:?}: {e}",
                pending
            );
            Some(pending.to_path_buf())
        }
    }
}

/// 启动时清理迁移遗留的旧数据目录。上一次迁移进程仍持有旧目录的日志/数据库
/// 句柄，Windows 下删除会失败；此刻新进程尚未打开旧目录，是安全窗口。
/// 删除成功（或目录已不存在）则清空待删标记，失败则保留供下次启动重试。
pub fn run_pending_cleanup(current_data_dir: &Path) {
    let mut config = load_config();
    let Some(pending) = config.pending_cleanup_dir.clone() else {
        return;
    };
    if try_cleanup(&pending, current_data_dir).is_none() {
        config.pending_cleanup_dir = None;
        let _ = save_config(&config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp `config.json` path in its own directory (per test).
    fn tmp_config_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ls_cfg_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(CONFIG_FILE)
    }

    fn leftover_tmp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_tmp() {
        let path = tmp_config_path("atomic");
        write_config_atomic(&path, r#"{"language":"zh"}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"language":"zh"}"#);

        // Overwrite an existing file.
        write_config_atomic(&path, r#"{"language":"en"}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"language":"en"}"#);

        let leftovers = leftover_tmp_files(path.parent().unwrap());
        assert!(leftovers.is_empty(), "no temp files may remain: {leftovers:?}");
    }

    /// 替换失败时：目标保持原样，且临时文件被清掉（不留垃圾）。
    #[test]
    fn atomic_write_failure_keeps_target_and_cleans_tmp() {
        let path = tmp_config_path("atomic_fail");
        // 目标位置先占一个**目录** → rename 必定失败（两个平台都是）。
        std::fs::create_dir_all(&path).unwrap();

        let err = write_config_atomic(&path, "{}").unwrap_err();
        assert!(err.contains("替换配置文件失败"), "unexpected error: {err}");
        assert!(path.is_dir(), "the existing entry must be left untouched");

        let leftovers = leftover_tmp_files(path.parent().unwrap());
        assert!(leftovers.is_empty(), "tmp must be cleaned up: {leftovers:?}");
    }

    /// 守着手写清单：`KNOWN_CONFIG_KEYS` 一旦与 `AppConfig` 字段脱节，这个测试就红。
    #[test]
    fn known_config_keys_stay_in_sync_with_appconfig() {
        let value = serde_json::to_value(AppConfig::default()).unwrap();
        let mut actual: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        actual.sort();
        let mut expected: Vec<String> =
            KNOWN_CONFIG_KEYS.iter().map(|s| (*s).to_string()).collect();
        expected.sort();
        assert_eq!(
            actual, expected,
            "KNOWN_CONFIG_KEYS 与 AppConfig 的字段不同步（新增字段后要同步更新）"
        );
    }

    /// 手改写错键名：只告警，**不能**因此拒掉整份配置。
    #[test]
    fn unknown_config_keys_are_tolerated_not_fatal() {
        let path = tmp_config_path("unknown_key");
        std::fs::write(&path, r#"{"language":"en","llm_apikey":"typo"}"#).unwrap();

        match read_config_checked(&path) {
            ConfigRead::Loaded(config) => assert_eq!(config.language, "en"),
            _ => panic!("一个拼错的键名不得让整份配置被拒"),
        }
        assert!(path.exists(), "known-good config must stay in place");
    }

    /// 损坏配置：另存备查 + 可重建，而不是当场被默认值覆盖。
    #[test]
    fn corrupt_config_is_archived_and_can_be_recreated() {
        let path = tmp_config_path("corrupt");
        std::fs::write(&path, "{ this is not json").unwrap();

        match read_config_checked(&path) {
            ConfigRead::Recreatable => {}
            _ => panic!("corrupt config must be reported as recreatable"),
        }
        assert!(!path.exists(), "corrupt file must be moved aside, not truncated in place");

        let dir = path.parent().unwrap();
        let archived: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("corrupt"))
            .collect();
        assert_eq!(archived.len(), 1, "exactly one archive expected: {archived:?}");
        let content = std::fs::read_to_string(dir.join(&archived[0])).unwrap();
        assert_eq!(content, "{ this is not json", "archive must keep the original bytes");
    }

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
        let mk = |id: &str, ty: ModelType| ModelConfig { id: id.into(), model_type: ty, enabled: false, max_output_tokens: None };
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
                    ModelConfig { id: "m1".into(), model_type: ModelType::Embedding, enabled: false, max_output_tokens: None },
                    ModelConfig { id: "m2".into(), model_type: ModelType::Llm, enabled: false, max_output_tokens: None },
                    ModelConfig { id: "m3".into(), model_type: ModelType::Llm, enabled: true, max_output_tokens: None },
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
    fn pending_cleanup_removes_stale_dir_and_clears_flag() {
        let base = std::env::temp_dir().join(format!("ls_cleanup_{}", uuid::Uuid::new_v4()));
        let old = base.join("old");
        let current = base.join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(old.join("data.db"), b"x").unwrap();

        assert!(try_cleanup(&old, &current).is_none(), "stale dir must be deleted");
        assert!(!old.exists(), "old dir should be gone");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pending_cleanup_never_deletes_active_or_parent_dir() {
        let base = std::env::temp_dir().join(format!("ls_cleanup_{}", uuid::Uuid::new_v4()));
        let current = base.join("data");
        std::fs::create_dir_all(&current).unwrap();

        assert!(try_cleanup(&current, &current).is_none());
        assert!(current.exists(), "active dir must survive");

        assert!(try_cleanup(&base, &current).is_none());
        assert!(base.exists(), "parent of active dir must survive");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn provider_find_model_matches_by_id() {
        let provider = ProviderConfig {
            id: "p1".into(),
            name: "x".into(),
            base_url: "http://x/v1".into(),
            api_key: String::new(),
            models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Embedding, enabled: false, max_output_tokens: None }],
        };
        assert!(provider.find_model("m1").is_some());
        assert!(provider.find_model("ghost").is_none());
    }
}