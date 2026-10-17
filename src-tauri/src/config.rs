use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fs4::FileExt;
use serde::{Serialize, Deserialize};

const CONFIG_DIR: &str = ".link-searcher";
const CONFIG_FILE: &str = "config.json";
/// 跨进程写锁的锁文件（与 `config.json` 同目录）。
const CONFIG_LOCK_FILE: &str = "config.lock";
/// 等跨进程锁的上限；超时后**降级**而不是无限等（见 [`lock_config`]）。
const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(2);
/// 等锁时的轮询间隔。
const CONFIG_LOCK_POLL: Duration = Duration::from_millis(20);

/// **进程内**串行化配置的整文件读-改-写。多个 Tauri 命令（provider CRUD）可能落在
/// 不同线程上；没有它，并发的 `load_config`→`save_config` 会丢更新（last-write-wins）。
///
/// 它同时是**外层**锁：同一时刻只允许一个线程去碰跨进程锁句柄。这不是形式主义 ——
/// `fs4` 的锁绑在**句柄**上，同进程多线程共用一个句柄时 `try_lock_exclusive` 会直接
/// 成功，**并不构成互斥**。真正的进程内互斥由这层 Mutex 提供。
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// 跨进程锁文件句柄。**全进程共用一个**（理由见 [`lock_handle`]）。
static CONFIG_LOCK_HANDLE: OnceLock<File> = OnceLock::new();

/// 本进程**已知**的 `config.json` 内容 hash（自己写的，或已接受的外部改动）。
/// 热重载监听靠它区分"自己的写"与"外部手改"（见 [`observe_config_content`]）。
static KNOWN_CONFIG_HASH: Mutex<Option<String>> = Mutex::new(None);

/// 本进程**已知**的飞书策略快照：一次外部改动**只有真动了这一块**才需要重启 bot
/// （bot 启动时把策略读进了内存，不会自己发现文件变了）。
static KNOWN_LARK: Mutex<Option<LarkIntegration>> = Mutex::new(None);

/// 配置写锁的守卫：Drop 时释放跨进程锁。
struct ConfigGuard {
    /// 进程内互斥（外层）。只为持有，不读其值。
    _process: std::sync::MutexGuard<'static, ()>,
    /// 本次是否真的拿到了跨进程锁。`false` ＝ **降级**（只保证进程内串行化）。
    cross_process: bool,
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        if !self.cross_process {
            return;
        }
        if let Some(file) = CONFIG_LOCK_HANDLE.get()
            && let Err(e) = file.unlock()
        {
            log::warn!("[CONFIG] 释放跨进程配置锁失败: {e}");
        }
    }
}

/// 锁文件句柄（`OnceLock` ⇒ 全进程**共用同一个**）。
///
/// ⚠️ **必须共用同一个句柄**：`fs4` 在 Windows 走 `LockFileEx`、在 Unix 走 `flock`，
/// 两者的锁都**绑在句柄 / 打开文件描述上**。若每次加锁都新开一个 fd，同进程的第二次
/// 加锁会**把自己挡住**（`flock` 对不同 fd 互斥、`LockFileEx` 也不同句柄互斥）——
/// 而 `load_config` 的迁移分支正是"持锁时再写文件"这条会嵌套的路径。
fn lock_handle() -> Option<&'static File> {
    if let Some(f) = CONFIG_LOCK_HANDLE.get() {
        return Some(f);
    }
    let dir = config_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("[CONFIG] 创建配置目录失败，跨进程锁降级: {e}");
        return None;
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(CONFIG_LOCK_FILE))
    {
        Ok(f) => f,
        Err(e) => {
            log::warn!("[CONFIG] 打开配置锁文件失败，跨进程锁降级: {e}");
            return None;
        }
    };
    // 并发首次：谁先 set 谁赢；输的那个丢掉自己的句柄（指向同一文件，语义等价）。
    let _ = CONFIG_LOCK_HANDLE.set(file);
    CONFIG_LOCK_HANDLE.get()
}

/// 在给定句柄上尝试取得**跨进程**排他锁（有界重试）。抽出句柄/时长参数是为了可单测。
///
/// 返回 `false` 表示**降级**（拿不到句柄，或对方持锁超过 `wait`）——调用方继续，
/// 只保证进程内串行化。见 [`lock_config`] 为何不无限等。
fn try_lock_with(file: Option<&File>, wait: Duration, poll: Duration) -> bool {
    let Some(file) = file else {
        return false;
    };
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return true,
            Err(e) => {
                if Instant::now() >= deadline {
                    log::warn!(
                        "[CONFIG] 等跨进程配置锁超时（{wait:?}）：本次降级为进程内串行化，继续写。原因: {e}"
                    );
                    return false;
                }
                std::thread::sleep(poll);
            }
        }
    }
}

/// 取得配置写锁：**进程内 Mutex（外层）+ 跨进程 advisory 锁（内层）**。
///
/// 所有"整文件读-改-写"的入口都要在**整个 RMW 期间**持有它 —— 只锁"写"是不够的：
/// A 读 / B 读 / A 写 / B 写，B 仍会覆盖 A 的改动（**原子的写照样能吞掉另一个原子的写**）。
///
/// 为什么用 `try_lock_exclusive` + **有界**重试，而不是阻塞式 `lock_exclusive`：
/// 若另一个进程（CLI / headless）卡住不放，无限等会把**界面线程**冻住。宁可在超时后
/// 记一条 WARN 并降级继续写，也不让界面无响应 —— 跨进程锁是"防两进程互相覆盖"的
/// **加固**，不是正确性的唯一来源（正确性由原子写 + RMW 保证）。
fn lock_config() -> ConfigGuard {
    let process = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cross_process = try_lock_with(lock_handle(), CONFIG_LOCK_WAIT, CONFIG_LOCK_POLL);
    ConfigGuard {
        _process: process,
        cross_process,
    }
}

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

/// 对外集成（飞书等）的配置块。
///
/// 放 `config.json` 而不是 DB `app_settings`：这是 §5.6「配置写入」的一次收口 ——
/// 飞书那条链路的策略此前是 6 个平铺的 DB 字符串键，散落且无法整体备份/审阅。
/// `#[serde(default)]`（容器级）保证**老 `config.json` 原样可加载**：
/// 缺 `integrations` 就用默认值，不会因为读不懂就把整份配置判为损坏。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Integrations {
    pub lark: LarkIntegration,
}

/// 飞书机器人策略（`docs/LARK_BOT.md` §7.1 ⑤）。
///
/// ⚠️ **空数组语义方向相反，别"顺手统一"**（§7.1 ②）：
/// - `allowed_users` / `allowed_chats` 空 ⇒ **拒绝全部**（fail-closed）
/// - `export_allow_dirs` 空 ⇒ **不限制**（fail-open）
///
/// `enabled` 默认 **false**，与 `web_api_enabled` 的"默认开"**刻意相反**：
/// 把本地私有库的内容发到飞书必须显式开启。
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(default)]
pub struct LarkIntegration {
    pub enabled: bool,
    /// L2：是否要求私聊（非私聊一律拒绝）。
    pub require_p2p: bool,
    /// L1：允许触发机器人的 `open_id`；**空 ⇒ 拒绝全部**。
    pub allowed_users: Vec<String>,
    /// `chat_id` 白名单。**当前不参与判定**（§7.1 ④），保留键名以备将来支持群。
    pub allowed_chats: Vec<String>,
    /// 内容外发收紧阀；**空 ⇒ 不限制**。
    pub export_allow_dirs: Vec<String>,
    /// 冷启动配对模式（§7.1 ⑥）。
    pub pairing: bool,
    /// 配对模式的**截止时刻**（unix 秒）。
    ///
    /// **`0` 或已经过去都算失效**（见 [`LarkIntegration::pairing_active`]）——
    /// fail-closed：忘记设截止时刻＝没开配对，而不是"永远有效"
    /// （长期开着等于没有白名单）。
    pub pairing_expires_at: i64,
}

impl LarkIntegration {
    /// 规整化：去掉空白项、去重，并维持"**没在配对就没有截止时刻**"这个不变量。
    ///
    /// 由 [`set_lark_integration`] 统一执行 —— 所有写入口（设置页 / Web API /
    /// 一次性迁移 / 手改后的热重载）都汇到那里，调用方**不必各自记得**清理。
    pub fn sanitized(mut self) -> Self {
        for list in [
            &mut self.allowed_users,
            &mut self.allowed_chats,
            &mut self.export_allow_dirs,
        ] {
            let mut seen = std::collections::HashSet::new();
            list.retain_mut(|v| {
                let trimmed = v.trim().to_string();
                if trimmed.is_empty() {
                    return false;
                }
                *v = trimmed.clone();
                seen.insert(trimmed)
            });
        }
        if !self.pairing {
            self.pairing_expires_at = 0;
        }
        self
    }

    /// 配对是否**此刻有效**（§7.1 ⑥）。
    ///
    /// **fail-closed**：`pairing_expires_at <= now` 一律无效 —— **包括 `0`**。
    /// 于是"配对必须带一个将来的截止时刻"，漏设＝没开，而不是永久敞开。
    pub fn pairing_active(&self, now: i64) -> bool {
        self.pairing && self.pairing_expires_at > now
    }
}

impl Default for LarkIntegration {
    fn default() -> Self {
        // 安全默认：关闭 + 要求私聊 + 空名单（＝谁都不许）。
        Self {
            enabled: false,
            require_p2p: true,
            allowed_users: Vec::new(),
            allowed_chats: Vec::new(),
            export_allow_dirs: Vec::new(),
            pairing: false,
            pairing_expires_at: 0,
        }
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
    /// WebUI 对**外部设备**（手机 / 其它电脑）的基址，例如
    /// `https://100.101.102.101:8443`（Tailscale）或 `https://link.example.com`。
    /// 空 = 自动：飞书 bot 优先用 Tailscale 地址，其次本机默认路由的局域网 IP，
    /// 并会在文案里附上另一个可用地址（见 `docs/LARK_BOT.md` §5.13）。
    /// 只在 `config.json` 里手改即可（B 档：配置文件为中心）；改动热加载生效
    /// （每次现读，无需重启）。
    #[serde(default)]
    pub webui_base_url: String,
    /// 对外集成（飞书机器人等）。见 [`Integrations`]。
    #[serde(default)]
    pub integrations: Integrations,
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
            webui_base_url: String::new(),
            integrations: Integrations::default(),
        }
    }
}

/// `config.json` 的绝对路径（备份 / 热重载监听等使用）。
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
/// under [`lock_config`]（进程内 + 跨进程）so plan updates and provider CRUD
/// serialize instead of clobbering each other. Best-effort — a failure only logs.
pub fn save_embed_plan(key: &str, plan: EmbedPlan) {
    let _g = lock_config();
    // ⚠️ 解析失败时必须**放弃写入**：这里原本回退 `AppConfig::default()`，等于"读不懂就整份
    // 重写"，一次手改到一半的 config.json 会被默认值冲掉（见 [`read_config_for_write`]）。
    let Some(mut cfg) = read_config_for_write() else {
        return;
    };
    cfg.embed_plans.insert(key.to_string(), plan);
    if let Err(e) = write_config_file(&cfg) {
        log::warn!("[AI] 保存嵌入调优计划失败: {e}");
    }
}

/// Read the Web API token from `config.json`（不做迁移/默认填充，直接读文件）。
pub fn web_api_token() -> String {
    let _g = lock_config();
    read_config_file().web_api_token
}

/// 只要 `webui_base_url` 的值：**不跑迁移、不加锁** —— 供 `/api/session` 这类
/// "每次请求读一下"的只读路径使用（`load_config` 会走迁移分支并持跨进程锁，
/// 对高频只读不划算）。解析失败 → 回退空串（= 自动选地址），与"读不懂就当没配"一致。
pub fn webui_base_url() -> String {
    read_config_file().webui_base_url
}

/// 把 Web API token 写回 `config.json`（加锁的整文件读-改-写，避免与
/// provider CRUD / embed plan 更新互相覆盖）。值未变化时**不落盘但仍返回 `Ok`**。
///
/// ⚠️ 本函数只动 `config.json` 这一侧 —— 另一侧（`app_settings`，鉴权中间件读它）必须由
/// [`crate::webapi::apply_token`] 在**同一处**写完。想改 token 请调它，不要在这里再包一层，
/// 否则又会留下"两处不一致 → 改了 token 却被 401"的分叉。
pub fn set_web_api_token(token: &str) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("web api token 不能为空".into());
    }
    let _g = lock_config();
    let mut cfg = read_config_for_write()
        .ok_or_else(|| "config.json 不可读，已放弃写入 web_api_token".to_string())?;
    if cfg.web_api_token == token {
        return Ok(());
    }
    cfg.web_api_token = token.to_string();
    write_config_file(&cfg)
}

/// 读飞书集成配置（`config.json` 的 `integrations.lark`）。
///
/// 不跑迁移、不写盘 —— 读者只要值（bot 生命周期 / 命令层 / Web API）。
pub fn lark_integration() -> LarkIntegration {
    read_config_file().integrations.lark
}

/// 写回飞书集成配置（加锁的整文件读-改-写）。
///
/// 与 [`set_web_api_token`] 同款：先读**当前文件**，只替换 `integrations.lark`，
/// 其余字段（providers / embed_plans / 用户的未知键）**原样保留**。这正是
/// `docs/LARK_BOT.md` §5.6 故障②（"我手改了 config.json 却被应用整份覆盖"）的
/// 收口点 —— 所以飞书配置**绝不**走 [`save_config`] 那种"整份重建"的路径。
/// 值未变化时不落盘。
///
/// 返回**是否真的变了**（＝是否落了盘）。调用方据此决定要不要广播
/// `config-changed`：没变就别惊动界面，也别让"空写"看起来像一次改动。
/// ⚠️ 但**不要**用它来决定要不要重启 bot —— 重启与否是调用方的语义
/// （`commands/lark.rs` 保持原样：写完就重启）。
pub fn set_lark_integration(lark: &LarkIntegration) -> Result<bool, String> {
    let _g = lock_config();
    // 读不懂现有配置就**放弃写入**（否则会把整份配置冲成默认值，见 [`read_config_for_write`]）
    let mut cfg = read_config_for_write()
        .ok_or_else(|| "config.json 不可读，已放弃写入（避免覆盖整份配置）".to_string())?;
    // 规整化**在这里统一做**：所有写入口都汇到本函数，调用方不必各自记得清理。
    let next = lark.clone().sanitized();
    if cfg.integrations.lark == next {
        return Ok(false);
    }
    cfg.integrations.lark = next;
    write_config_file(&cfg)?;
    Ok(true)
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

/// 供"读-改-写"使用的配置读取：**读不懂就返回 `None`**，调用方据此**放弃本次写入**。
///
/// 与 [`read_config_file`] 的关键区别：那里解析失败会**回退到默认配置** —— 对"只要值"的
/// 读者没问题，但写入是"读-改-写"，一旦把"解析失败"当成"空配置"，一次**手改到一半 / 被
/// 截断**的 `config.json` 就会让整份配置（providers、`api_key`、`web_api_token`、用户的未知键）
/// **被默认值冲掉**。文件**不存在**不算损坏（全新安装，没有可覆盖的东西）→ 按默认配置处理。
fn read_config_for_write() -> Option<AppConfig> {
    let path = config_dir().join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<AppConfig>(&raw) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                log::warn!(
                    "[CONFIG] {} 解析失败，已放弃本次写入以免覆盖整份配置: {e}",
                    path.display()
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(AppConfig::default()),
        Err(e) => {
            log::warn!("[CONFIG] 读取 {} 失败，已放弃本次写入: {e}", path.display());
            None
        }
    }
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

/// Write config without taking the lock — callers must hold the config write
/// lock ([`lock_config`]，含跨进程那一层)。
fn write_config_file(config: &AppConfig) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
    let path = dir.join(CONFIG_FILE);
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("{e}"))?;
    write_config_atomic(&path, &content)?;
    // 记下"这份内容是我们写的"：热重载监听（`config_watch`）靠它**忽略自己触发的事件**。
    note_known(&content, &config.integrations.lark);
    Ok(())
}

/// `config.json` 内容的稳定 hash（Sha256 hex）。
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 本进程已知的 `config.json` 内容 hash；`None` ＝ 还没有基线。
pub fn known_config_hash() -> Option<String> {
    KNOWN_CONFIG_HASH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 记下"这份内容我们已知"（自己写的，或外部改动已被接受）。
fn note_known(content: &str, lark: &LarkIntegration) {
    *KNOWN_CONFIG_HASH.lock().unwrap_or_else(|e| e.into_inner()) = Some(content_hash(content));
    *KNOWN_LARK.lock().unwrap_or_else(|e| e.into_inner()) = Some(lark.clone());
}

/// 一次**外部** `config.json` 改动被接受后的结论。
pub struct ExternalConfigChange {
    /// 飞书策略是否与上一次已知值不同（**只有真变了**才需要重启 bot）。
    pub lark_changed: bool,
}

/// 判定并记录磁盘上的 `config.json` 内容（由 `config_watch` 调用）。
///
/// - 与已知 hash 相同 → `None`：**我们自己的写**，或已处理过。按**内容**判定而不是
///   mtime —— 按 mtime 会把应用自己的每次保存都当成"外部改动"，触发重载风暴。
/// - 解析失败（手改写到一半）→ `None` 且**不动基线**，等下一次去抖事件。
/// - 否则 → 刷新基线并返回结论。
pub fn observe_config_content(content: &str) -> Option<ExternalConfigChange> {
    let hash = content_hash(content);
    if known_config_hash().as_deref() == Some(hash.as_str()) {
        return None;
    }
    let cfg: AppConfig = serde_json::from_str(content).ok()?;
    let prev = KNOWN_LARK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let lark_changed = prev.as_ref() != Some(&cfg.integrations.lark);
    note_known(content, &cfg.integrations.lark);
    Some(ExternalConfigChange { lark_changed })
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
/// 2. **临时文件名带 pid**：主防线是跨进程锁（[`lock_config`]），但锁**可能降级**
///    （见其"有界等待"），所以带 pid 的临时名是**第二道保险** —— 锁一旦被绕过/降级，
///    两个进程也不会写同一个临时文件而互相撕碎。进程内 Mutex 单独挡不住第二个进程。
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
    "webui_base_url",
    "integrations",
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
    let _g = lock_config();
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
    let _g = lock_config();
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

    /// 老 `config.json`（没有 `integrations`）必须**原样可加载**，且飞书策略取
    /// **安全默认**：关 + 要求私聊 + 空名单（＝谁都不许）—— §7.1 ⑤ 的 fail-closed。
    #[test]
    fn integrations_default_to_fail_closed_and_legacy_json_still_loads() {
        let legacy = r#"{"data_dir":"/tmp/x"}"#;
        let cfg: AppConfig = serde_json::from_str(legacy).unwrap();
        let lark = cfg.integrations.lark;
        assert!(!lark.enabled, "默认必须是关（与 web_api_enabled 的默认开刻意相反）");
        assert!(lark.require_p2p, "默认必须要求私聊");
        assert!(lark.allowed_users.is_empty(), "默认空名单＝拒绝全部");
        assert!(lark.allowed_chats.is_empty());
        assert!(!lark.pairing);
        assert_eq!(lark.pairing_expires_at, 0);

        // 只给部分字段也要能补齐（容器级 `serde(default)`）。
        let partial = r#"{"integrations":{"lark":{"enabled":true}}}"#;
        let cfg: AppConfig = serde_json::from_str(partial).unwrap();
        assert!(cfg.integrations.lark.enabled);
        assert!(cfg.integrations.lark.require_p2p, "未给的字段必须退回安全默认");

        // 往返保住。
        let json = serde_json::to_string(&AppConfig::default()).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.integrations.lark, LarkIntegration::default());
    }

    /// 配对有效性 **fail-closed**：`0` / 已过去 / `pairing = false` 一律无效。
    /// 这挡住了"忘了设截止时刻＝永久敞开"这条最危险的解读（§7.1 ⑥ 第 4 条）。
    #[test]
    fn pairing_active_is_fail_closed() {
        let mut lark = LarkIntegration {
            pairing: true,
            pairing_expires_at: 0,
            ..LarkIntegration::default()
        };
        assert!(
            !lark.pairing_active(1_000),
            "截止时刻为 0 必须视为已过期（而不是\"不限时\"）"
        );
        lark.pairing_expires_at = 999;
        assert!(!lark.pairing_active(1_000), "已过去的时刻必须无效");
        lark.pairing_expires_at = 1_001;
        assert!(lark.pairing_active(1_000), "将来的时刻应有效");
        lark.pairing = false;
        assert!(!lark.pairing_active(0), "没开配对必须无效");
    }

    /// 规整化：去空白 / 去重 / 丢空项；**没在配对就不留截止时刻**。
    #[test]
    fn sanitized_normalizes_lists_and_pairing_expiry() {
        let lark = LarkIntegration {
            allowed_users: vec![
                " ou_a ".into(),
                "ou_a".into(),
                String::new(),
                "   ".into(),
                "ou_b".into(),
            ],
            allowed_chats: vec![" ".into()],
            export_allow_dirs: vec!["  /tmp/a  ".into(), "/tmp/a".into()],
            pairing: false,
            pairing_expires_at: 12_345,
            ..LarkIntegration::default()
        }
        .sanitized();
        assert_eq!(lark.allowed_users, vec!["ou_a".to_string(), "ou_b".to_string()]);
        assert!(lark.allowed_chats.is_empty());
        assert_eq!(lark.export_allow_dirs, vec!["/tmp/a".to_string()]);
        assert_eq!(lark.pairing_expires_at, 0, "没在配对就不该留下截止时刻");

        let lark = LarkIntegration {
            pairing: true,
            pairing_expires_at: 12_345,
            ..LarkIntegration::default()
        }
        .sanitized();
        assert_eq!(lark.pairing_expires_at, 12_345, "在配对时必须保留截止时刻");
    }

    /// 跨进程锁**真的**互斥：另一个句柄持锁时本句柄必须拿不到（而不是"假装拿到"）。
    /// 用同一路径的**两个句柄**模拟两个进程 —— `fs4` 的锁绑在句柄上，正是这个语义。
    #[test]
    fn cross_process_lock_is_exclusive_across_handles() {
        use fs4::FileExt as _;

        let dir = std::env::temp_dir().join(format!("ls_cfg_lock_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap()
        };
        let a = open();
        let b = open();

        assert!(a.try_lock_exclusive().is_ok(), "第一个句柄应拿到锁");
        assert!(
            !try_lock_with(Some(&b), Duration::from_millis(60), Duration::from_millis(10)),
            "对方持锁期间必须有界等待后返回 false（降级），而不是无限等"
        );
        a.unlock().unwrap();
        assert!(
            try_lock_with(Some(&b), Duration::from_millis(300), Duration::from_millis(10)),
            "对方释放后应能拿到"
        );
        let _ = b.unlock();

        // 拿不到句柄（例如目录不可建）＝ 降级，绝不能死锁。
        assert!(!try_lock_with(None, Duration::ZERO, Duration::from_millis(5)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 热重载的**自写抑制**：按**内容**判定 —— 同一份内容第二次必须返回 `None`，
    /// 否则应用自己的每次保存都会被当成"外部改动"（重载风暴）。
    #[test]
    fn observe_config_content_suppresses_self_writes_and_reports_lark_change() {
        let base = AppConfig::default();
        let base_json = serde_json::to_string_pretty(&base).unwrap();

        // 建立基线：无论此前是什么状态，这里都把它固定为 base_json。
        assert!(observe_config_content(&base_json).is_some());
        assert!(
            observe_config_content(&base_json).is_none(),
            "内容相同必须静默（自写抑制按内容，而不是按 mtime/事件本身）"
        );

        // 只改无关字段 → 接受，但报告"飞书没变"。
        let other = AppConfig {
            language: "en".into(),
            ..AppConfig::default()
        };
        let other_json = serde_json::to_string_pretty(&other).unwrap();
        let change = observe_config_content(&other_json).expect("内容变了就该被接受");
        assert!(!change.lark_changed, "只改 language 不该报告飞书变了");

        // 改飞书策略 → 报告"变了"（调用方据此重启 bot）。
        let lark_cfg = AppConfig {
            integrations: Integrations {
                lark: LarkIntegration {
                    enabled: true,
                    ..LarkIntegration::default()
                },
            },
            ..AppConfig::default()
        };
        let lark_json = serde_json::to_string_pretty(&lark_cfg).unwrap();
        let change = observe_config_content(&lark_json).expect("内容变了就该被接受");
        assert!(change.lark_changed, "飞书策略变了必须报告");

        // 解析失败（手改写到一半）→ 静默，且**不动基线**（等下一次去抖事件）。
        assert!(observe_config_content("{ 这不是 json").is_none());
        assert!(
            observe_config_content(&lark_json).is_none(),
            "一次解析失败不得破坏已知基线"
        );
    }
}