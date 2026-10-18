//! OpenAI-compatible AI gateway (embeddings + chat) and vector helpers.
//!
//! The gateway is configured via `AppConfig` (`ai_api_base` etc.). An empty
//! `ai_api_base` disables the feature; every public entry point degrades
//! gracefully to `None`/empty when unconfigured or unreachable.

pub mod local_embed;
pub mod local_rerank;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::config::{ModelType, ProviderConfig};
use serde::{Deserialize, Serialize};

/// Query-embedding cache. `embed()` serves single query texts from the
/// retrieval path (rewrite / BM25 query / vector scan); caching them skips
/// repeated local-BGE / gateway inference when the same or a near-identical
/// query returns in a session.
///
/// Eviction is **LRU**: 容量满时只淘汰最久未用的那一条（旧实现是"满了整体
/// clear"，把整块热集一起丢掉，紧接着的查询全都要重新嵌入）。键里含模型 id，
/// 换模型天然失效；换**网关/密钥**由 `update_config` 显式清空
/// （见 `clear_query_embed_cache`）。
/// Document/chunk embedding goes through `embed_batched`, never cached here.
static QUERY_EMBED_CACHE: OnceLock<Mutex<QueryEmbedCache>> = OnceLock::new();
const QUERY_EMBED_CACHE_CAP: usize = 256;

/// Bounded LRU: `map` 存向量，`order` 记录最近使用顺序（末尾最新）。
#[derive(Default)]
struct QueryEmbedCache {
    map: HashMap<String, Vec<f32>>,
    order: std::collections::VecDeque<String>,
}

impl QueryEmbedCache {
    fn get(&mut self, key: &str) -> Option<Vec<f32>> {
        let v = self.map.get(key)?.clone();
        self.touch(key);
        Some(v)
    }

    fn insert(&mut self, key: String, v: Vec<f32>) {
        if self.map.insert(key.clone(), v).is_some() {
            self.touch(&key);
        } else {
            self.order.push_back(key);
        }
        while self.order.len() > QUERY_EMBED_CACHE_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
    }

    /// 标记为"刚用过"。
    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

fn query_embed_cache() -> &'static Mutex<QueryEmbedCache> {
    QUERY_EMBED_CACHE.get_or_init(|| Mutex::new(QueryEmbedCache::default()))
}

fn cache_key(text: &str) -> String {
    // 缓存键包含当前 embedding 模型 id：切换模型后旧向量不可比，天然失效。
    let model = crate::config::load_config()
        .active_embedding_model_id
        .clone();
    format!("{model}\n{}", text.trim())
}

pub fn cached_embed(text: &str) -> Option<Vec<f32>> {
    let key = cache_key(text);
    if key.is_empty() {
        return embed(text);
    }
    {
        let mut cache = query_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = cache.get(&key) {
            return Some(v);
        }
    }
    let v = embed(text)?;
    let mut cache = query_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(key, v.clone());
    Some(v)
}

/// Drop the query-embedding cache (e.g. when the active embedding model or
/// gateway changes; cached vectors from another model are not comparable).
pub fn clear_query_embed_cache() {
    if let Some(c) = QUERY_EMBED_CACHE.get() {
        if let Ok(mut cache) = c.lock() {
            cache.clear();
        }
    }
}

/// [`cached_embed`] with a wall-clock cap. Interactive retrieval must never
/// block for minutes on a saturated local embed pool, so on timeout this
/// returns `None` and the caller falls back to lexical (BM25) retrieval.
/// The worker thread is left running: if it later obtains a replica it still
/// fills the cache, which only helps subsequent queries.
pub fn cached_embed_with_timeout(text: &str, timeout_secs: u64) -> Option<Vec<f32>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = text.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(cached_embed(&owned));
    });
    rx.recv_timeout(std::time::Duration::from_secs(timeout_secs))
        .ok()
        .flatten()
}

/// Number of interactive chat turns currently in flight. Background embedding
/// backfill yields to these so interactive retrieval keeps the local model
/// pool (and CPU) while the user waits.
static ACTIVE_CHAT_TURNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// RAII marker for an interactive chat turn. Decrements on every exit path,
/// including early returns, cancellation and panics.
pub struct ChatTurnGuard;

impl ChatTurnGuard {
    /// Mark a chat turn as started. Keep the returned guard alive for the whole
    /// turn (retrieval + LLM); it releases automatically when dropped.
    pub fn begin() -> Self {
        ACTIVE_CHAT_TURNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for ChatTurnGuard {
    fn drop(&mut self) {
        ACTIVE_CHAT_TURNS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// True while at least one interactive chat turn is running.
pub fn chat_in_flight() -> bool {
    ACTIVE_CHAT_TURNS.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// Resolved endpoint for a model role: the provider + model id in use.
#[derive(Debug, Clone)]
pub struct ActiveEndpoint {
    pub base_url: String,
    pub api_key: String,
    pub model_id: String,
}

impl ActiveEndpoint {
    fn new(p: &ProviderConfig, model_id: &str) -> Self {
        Self {
            base_url: p.base_url.clone(),
            api_key: p.api_key.clone(),
            model_id: model_id.to_string(),
        }
    }

    /// 返回 Self 当 base_url 是"远程 http://"（非 localhost/127.0.0.1/0.0.0.0 等本地地址）。
    fn check_https(&self) -> Option<&Self> {
        let url = self.base_url.trim_end_matches('/');
        let host = url
            .strip_prefix("http://")
            .and_then(|rest| rest.split(['/', ':']).next())
            .unwrap_or("");
        let is_local = ["localhost", "127.0.0.1", "0.0.0.0", "::1"]
            .iter().any(|l| host.eq_ignore_ascii_case(l));
        if url.starts_with("http://") && !is_local {
            Some(self)
        } else {
            None
        }
    }

    fn warn_insecure(&self) {
        use std::sync::OnceLock;
        static WARNED: OnceLock<()> = OnceLock::new();
        if WARNED.set(()).is_ok() {
            log::warn!(
                "[AI] 检测到远程 http:// AI 网关: {}（API Key 与文档内容将明文传输，建议改用 https:// 或本地 http://）",
                self.base_url
            );
        }
    }
}

/// Resolve the endpoint for a model role from the active selection
/// (`provider_id:model_id`). Returns None when unconfigured or dangling.
fn resolve_active_endpoint(cfg: &crate::config::AppConfig, kind: ModelType) -> Option<ActiveEndpoint> {
    let active_id = match kind {
        ModelType::Embedding => &cfg.active_embedding_model_id,
        ModelType::Llm => &cfg.active_llm_model_id,
        ModelType::Reranker => &cfg.active_reranker_model_id,
        ModelType::Unknown => return None,
    };
    if active_id.is_empty() {
        return None;
    }
    let (provider_id, model_id) = active_id.split_once(':')?;
    let provider = cfg.providers.iter().find(|p| p.id == provider_id)?;
    if !provider.models.iter().any(|m| m.id == model_id) {
        return None;
    }
    // SSRF 护栏（见 `validate_gateway_url`）：被拒的网关本轮直接禁用 ——
    // 手工改过 config.json 的情况由这里兜住，同时打一条可定位的 ERROR 日志。
    if let Err(e) = validate_gateway_url(&provider.base_url) {
        log::error!("[AI] 拒绝使用网关地址（{kind:?}）: {e}");
        return None;
    }
    // 安全：远程 http:// 网关会明文传输 API Key 与文档内容，仅提示不阻断
    if let Some(ep) = ActiveEndpoint::new(provider, model_id).check_https() {
        ep.warn_insecure()
    }
    Some(ActiveEndpoint::new(provider, model_id))
}

const DEFAULT_MAX_TOKENS: u32 = 1_000_000;

/// 网关 URL 的 **SSRF 护栏**：拒绝「链路上绝不该出现」的地址。
///
/// **只堵必要的一类**：云元数据服务（`169.254.169.254` / `100.100.100.200` 等）
/// 与 `metadata.*` 主机名 —— 它们只可能出现在 SSRF 里，任何真实网关都不会部署在
/// 那里。回环（`127.0.0.1`：本机 Ollama）与私网（局域网 llama.cpp / vLLM）都是
/// **合法**用法，刻意放行（要不要连私网一起封，第三十八轮讨论后结论是不封）。
///
/// 调用点：保存网关配置时（立刻给用户明确报错）+ 每次解析活动端点时（兜住手工
/// 改过 `config.json` 的情况）。
pub fn validate_gateway_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("网关地址为空".to_string());
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(format!("只支持 http:// 或 https:// 网关地址: {url}"));
    }
    let authority = lower
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .map(|a| a.rsplit_once('@').map(|(_, h)| h).unwrap_or(a))
        .unwrap_or("");
    // IPv6 字面量带方括号（`[::1]:8443`），端口在 `]` 之后；IPv4/域名则剥掉
    // 末尾的 `:port`（仅当冒号后全是数字，避免把 IPv6 的冒号当端口分隔）。
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
            _ => authority,
        }
    };
    if host.is_empty() {
        return Err(format!("网关地址缺少主机名: {url}"));
    }
    if host == "metadata.google.internal" || host == "metadata.goog" {
        return Err(format!("拒绝云元数据地址（SSRF 风险）: {url}"));
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let is_link_local = match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                // 169.254.0.0/16：链路本地（AWS/GCP/Azure/阿里云元数据都在这一段）
                o[0] == 169 && o[1] == 254
            }
            std::net::IpAddr::V6(v6) => {
                let seg = v6.segments();
                let ll = (seg[0] & 0xffc0) == 0xfe80; // fe80::/10
                let mapped_ll = v6
                    .to_ipv4_mapped()
                    .map(|v4| {
                        let o = v4.octets();
                        o[0] == 169 && o[1] == 254
                    })
                    .unwrap_or(false);
                ll || mapped_ll
            }
        };
        if is_link_local {
            return Err(format!("拒绝链路本地地址（常为云元数据服务，SSRF 风险）: {url}"));
        }
    }
    Ok(())
}

/// Trustworthy per-model output ceiling, from the gateway's `/v1/models`
/// `capabilities.maxOutput` (cached on the model config).
///
/// `None` means the gateway does not report one. Callers MUST NOT substitute a
/// guessed value in that case: routing/aggregator gateways (e.g. an alias like
/// `coding`) may route each request to a different upstream model with a
/// different ceiling, so any single cached/fallback number is either rejected
/// with HTTP 400 or silently truncates. When `None`, omit `max_tokens` and let
/// the upstream apply its own default.
fn detected_max_tokens() -> Option<u32> {
    let cfg = crate::config::load_config();
    let (pid, mid) = cfg.active_llm_model_id.split_once(':').unwrap_or(("", &cfg.active_llm_model_id));
    let detected = cfg.providers.iter()
        .find(|p| p.id == pid)
        .and_then(|p| p.models.iter().find(|m| m.id == mid))
        .and_then(|m| m.max_output_tokens);
    log::debug!(
        "[AI] max_tokens={:?} (source={})",
        detected,
        if detected.is_some() { "detected" } else { "none->omit" }
    );
    detected
}

/// Display-only helper for the settings "测试连接" panel: the detected value,
/// or [`DEFAULT_MAX_TOKENS`] as a human-readable fallback. Chat requests use
/// [`detected_max_tokens`] instead and omit the field when it is `None`.
fn active_max_tokens() -> u32 {
    detected_max_tokens().unwrap_or(DEFAULT_MAX_TOKENS)
}

/// Classify a model id by name heuristics. Pure function, user-overridable.
pub fn classify_model_by_name(id: &str) -> ModelType {
    let lower = id.to_lowercase();
    const EMBED_HINTS: &[&str] = &[
        "embed", "text-embedding", "bge", "minilm", "e5-", "gte-", "nomic-embed",
        "jina-embeddings", "mxbai-embed", "all-minilm",
    ];
    const RERANK_HINTS: &[&str] = &["reranker", "rerank", "cross-encoder"];
    const LLM_HINTS: &[&str] = &[
        "instruct", "chat", "llm", "gpt", "qwen", "deepseek", "gemma", "llama", "mistral",
        "mixtral", "yi-", "glm", "phi", "command-r", "claude", "gemini",
    ];
    // Rerank 必须先判：`bge-reranker-v2-m3` 含 "bge"，若先跑 EMBED_HINTS 会被
    // 误判为嵌入模型，用户在设置页就会把它选进"嵌入模型"下拉框并调用失败。
    for h in RERANK_HINTS {
        if lower.contains(h) {
            return ModelType::Reranker;
        }
    }
    for h in EMBED_HINTS {
        if lower.contains(h) {
            return ModelType::Embedding;
        }
    }
    for h in LLM_HINTS {
        if lower.contains(h) {
            return ModelType::Llm;
        }
    }
    // No特征命中：默认视为对话模型（绝大多数 provider/models 是 LLM；
    // 真正无特征的 embedding 极罕见, 且 UI 可手动改类型）。
    ModelType::Llm
}

pub fn ai_enabled() -> bool {
    embedding_enabled() || llm_enabled()
}

/// True when an embedding model is active (semantic search usable).
pub fn embedding_enabled() -> bool {
    use std::sync::OnceLock;
    static LOGGED: OnceLock<()> = OnceLock::new();
    let cfg = crate::config::load_config();
    let model_id = &cfg.active_embedding_model_id;
    let is_local = crate::config::is_local_embedding_model(model_id);
    if is_local {
        let model_name = local_embed::local_model_dir_name(model_id).unwrap_or("bge-large-zh-v1.5");
        let ready = local_embed::bge_model_ready(&cfg.data_dir, model_name);
        if LOGGED.set(()).is_ok() {
            log::info!("[AI] embed: model={} local={} ready={}", model_id, is_local, ready);
        }
        return ready;
    }
    let ok = resolve_active_endpoint(&cfg, ModelType::Embedding).is_some();
    if LOGGED.set(()).is_ok() {
        log::info!("[AI] embed: model={} local=false ok={}", model_id, ok);
    }
    ok
}

/// True when an LLM model is active (summary / RAG usable).
pub fn llm_enabled() -> bool {
    resolve_active_endpoint(&crate::config::load_config(), ModelType::Llm).is_some()
}

/// 区分"未配置"与"已配置但当前模型不可用"（active 指向被删 provider /
/// 不存在的模型）。返回 None 表示可用。
pub fn llm_unavailable_reason() -> Option<&'static str> {
    llm_unavailable_reason_for(&crate::config::load_config())
}

fn llm_unavailable_reason_for(cfg: &crate::config::AppConfig) -> Option<&'static str> {
    let active_id = cfg.active_llm_model_id.as_str();
    if active_id.is_empty() {
        return Some("AI 服务未配置，请在设置页配置 API Base URL");
    }
    let Some((provider_id, model_id)) = active_id.split_once(':') else {
        return Some("AI 服务未配置，请在设置页配置 API Base URL");
    };
    let Some(_provider) = cfg.providers.iter().find(|p| p.id == provider_id) else {
        return Some("当前使用的 LLM 网关已被删除，请在设置页重新选择");
    };
    let model_exists = cfg.providers.iter()
        .filter(|p| p.id == provider_id)
        .flat_map(|p| p.models.iter())
        .any(|m| m.id == model_id);
    if !model_exists {
        return Some("当前使用的 LLM 模型不可用，请在设置页重新选择");
    }
    None
}

/// Embedding models usually cap input at 512 tokens; over-length text rejects
/// the whole gateway batch, so inputs are truncated to a safe char budget.
const EMBED_MAX_CHARS: usize = 2000;

pub fn truncate_for_embed(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= EMBED_MAX_CHARS {
        s.to_string()
    } else {
        chars[..EMBED_MAX_CHARS].iter().collect()
    }
}

/// Embed texts; failed texts map to `None`.
///
/// Local models run batches in parallel across a replica pool (see
/// `local_embed::embed_batched_local`). Remote gateways run an **adaptive,
/// concurrent** path: batches are sent with a learned `(batch, concurrency)`
/// plan that the tuner grows while throughput improves and backs off on errors
/// (see [`next_embed_plan`]). Set `embed_adaptive=0` (or the env
/// `LINK_SEARCHER_EMBED_ADAPTIVE=0`) to force plain sequential batches using
/// `batch_size`.
pub fn embed_batched(texts: &[String], batch_size: usize) -> Vec<Option<Vec<f32>>> {
    let batch_size = batch_size.max(1);
    let cfg = crate::config::load_config();
    if crate::config::is_local_embedding_model(&cfg.active_embedding_model_id) {
        let model_name = local_embed::local_model_dir_name(&cfg.active_embedding_model_id)
            .unwrap_or("bge-large-zh-v1.5");
        if local_embed::bge_model_ready(&cfg.data_dir, model_name) {
            let _ = local_embed::init_local_embedder(&cfg.data_dir, model_name);
            return local_embed::embed_batched_local(texts, batch_size);
        }
        return vec![None; texts.len()];
    }
    if texts.is_empty() {
        return Vec::new();
    }
    let Some(ep) = resolve_active_endpoint(&cfg, ModelType::Embedding) else {
        return vec![None; texts.len()];
    };

    let adaptive = embed_adaptive_enabled();
    let key = embed_plan_key(&ep.base_url, &ep.model_id);
    let plan = if adaptive {
        // 优先用**内存里**那份（`last_plan`）：① 省掉一次整份 config.json 解析；
        // ② 让下面"只在结构性变化时落盘"不破坏 AIMD 爬坡连续性 —— 平台期的
        // `tok_per_s` 衰减仍逐轮在内存里累计，只是不必每次都写盘。
        last_plan_for(&key)
            .unwrap_or_else(|| crate::config::load_embed_plan(&key).unwrap_or_default())
    } else {
        crate::config::EmbedPlan { batch: batch_size, conc: 1, tok_per_s: 0.0, updated: 0 }
    };

    let (out, sample) = embed_batched_remote(&ep, texts, plan);
    log::debug!(
        "[AI] 远程嵌入: {} 项, {:.0} tok/s, 失败 {}",
        sample.items,
        sample.tok_per_s,
        sample.errors
    );

    let effective = if adaptive { next_embed_plan(plan, sample) } else { plan };
    if adaptive && plan_should_persist(plan, effective) {
        crate::config::save_embed_plan(&key, effective);
    }
    *last_plan().lock().unwrap_or_else(|e| e.into_inner()) = Some((key, effective));
    out
}

/// Observed result of one remote embedding pass (all chunks of one call).
#[derive(Debug, Clone, Copy, Default)]
pub struct EmbedSample {
    /// Throughput over the pass (gateway tokens — or chars when the gateway
    /// omits `usage` — per wall second).
    pub tok_per_s: f32,
    /// Items submitted.
    pub items: usize,
    /// Chunks that failed after retries.
    pub errors: usize,
}

/// Bounds for the adaptive tuner.
pub const EMBED_BATCH_MIN: usize = 16;
pub const EMBED_BATCH_MAX: usize = 512;
pub const EMBED_CONC_MIN: usize = 1;
pub const EMBED_CONC_MAX: usize = 8;
/// Growth/decay factors for the AIMD ramp.
const GROW_RATIO: f32 = 1.5;
const SHRINK_RATIO: f32 = 0.6;
/// Throughput must beat the recorded best by this factor to count as progress.
const IMPROVE_RATIO: f32 = 1.03;

fn round_up_8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// 取**本进程内存里**那份计划（key 匹配才用 —— `last_plan` 只存最近一条）。
fn last_plan_for(key: &str) -> Option<crate::config::EmbedPlan> {
    last_plan()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(k, _)| k == key)
        .map(|(_, p)| *p)
}

/// 这一次算出的计划是否**值得落盘**：只看结构性变化（`batch` / `conc`）。
///
/// 为什么不能"变了就写"：平台期分支每轮都会把 `tok_per_s` 乘 0.97（`updated` 也变），
/// 于是 `next != prev` **恒真** ⇒ 每一次嵌入调用都要"加跨进程锁 + 读 113 KB + fsync 写回"。
/// 而纯吞吐量/时间戳的变化对下次启动几乎没有意义（顶多起点略保守），结构参数才是要害。
/// 落盘次数因此从"每次调用"降到"每个爬坡台阶"（一次 backfill 约十几次）。
fn plan_should_persist(prev: crate::config::EmbedPlan, next: crate::config::EmbedPlan) -> bool {
    next.batch != prev.batch || next.conc != prev.conc
}

/// Pure tuner step: given the previous plan and one sample, compute the next
/// plan. Batch grows first until it caps, then concurrency grows; any error
/// backs both off. Deterministic and side-effect free (unit-tested).
pub fn next_embed_plan(
    prev: crate::config::EmbedPlan,
    sample: EmbedSample,
) -> crate::config::EmbedPlan {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut next = prev;

    if sample.errors > 0 {
        next.conc = (prev.conc / 2).max(EMBED_CONC_MIN);
        let shrunk = round_up_8((prev.batch as f32 * SHRINK_RATIO) as usize).max(EMBED_BATCH_MIN);
        next.batch = shrunk.min(prev.batch);
        next.updated = now;
        return next;
    }
    if sample.tok_per_s <= 0.0 {
        return next; // no measurement (e.g. empty) — leave the plan untouched
    }

    if sample.tok_per_s > prev.tok_per_s * IMPROVE_RATIO {
        if prev.batch < EMBED_BATCH_MAX {
            next.batch =
                round_up_8(((prev.batch as f32 * GROW_RATIO) as usize).min(EMBED_BATCH_MAX));
        } else if prev.conc < EMBED_CONC_MAX {
            next.conc = prev.conc + 1;
        }
        next.tok_per_s = sample.tok_per_s;
    } else if sample.tok_per_s < prev.tok_per_s * 0.9 {
        // Real regression: step back one notch — batch first, then concurrency.
        if prev.batch > EMBED_BATCH_MIN {
            let shrunk = round_up_8((prev.batch as f32 * SHRINK_RATIO) as usize).max(EMBED_BATCH_MIN);
            next.batch = if shrunk < prev.batch { shrunk } else { prev.batch.saturating_sub(8).max(EMBED_BATCH_MIN) };
        } else if prev.conc > EMBED_CONC_MIN {
            next.conc = prev.conc - 1;
        }
        next.tok_per_s = (prev.tok_per_s * 0.97).max(sample.tok_per_s);
    } else {
        // Plateau (within ±10%): hold this point and decay the recorded best
        // slightly, so a later probe can still register as an improvement
        // instead of freezing the ramp forever.
        next.tok_per_s = prev.tok_per_s * 0.97;
    }
    next.updated = now;
    next
}

/// Stable key for a persisted plan: endpoint + model.
fn embed_plan_key(base_url: &str, model_id: &str) -> String {
    format!("{}|{}", base_url.trim_end_matches('/'), model_id)
}

/// Per-request read timeout, scaled with the request size.
fn embed_read_timeout_secs(batch: usize) -> u64 {
    (batch as u64 * 3).clamp(60, 900)
}

/// Whether the adaptive remote path is enabled. Precedence: env
/// `LINK_SEARCHER_EMBED_ADAPTIVE` > setting `embed_adaptive` > default on.
fn embed_adaptive_enabled() -> bool {
    if let Ok(v) = std::env::var("LINK_SEARCHER_EMBED_ADAPTIVE") {
        return !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off");
    }
    let data_dir = crate::config::load_config().data_dir;
    let db_path = data_dir.join("data.db");
    crate::db::get_pool(&db_path.to_string_lossy())
        .ok()
        .and_then(|pool| {
            pool.get().ok().and_then(|conn| {
                conn.query_row(
                    "SELECT value FROM app_settings WHERE key = 'embed_adaptive'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            })
        })
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true)
}

/// Last plan used by the remote path, for the index-status progress line.
static LAST_EMBED_PLAN: OnceLock<Mutex<Option<(String, crate::config::EmbedPlan)>>> =
    OnceLock::new();

fn last_plan() -> &'static Mutex<Option<(String, crate::config::EmbedPlan)>> {
    LAST_EMBED_PLAN.get_or_init(|| Mutex::new(None))
}

/// One-line description of the last remote embedding plan (None when the
/// active model is local or no remote pass has run yet).
pub fn embed_plan_summary() -> Option<String> {
    let cfg = crate::config::load_config();
    if crate::config::is_local_embedding_model(&cfg.active_embedding_model_id) {
        return None;
    }
    let guard = last_plan().lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|(key, p)| {
        let model = key.split('|').nth(1).unwrap_or(key.as_str());
        format!("{model} batch={} 并发={} 峰值={:.0}tok/s", p.batch, p.conc, p.tok_per_s)
    })
}

/// One HTTP embeddings request with retry/backoff. Returns vectors in input
/// order plus the gateway-reported token count (0 when `usage` is absent).
fn embed_one_remote(
    ep: &ActiveEndpoint,
    texts: &[String],
    timeout_secs: u64,
) -> Result<(Vec<Option<Vec<f32>>>, u64), String> {
    #[derive(Serialize)]
    struct Req<'a> {
        model: &'a str,
        input: Vec<String>,
    }
    #[derive(Deserialize)]
    struct EmbeddingEntry {
        index: usize,
        embedding: Vec<f32>,
    }
    #[derive(Deserialize)]
    struct Usage {
        #[serde(default)]
        total_tokens: u64,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<EmbeddingEntry>,
        #[serde(default)]
        usage: Option<Usage>,
    }

    let url = format!("{}/embeddings", ep.base_url.trim_end_matches('/'));
    let body = Req {
        model: &ep.model_id,
        input: texts.iter().map(|t| truncate_for_embed(t)).collect(),
    };
    let req_body = serde_json::to_string(&body).map_err(|e| e.to_string())?;

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let send = build_agent_with_read_timeout(timeout_secs)
            .post(&url)
            .set("Content-Type", "application/json")
            .set_auth(&ep.api_key)
            .send_string(&req_body);
        let parsed: Result<Resp, String> = send
            .map_err(|e| e.to_string())
            .and_then(|r| r.into_string().map_err(|e| e.to_string()))
            .and_then(|body| serde_json::from_str::<Resp>(&body).map_err(|e| e.to_string()));
        match parsed {
            Ok(resp) => {
                let mut by_index: HashMap<usize, Vec<f32>> =
                    resp.data.into_iter().map(|e| (e.index, e.embedding)).collect();
                let vecs = texts.iter().enumerate().map(|(i, _)| by_index.remove(&i)).collect();
                let tokens = resp.usage.map(|u| u.total_tokens).unwrap_or(0);
                return Ok((vecs, tokens));
            }
            Err(e) if attempt < 3 => {
                let backoff = std::time::Duration::from_millis(400 * attempt as u64 * attempt as u64);
                log::warn!("[AI] embeddings 请求失败(第 {attempt} 次): {e}; {backoff:?} 后重试");
                std::thread::sleep(backoff);
            }
            Err(e) => return Err(format!("重试 {attempt} 次仍失败: {e}")),
        }
    }
}

/// Run `texts` through the gateway with `plan.conc` concurrent requests, each
/// carrying `plan.batch` inputs. Results keep input order; failed chunks become
/// `None`. Returns the vectors plus a throughput sample for the tuner.
fn embed_batched_remote(
    ep: &ActiveEndpoint,
    texts: &[String],
    plan: crate::config::EmbedPlan,
) -> (Vec<Option<Vec<f32>>>, EmbedSample) {
    let batch = plan.batch.clamp(EMBED_BATCH_MIN, EMBED_BATCH_MAX);
    let conc = plan.conc.clamp(EMBED_CONC_MIN, EMBED_CONC_MAX);
    let chunks: Vec<&[String]> = texts.chunks(batch).collect();
    let n_chunks = chunks.len();
    let timeout = embed_read_timeout_secs(batch);
    let started = std::time::Instant::now();

    let next = std::sync::atomic::AtomicUsize::new(0);
    let errors = std::sync::atomic::AtomicUsize::new(0);
    let results: Mutex<Vec<Option<(Vec<Option<Vec<f32>>>, u64)>>> = Mutex::new(vec![None; n_chunks]);

    let workers = conc.min(n_chunks).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if i >= n_chunks {
                        break;
                    }
                    match embed_one_remote(ep, chunks[i], timeout) {
                        Ok((vecs, tokens)) => {
                            results
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())[i] = Some((vecs, tokens));
                        }
                        Err(e) => {
                            log::warn!("[AI] embeddings 分块 {i} 失败: {e}");
                            errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
            });
        }
    });

    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
    let mut total_tokens: u64 = 0;
    for (i, chunk) in chunks.iter().enumerate() {
        let slot = results.lock().unwrap_or_else(|e| e.into_inner())[i].take();
        match slot {
            Some((vecs, tokens)) => {
                total_tokens += tokens;
                out.extend(vecs);
            }
            None => out.extend(std::iter::repeat(None::<Vec<f32>>).take(chunk.len())),
        }
    }
    out.truncate(texts.len());

    let chars: usize = texts.iter().map(|t| t.chars().count()).sum();
    let units = if total_tokens > 0 { total_tokens as f64 } else { chars as f64 };
    let sample = EmbedSample {
        tok_per_s: (units / elapsed) as f32,
        items: texts.len(),
        errors: errors.load(std::sync::atomic::Ordering::SeqCst),
    };
    (out, sample)
}

pub fn embed_batch(texts: &[String]) -> Vec<Option<Vec<f32>>> {
    let cfg = crate::config::load_config();
    if crate::config::is_local_embedding_model(&cfg.active_embedding_model_id) {
        let model_name = local_embed::local_model_dir_name(&cfg.active_embedding_model_id).unwrap_or("bge-large-zh-v1.5");
        if local_embed::bge_model_ready(&cfg.data_dir, model_name) {
            let _ = local_embed::init_local_embedder(&cfg.data_dir, model_name);
            return local_embed::embed_batch_local(texts);
        }
        return vec![None; texts.len()];
    }
    let Some(ep) = resolve_active_endpoint(&cfg, ModelType::Embedding) else {
        return vec![None; texts.len()];
    };
    if texts.is_empty() {
        return vec![];
    }
    let url = format!("{}/embeddings", ep.base_url.trim_end_matches('/'));

    #[derive(Serialize)]
    struct Req {
        model: String,
        input: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<EmbeddingEntry>,
    }
    #[derive(Deserialize)]
    struct EmbeddingEntry {
        index: usize,
        embedding: Vec<f32>,
    }

    let body = Req {
        model: ep.model_id.clone(),
        input: texts.iter().map(|t| truncate_for_embed(t)).collect(),
    };

    let req_body = serde_json::to_string(&body).unwrap_or_default();
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth(&ep.api_key)
        .send_string(&req_body);

    let parsed: Result<Resp, String> = send_result
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|body| serde_json::from_str::<Resp>(&body).map_err(|e| e.to_string()));

    let resp = match parsed {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[AI] embeddings request failed: {e}");
            return vec![None; texts.len()];
        }
    };
    let mut by_index: HashMap<usize, Vec<f32>> = resp
        .data
        .into_iter()
        .map(|e| (e.index, e.embedding))
        .collect();
    texts
        .iter()
        .enumerate()
        .map(|(i, _)| by_index.remove(&i))
        .collect()
}

/// Rerank `passages` against `query`. Returns one score per passage **in input
/// order**. A `local:<model>` reranker runs the bundled cross-encoder; otherwise
/// the gateway's `/rerank` endpoint is used. `None` on any failure (no reranker
/// configured, model/gateway unavailable, malformed response) — caller keeps the
/// original ordering.
pub fn rerank(query: &str, passages: &[String]) -> Option<Vec<f32>> {
    if passages.is_empty() {
        return None;
    }
    let cfg = crate::config::load_config();

    // Bundled local cross-encoder.
    if let Some(model_name) = local_embed::local_model_dir_name(&cfg.active_reranker_model_id) {
        if local_rerank::rerank_model_ready(&cfg.data_dir, model_name) {
            let _ = local_rerank::init_local_reranker(&cfg.data_dir, model_name);
            return local_rerank::rerank_local(query, passages);
        }
        return None;
    }

    let ep = resolve_active_endpoint(&cfg, ModelType::Reranker)?;
    let url = format!("{}/rerank", ep.base_url.trim_end_matches('/'));

    #[derive(Serialize)]
    struct Req<'a> {
        model: &'a str,
        query: &'a str,
        documents: &'a [String],
    }
    #[derive(Deserialize)]
    struct Resp {
        results: Vec<RerankEntry>,
    }
    #[derive(Deserialize)]
    struct RerankEntry {
        index: usize,
        relevance_score: f32,
    }

    let body = Req { model: &ep.model_id, query, documents: passages };
    let req_body = serde_json::to_string(&body).unwrap_or_default();
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth(&ep.api_key)
        .send_string(&req_body);

    let parsed: Result<Resp, String> = send_result
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|body| serde_json::from_str::<Resp>(&body).map_err(|e| e.to_string()));

    let resp = match parsed {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[AI] rerank request failed: {e}");
            return None;
        }
    };
    let mut scores = vec![f32::MIN; passages.len()];
    for entry in resp.results {
        if entry.index < passages.len() {
            scores[entry.index] = entry.relevance_score;
        }
    }
    Some(scores)
}

pub fn embed(text: &str) -> Option<Vec<f32>> {
    let cfg = crate::config::load_config();
    if crate::config::is_local_embedding_model(&cfg.active_embedding_model_id) {
        let model_name = local_embed::local_model_dir_name(&cfg.active_embedding_model_id).unwrap_or("bge-large-zh-v1.5");
        if local_embed::bge_model_ready(&cfg.data_dir, model_name) {
            let _ = local_embed::init_local_embedder(&cfg.data_dir, model_name);
            return local_embed::embed_query_local(text);
        }
        return None;
    }
    embed_batch(&[text.to_string()]).into_iter().next().flatten()
}

/// Brute-force cosine scan over ALL stored embeddings.
/// Returns every doc_id with similarity >= threshold, sorted descending.
/// `release_top_k` is passed through to [`vector_scan_with_query_emb`];
/// this wrapper keeps the pure-threshold behaviour (0 = no release).
pub fn vector_full_scan(
    conn: &rusqlite::Connection,
    query: &str,
    threshold: f32,
) -> Result<Vec<(String, f32)>, String> {
    let query_emb = cached_embed(query).ok_or("query embedding failed (embedding not enabled?)")?;
    vector_scan_with_query_emb(conn, &query_emb, threshold, 0)
}

/// Core of [`vector_full_scan`] taking a pre-computed query embedding,
/// so callers scanning both file and chunk vectors only embed once.
///
/// 阈值只用来判定「强相关」，不再是整条通道的闸门：短实体查询（2~4 字，如
/// "深蓝"）与全库任意文档的余弦普遍落在 0.45~0.52，硬阈值 0.55 会返回 0
/// 命中，语义通道等于被关掉。`release_top_k > 0` 时，**若过阈值的强命中为 0**
/// 则额外保底放行相似度最高的这么多份弱候选（调用方按相似度给低权重，交给
/// 重排决定去留）；已有强命中说明通道工作正常，不再灌弱候选制造噪声。
/// 返回按相似度降序：强相关在前，保底放行的弱候选在后。
pub fn vector_scan_with_query_emb(
    conn: &rusqlite::Connection,
    query_emb: &[f32],
    threshold: f32,
    release_top_k: usize,
) -> Result<Vec<(String, f32)>, String> {
    let all = crate::db::tracker::get_all_embeddings(conn).map_err(|e| e.to_string())?;
    let all_count = all.len();
    let mut scored: Vec<(String, f32)> = all
        .into_iter()
        .map(|(fid, vec)| (fid, cosine(query_emb, &vec)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let max_sim = scored.first().map(|(_, s)| *s).unwrap_or(0.0);
    let (strong, weak): (Vec<_>, Vec<_>) =
        scored.into_iter().partition(|(_, sim)| *sim >= threshold);
    let strong_count = strong.len();
    let mut results = strong;
    if release_top_k > 0 && results.is_empty() {
        results.extend(weak.into_iter().take(release_top_k));
    }
    let released = results.len() - strong_count;
    log::info!(
        "[AI]   vector: {all_count} emb, {strong_count} above {threshold:.2} (+{released} released, max_sim={max_sim:.4})"
    );
    Ok(results)
}

/// Brute-force cosine scan over ALL stored chunk embeddings.
/// Returns (md5, chunk_index, similarity) sorted descending, capped at
/// `top_k` so long documents with many chunks can't flood the caller.
/// Chunk vectors encode ~1500-char windows, so detail-level content that a
/// whole-file vector dilutes is directly retrievable here.
/// `release_top_k` behaves as in [`vector_scan_with_query_emb`].
pub fn chunk_vector_scan(
    conn: &rusqlite::Connection,
    query: &str,
    threshold: f32,
    top_k: usize,
    release_top_k: usize,
) -> Result<Vec<(String, usize, f32)>, String> {
    let query_emb = cached_embed(query).ok_or("query embedding failed (embedding not enabled?)")?;
    chunk_vector_scan_with_query_emb(conn, &query_emb, threshold, top_k, release_top_k)
}

/// Core of [`chunk_vector_scan`] taking a pre-computed query embedding,
/// so callers scanning both file and chunk vectors only embed once.
pub fn chunk_vector_scan_with_query_emb(
    conn: &rusqlite::Connection,
    query_emb: &[f32],
    threshold: f32,
    top_k: usize,
    release_top_k: usize,
) -> Result<Vec<(String, usize, f32)>, String> {
    let all = crate::db::tracker::get_all_chunk_embeddings(conn).map_err(|e| e.to_string())?;
    let all_count = all.len();
    let results = chunk_vector_scan_impl(query_emb, &all, threshold, top_k, release_top_k);
    let above = results.iter().filter(|(_, _, s)| *s >= threshold).count();
    log::info!("[AI]   chunk_vector: {} emb, {} above {:.2} (+{} released, top {top_k})",
        all_count, above, threshold, results.len() - above);
    Ok(results)
}

/// Pure scoring core of [`chunk_vector_scan`], split out for unit testing
/// (no model / DB dependency). Same threshold + release semantics as the
/// document-level scan: 仅当**过阈值的强命中为空**时，才在 `strong` 之后
/// 保底放行 `release_top_k` 份最相似的弱候选。
fn chunk_vector_scan_impl(
    query_emb: &[f32],
    rows: &[(String, usize, Vec<f32>)],
    threshold: f32,
    top_k: usize,
    release_top_k: usize,
) -> Vec<(String, usize, f32)> {
    let mut scored: Vec<(String, usize, f32)> = rows
        .iter()
        .map(|(md5, idx, vec)| (md5.clone(), *idx, cosine(query_emb, vec)))
        .collect();
    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let (strong, weak): (Vec<_>, Vec<_>) =
        scored.into_iter().partition(|(_, _, sim)| *sim >= threshold);
    let mut results = strong;
    if release_top_k > 0 && results.is_empty() {
        results.extend(weak.into_iter().take(release_top_k));
    }
    results.truncate(top_k);
    results
}

/// Two-level-funnel chunk scan: cosine over chunk embeddings belonging ONLY
/// to the given md5 set (documents already short-listed by the document-level
/// coarse filter), instead of brute-forcing the whole chunk corpus.
/// Returns (md5, chunk_index, similarity) sorted desc, capped at `top_k`.
pub fn chunk_vector_scan_for_md5s(
    conn: &rusqlite::Connection,
    query_emb: &[f32],
    md5s: &[String],
    threshold: f32,
    top_k: usize,
    release_top_k: usize,
) -> Result<Vec<(String, usize, f32)>, String> {
    let rows = crate::db::tracker::get_chunk_embeddings_by_md5s(conn, md5s).map_err(|e| e.to_string())?;
    let scanned = rows.len();
    let results = chunk_vector_scan_impl(query_emb, &rows, threshold, top_k, release_top_k);
    let above = results.iter().filter(|(_, _, s)| *s >= threshold).count();
    log::info!(
        "[AI]   chunk_vector(funnel): {} md5s -> {} chunk emb scanned, {} above {:.2} (+{} released, top {top_k})",
        md5s.len(), scanned, above, threshold, results.len() - above
    );
    Ok(results)
}

/// Send a chat-completion prompt to the configured LLM and return the reply
/// text. `system` is the instruction, `user` the task/content. Returns `None`
/// when unconfigured or the request fails (downgrade, never block).
pub fn chat(system: &str, user: &str) -> Option<String> {
    chat_with_timeout(system, user, 60 * 60)
}

/// Smallest `max_tokens` the adaptive-retry loop will step down to.
const MIN_MAX_TOKENS: u32 = 1024;

/// Pull the gateway's stated `max_tokens` ceiling out of a 400 body
/// (e.g. "max_tokens 不能超过 65536"). Returns the largest plausible value
/// below `current`, so a body echoing the rejected request size can't win.
fn max_tokens_limit_from_error(body: &str, current: u32) -> Option<u32> {
    let lower = body.to_ascii_lowercase();
    let idx = lower.find("max_tokens")?;
    body[idx..]
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|tok| tok.parse::<u32>().ok())
        .filter(|&v| v >= MIN_MAX_TOKENS && v < current)
        .max()
}

/// Why a gateway rejected a chat request (HTTP 400). Only [`Self::OutputLimit`]
/// is fixable by changing `max_tokens`; retrying the others with a smaller
/// budget just burns attempts and hides the real error from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LlmErrorKind {
    /// `max_tokens` / output budget exceeds the upstream model's ceiling.
    OutputLimit,
    /// The input (system prompt + retrieved context) exceeds the context window.
    ContextLimit,
    /// The model/provider behind an alias is not currently routable.
    UpstreamUnavailable,
    Other,
}

/// Classify a gateway error body. Deliberately heuristic: gateways phrase the
/// same condition very differently, and aggregators pass each upstream's own
/// wording straight through.
fn classify_llm_error(body: &str) -> LlmErrorKind {
    let lower = body.to_ascii_lowercase();
    // Context first: such bodies frequently mention `max_tokens` as well, and
    // shrinking `max_tokens` would never fix a too-long input.
    const CTX: &[&str] = &[
        "context length", "context_length", "context window", "maximum context",
        "max context", "too many tokens", "prompt is too long", "prompt too long",
        "reduce the length", "max_seq_len", "sequence length", "input is too long",
        "exceeds the maximum number of tokens",
    ];
    if CTX.iter().any(|k| lower.contains(k)) {
        return LlmErrorKind::ContextLimit;
    }
    const OUT: &[&str] = &[
        "max_tokens", "max_output_tokens", "max output tokens", "maximum output tokens",
        "max_completion_tokens", "completion tokens",
    ];
    if OUT.iter().any(|k| lower.contains(k)) {
        return LlmErrorKind::OutputLimit;
    }
    const UP: &[&str] = &[
        "no provider supported", "has no provider", "no available provider",
        "model not found", "model_not_found", "no such model", "no healthy upstream",
        "upstream", "bad gateway",
    ];
    if UP.iter().any(|k| lower.contains(k)) {
        return LlmErrorKind::UpstreamUnavailable;
    }
    LlmErrorKind::Other
}

/// Conservative explicit value to try when even an *omitted* `max_tokens` was
/// rejected with an output-limit error and the body named no ceiling.
const OMITTED_REJECTED_FALLBACK: u32 = 4096;

/// Choose the next `max_tokens` state after an output-limit rejection.
/// Priority: stop guessing (omit) → the ceiling named in the body → half the
/// previous value → a conservative constant. Any state already in `tried` is
/// skipped, so the retry loop always terminates.
fn next_max_tokens_after_limit(
    body: &str,
    current: Option<u32>,
    tried: &[Option<u32>],
) -> Option<Option<u32>> {
    let mut candidates: Vec<Option<u32>> = Vec::new();
    // Preferred remedy: let the (possibly different) upstream decide.
    if current.is_some() {
        candidates.push(None);
    }
    if let Some(limit) = max_tokens_limit_from_error(body, current.unwrap_or(u32::MAX)) {
        candidates.push(Some(limit));
    }
    if let Some(v) = current
        && v > MIN_MAX_TOKENS
    {
        candidates.push(Some((v / 2).max(MIN_MAX_TOKENS)));
    }
    if current.is_none() {
        candidates.push(Some(OMITTED_REJECTED_FALLBACK));
    }
    candidates
        .into_iter()
        .find(|c| *c != current && !tried.contains(c))
}

/// [`chat`] with an explicit read timeout, so short utility calls (e.g. query
/// keyword rewriting) can fail fast instead of hanging on a stalled gateway.
pub fn chat_with_timeout(system: &str, user: &str, read_timeout_secs: u64) -> Option<String> {
    let cfg = crate::config::load_config();
    let ep = resolve_active_endpoint(&cfg, crate::config::ModelType::Llm)?;
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));

    let messages = vec![
        ChatMsg { role: "system".into(), content: system.into(), reasoning: None, reasoning_content: None },
        ChatMsg { role: "user".into(), content: user.into(), reasoning: None, reasoning_content: None },
    ];
    log::info!(
        "[AI]   → LLM: model={} chars={}",
        ep.model_id,
        user.chars().count()
    );

    // Gateways differ: some clamp an oversized max_tokens, some reject it with
    // 400, and routing/aggregator aliases may not even have a stable ceiling.
    // Strategy: send the detected ceiling when known, otherwise omit and let
    // the upstream decide; on an output-limit 400 step through
    // `next_max_tokens_after_limit` (which prefers omitting). Other error kinds
    // are not retried on max_tokens, so the real cause reaches the caller.
    let mut max_tokens = detected_max_tokens();
    let mut tried: Vec<Option<u32>> = vec![max_tokens];
    let mut upstream_retried = false;
    let mut parsed: Result<ChatResp, String> = Err("request not attempted".into());
    for _attempt in 0..5 {
        let req = ChatReq {
            model: ep.model_id.clone(),
            messages: messages.clone(),
            temperature: 0.3,
            max_tokens,
            stream: false,
        };
        let req_body = match serde_json::to_string(&req) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[AI] chat request build failed: {e}");
                return None;
            }
        };
        let send_result = build_agent_with_read_timeout(read_timeout_secs)
            .post(&url)
            .set("Content-Type", "application/json")
            .set_auth(&ep.api_key)
            .send_string(&req_body);

        match send_result {
            Ok(resp) => {
                parsed = resp
                    .into_string()
                    .map_err(|e| e.to_string())
                    .and_then(|body| parse_chat_response(&body));
                break;
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let kind = if (500..600).contains(&code) {
                    LlmErrorKind::UpstreamUnavailable
                } else {
                    classify_llm_error(&body)
                };
                log::warn!(
                    "[AI] chat request rejected (HTTP {code}, {kind:?}): {}",
                    body.trim().chars().take(200).collect::<String>()
                );
                let mut retry = false;
                match kind {
                    LlmErrorKind::OutputLimit => {
                        if let Some(next) = next_max_tokens_after_limit(&body, max_tokens, &tried) {
                            log::warn!(
                                "[AI] gateway rejected max_tokens={max_tokens:?}; retrying with {next:?}"
                            );
                            max_tokens = next;
                            tried.push(next);
                            retry = true;
                        }
                    }
                    // Routing gateways may land on a healthy upstream on retry.
                    LlmErrorKind::UpstreamUnavailable if !upstream_retried => {
                        upstream_retried = true;
                        log::warn!("[AI] upstream unavailable; retrying once");
                        retry = true;
                    }
                    _ => {}
                }
                if retry {
                    continue;
                }
                parsed = Err(body);
                break;
            }
            Err(e) => {
                parsed = Err(e.to_string());
                break;
            }
        }
    }

    match parsed {
        Ok(resp) => {
            let choice = resp.choices.into_iter().next();
            if choice
                .as_ref()
                .and_then(|c| c.finish_reason.as_deref())
                .is_some_and(|f| f.eq_ignore_ascii_case("length"))
            {
                log::warn!("[AI] response truncated by output limit (finish_reason=length)");
            }
            let content = choice.and_then(|c| {
                if !c.message.content.is_empty() {
                    Some(c.message.content)
                } else {
                    c.message.reasoning.filter(|r| !r.is_empty())
                        .or_else(|| c.message.reasoning_content.filter(|r| !r.is_empty()))
                }
            });
            log::info!(
                "[AI]   ← LLM: {} chars",
                content.as_deref().map(|c| c.chars().count()).unwrap_or(0)
            );
            content
        }
        Err(e) => {
            log::warn!("[AI] chat request failed: {e}");
            None
        }
    }
}

/// Query-time vocabulary rewrite: reads the library's high-frequency terms
/// from `hotword_counts`, feeds them alongside the user's question to the LLM,
/// and returns BM25-friendly keywords that bridge the vocabulary gap
/// (e.g., "跳槽" → "竞业限制") without predicting user questions offline.
pub fn vocabulary_rewrite(query: &str, conn: &rusqlite::Connection) -> Option<Vec<String>> {
    let keywords: Vec<String> = conn
        .prepare(
            "SELECT word FROM hotword_counts
             WHERE length(word) >= 2 AND count >= 20
             ORDER BY count DESC LIMIT 200",
        )
        .ok()?
        .query_map([], |r| r.get::<_, String>(0))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    if keywords.is_empty() {
        return None;
    }

    let system = "用户用口语提问，但文档库使用正式法律/商业术语。请根据用户问题和库内术语参考，输出适合全文检索的关键词。要求：1）使用库内术语替代口语词；2）保留用户问题中的专有名词和数字；3）只输出关键词，空格分隔，不要编号和解释。";
    let user = format!(
        "用户问题：{}\n\n库内术语参考：{}",
        query,
        keywords.join(" ")
    );

    let response = chat_with_timeout(system, &user, 20)?;
    let rewritten: Vec<String> = response
        .split_whitespace()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if rewritten.is_empty() {
        None
    } else {
        log::info!(
            "[AI] vocabulary_rewrite: {} chars → {} keywords: {:?}",
            query.chars().count(),
            rewritten.len(),
            rewritten
        );
        Some(rewritten)
    }
}

/// Outcome of a streaming chat call.
pub struct ChatStreamOutcome {
    pub text: Option<String>,
    pub took_ms: u64,
    pub cancelled: bool,
    /// Upstream stopped because it hit its output-token ceiling
    /// (`finish_reason == "length"`), so `text` is incomplete.
    pub truncated: bool,
    /// 失败原因（`text == None` 且非取消时才有值）。
    ///
    /// 调用方必须把它变成**用户可见**的信息 —— 以前 `text` 为 None 时被
    /// `unwrap_or_default()` 变成空回答，网关 400/401/断网的真实原因全部丢失，
    /// 界面只剩"没有回答"（见 `commands/ai.rs` 的两处调用）。
    pub error: Option<String>,
}

/// Send a chat-completion prompt in streaming mode (`stream: true`), invoking
/// `on_delta` for every content fragment as it arrives. Falls back to a plain
/// non-streamed response when the gateway ignores `stream` (first line is not
/// `data:`).
///
/// Non-cancellable convenience wrapper — see [`chat_stream_cancellable`].
pub fn chat_stream(
    system: &str,
    user: &str,
    on_delta: &mut dyn FnMut(&str, bool),
) -> ChatStreamOutcome {
    chat_stream_cancellable(system, user, None, on_delta)
}

/// [`chat_stream`] with a per-generation [`CancelToken`] (`docs/LARK_BOT.md`
/// §5.5 stage 1). Stops early when the token fires; the partial text is then
/// discarded by the caller.
pub fn chat_stream_cancellable(
    system: &str,
    user: &str,
    cancel: Option<&CancelToken>,
    on_delta: &mut dyn FnMut(&str, bool),
) -> ChatStreamOutcome {
    use std::io::{BufRead, Read};
    let is_cancelled = || cancel.is_some_and(CancelToken::is_cancelled);
    let started = std::time::Instant::now();
    let fail = |msg: String| ChatStreamOutcome {
        text: None,
        took_ms: 0,
        cancelled: false,
        truncated: false,
        error: Some(msg),
    };
    if !llm_enabled() {
        let reason = llm_unavailable_reason().unwrap_or("AI 服务未配置（请在设置页配置 AI 网关）");
        return fail(reason.to_string());
    }
    let cfg = crate::config::load_config();
    let Some(ep) = resolve_active_endpoint(&cfg, crate::config::ModelType::Llm) else {
        return fail("未找到可用的 LLM 模型（请在设置页选择模型）".to_string());
    };
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));

    // Build+send with the same adaptive policy as the non-streaming path. The
    // retry loop runs only before any delta is emitted, so a retry can never
    // duplicate streamed text.
    let messages = vec![
        ChatMsg { role: "system".into(), content: system.into(), reasoning: None, reasoning_content: None },
        ChatMsg { role: "user".into(), content: user.into(), reasoning: None, reasoning_content: None },
    ];
    let mut max_tokens = detected_max_tokens();
    let mut tried: Vec<Option<u32>> = vec![max_tokens];
    let mut upstream_retried = false;
    let mut reader = loop {
        let req_body = match serde_json::to_string(&ChatReq {
            model: ep.model_id.clone(),
            messages: messages.clone(),
            temperature: 0.3,
            max_tokens,
            stream: true,
        }) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[AI] chat stream build failed: {e}");
                return fail(format!("请求构造失败: {e}"));
            }
        };
        log::info!(
            "[AI] chat stream request: model={} user_chars={} max_tokens={max_tokens:?}",
            ep.model_id,
            user.chars().count()
        );

        match build_agent()
            .post(&url)
            .set("Content-Type", "application/json")
            .set_auth(&ep.api_key)
            .send_string(&req_body)
        {
            Ok(r) if (200..300).contains(&r.status()) => break r.into_reader(),
            Ok(r) => {
                // ureq normally maps non-2xx to Err(Status); kept for safety.
                let code = r.status();
                let mut body = String::new();
                let _ = r.into_string().map(|s| body = s);
                let snippet: String = body.trim().chars().take(200).collect();
                log::warn!("[AI] chat stream HTTP {code}: {snippet}");
                return fail(format!("AI 网关返回 HTTP {code}: {snippet}"));
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let kind = if (500..600).contains(&code) {
                    LlmErrorKind::UpstreamUnavailable
                } else {
                    classify_llm_error(&body)
                };
                log::warn!(
                    "[AI] chat stream rejected (HTTP {code}, {kind:?}): {}",
                    body.trim().chars().take(200).collect::<String>()
                );
                let mut retry = false;
                match kind {
                    LlmErrorKind::OutputLimit => {
                        if let Some(next) = next_max_tokens_after_limit(&body, max_tokens, &tried) {
                            log::warn!(
                                "[AI] chat stream rejected max_tokens={max_tokens:?}; retrying with {next:?}"
                            );
                            max_tokens = next;
                            tried.push(next);
                            retry = true;
                        }
                    }
                    LlmErrorKind::UpstreamUnavailable if !upstream_retried => {
                        upstream_retried = true;
                        log::warn!("[AI] chat stream upstream unavailable; retrying once");
                        retry = true;
                    }
                    _ => {}
                }
                if retry {
                    continue;
                }
                let snippet: String = body.trim().chars().take(200).collect();
                return fail(format!("AI 网关拒绝（HTTP {code}）: {snippet}"));
            }
            Err(e) => {
                log::warn!("[AI] chat stream failed: {e}");
                return fail(format!("网络错误: {e}"));
            }
        }
    };

    #[derive(serde::Deserialize)]
    struct StreamResp {
        choices: Vec<StreamChoice>,
    }
    #[derive(serde::Deserialize)]
    struct StreamChoice {
        #[serde(default)]
        finish_reason: Option<String>,
        delta: StreamDelta,
    }
    #[derive(serde::Deserialize)]
    struct StreamDelta {
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        reasoning: Option<String>,
        #[serde(default)]
        reasoning_content: Option<String>,
    }

    let mut full = String::new();
    let mut reasoning_buf = String::new();
    let mut cancelled = false;
    let mut finish_reason: Option<String> = None;
    let mut first_line = true;
    let mut line = String::new();
    let mut buf_reader = std::io::BufReader::new(&mut reader);
    loop {
        line.clear();
        match buf_reader.read_line(&mut line) {
            Ok(0) => break,
            Err(e) => {
                log::warn!("[AI] chat stream read error: {e}");
                break;
            }
            Ok(_) => {}
        }
        let l = line.trim();
        if first_line {
            first_line = false;
            if !l.starts_with("data:") {
                // Gateway ignored stream:true — read the rest as a plain body.
                if is_cancelled() {
                    cancelled = true;
                    break;
                }
                let mut rest = String::new();
                let _ = buf_reader.read_to_string(&mut rest);
                let mut body = line.clone();
                body.push_str(&rest);
                let parsed = parse_chat_response(&body).ok();
                if let Some(fr) = parsed
                    .as_ref()
                    .and_then(|r| r.choices.first())
                    .and_then(|c| c.finish_reason.as_deref())
                {
                    finish_reason = Some(fr.to_string());
                }
                let text = parsed
                    .and_then(|r| r.choices.into_iter().next().and_then(|c| {
                        if !c.message.content.is_empty() {
                            Some(c.message.content)
                        } else {
                            c.message.reasoning.filter(|r| !r.is_empty())
                                .or_else(|| c.message.reasoning_content.filter(|r| !r.is_empty()))
                        }
                    }))
                    .unwrap_or_default();
                if !text.is_empty() {
                    full = text.clone();
                    on_delta(&text, false);
                }
                break;
            }
        }
        if let Some(p) = l.strip_prefix("data:") {
            let p = p.trim();
            if p == "[DONE]" {
                break;
            }
            if let Ok(sr) = serde_json::from_str::<StreamResp>(p) {
                if let Some(fr) = sr.choices.first().and_then(|c| c.finish_reason.as_deref()) {
                    finish_reason = Some(fr.to_string());
                }
                if let Some(d) = sr.choices.first().and_then(|c| c.delta.content.as_ref()) {
                    if !d.is_empty() {
                        full.push_str(d);
                        on_delta(d, false);
                    }
                } else if let Some(d) = sr.choices.first().and_then(|c| c.delta.reasoning.as_ref())
                    && !d.is_empty() {
                        reasoning_buf.push_str(d);
                        on_delta(d, true);
                    } else if let Some(d) = sr.choices.first().and_then(|c| c.delta.reasoning_content.as_ref())
                        && !d.is_empty() {
                            reasoning_buf.push_str(d);
                            on_delta(d, true);
                        }
            }
        }
        if is_cancelled() {
            cancelled = true;
            break;
        }
    }

    // Fallback: when content is empty but reasoning arrived (some gateways
    // put the answer in reasoning_content with empty content), use the
    // reasoning text as the final answer — mirroring chat()'s behavior.
    if full.trim().is_empty() && !reasoning_buf.trim().is_empty() {
        full = reasoning_buf;
    }

    let truncated = finish_reason
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case("length"));
    if truncated {
        log::warn!(
            "[AI] chat stream truncated by output limit (finish_reason=length, chars={}): \
             the upstream model's output budget was too small",
            full.chars().count()
        );
    }

    let took_ms = started.elapsed().as_millis() as u64;
    if !cancelled {
        log::info!(
            "[AI] chat stream response: ok, content_chars={} took_ms={} truncated={}",
            full.chars().count(),
            took_ms,
            truncated
        );
    }
    ChatStreamOutcome { text: Some(full), took_ms, cancelled, truncated, error: None }
}

#[derive(Serialize, Deserialize, Clone)]
struct ChatMsg {
    role: String,
    content: String,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Serialize)]
struct ChatReq {
    model: String,
    messages: Vec<ChatMsg>,
    temperature: f32,
    /// Omitted entirely when the gateway doesn't report a ceiling — see
    /// [`detected_max_tokens`]. Sending a guessed value is what caused HTTP 400
    /// on gateways that validate (and on aggregators whose routed model varies).
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResp {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMsg,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// Parse a chat-completions response. Some OpenAI-compatible gateways
/// stream (SSE `data: {...}` frames) even with `stream: false`; falling
/// back to the last non-DONE payload keeps those working instead of
/// surfacing a spurious "trailing characters" failure.
fn parse_chat_response(body: &str) -> Result<ChatResp, String> {
    if let Ok(r) = serde_json::from_str::<ChatResp>(body) {
        return Ok(r);
    }
    body.lines()
        .filter_map(|l| l.strip_prefix("data:").map(str::trim)).rfind(|l| !l.is_empty() && *l != "[DONE]")
        .ok_or_else(|| "no SSE data payload in response".to_string())
        .and_then(|payload| {
            serde_json::from_str::<ChatResp>(payload).map_err(|e| e.to_string())
        })
}

pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < 1e-12 || nb < 1e-12 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn build_agent() -> ureq::Agent {
    // No total-request timeout: a reachable-but-slow local LLM must be able
    // to stream a long answer to completion. Connect fails fast (15s) so an
    // unreachable gateway errors immediately; the per-read cap (60min) is a
    // safety net, not a generation limit.
    build_agent_with_read_timeout(60 * 60)
}

fn build_agent_with_read_timeout(read_timeout_secs: u64) -> ureq::Agent {
    // 构建 agent 要新建 rustls ClientConfig（含根证书载入），而远程嵌入是**每组
    // 一次请求**（成千上万组）——旧实现每请求都重建一个；对话/嵌入的读超时只有
    // 少数几个取值，按值缓存即可（代理环境变量在启动时读取，改代理需重启，与缓存
    // 无关）。`ureq::Agent` 克隆只是 Arc 计数。
    static AGENTS: OnceLock<Mutex<HashMap<u64, ureq::Agent>>> = OnceLock::new();
    let cache = AGENTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(c) = cache.lock()
        && let Some(agent) = c.get(&read_timeout_secs)
    {
        return agent.clone();
    }
    let agent = build_agent_uncached(read_timeout_secs);
    if let Ok(mut c) = cache.lock() {
        // 超时取值本就有限；仍给一个上限，避免任何未来调用方按动态超时值把缓存撑大。
        if c.len() >= 16 {
            c.clear();
        }
        c.insert(read_timeout_secs, agent.clone());
    }
    agent
}

fn build_agent_uncached(read_timeout_secs: u64) -> ureq::Agent {
    let mut builder = ureq::builder()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(read_timeout_secs));
    for var in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(p) = std::env::var(var)
            && !p.is_empty() {
                if let Ok(proxy) = ureq::Proxy::new(&p) {
                    builder = builder.proxy(proxy);
                }
                break;
            }
    }
    builder.build()
}

/// Per-generation AI cancellation handle (`docs/LARK_BOT.md` §5.5 stage 1).
///
/// Replaces the former process-global `AI_CANCEL` flag: cancelling one
/// generation no longer aborts every other end's answer (desktop UI, Web and
/// Feishu used to share a single flag). Instances are owned by
/// [`crate::commands::sessions::in_flight`], which also exposes them for
/// cancellation by session id.
///
/// Semantics are one-shot and sticky: the flag is never reset, because each
/// generation gets a fresh token. A non-streaming request already in flight
/// completes in the background and the caller discards its result (it checks
/// [`Self::is_cancelled`] after the call returns).
#[derive(Debug, Default)]
pub struct CancelToken {
    cancelled: std::sync::atomic::AtomicBool,
}

impl CancelToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Per-gateway connectivity test result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewayTest {
    pub kind: &'static str,
    /// True when the gateway is configured, reachable and returned OK.
    pub ok: bool,
    /// Short reason when not ok (unconfigured / URL / HTTP status / parse).
    pub detail: String,
    /// Detected max output tokens from `/v1/models` capabilities (None = unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// Frontend-facing capability flags (from a cached test).
#[derive(Debug, Clone, Copy, serde::Serialize, Default)]
pub struct AiCapabilities {
    pub embedding: bool,
    pub llm: bool,
}

impl AiCapabilities {
    pub fn from_gateways((emb, llm): (bool, bool)) -> Self {
        Self { embedding: emb, llm }
    }
}

/// Ping both gateways with the smallest realistic request. Used by the
/// settings "test" button and for capability probes. The two probes run in
/// parallel: the LLM one may take many seconds to schedule a model, and
/// serial order would make it start only after the embedding probe finishes.
pub fn test_gateways() -> Vec<GatewayTest> {
    let failed = |kind: &'static str| GatewayTest { kind, ok: false, detail: "probe panicked".into(), max_output_tokens: None };
    let emb_handle = std::thread::spawn(test_embedding);
    let llm_handle = std::thread::spawn(test_llm);
    let tests = vec![
        emb_handle.join().unwrap_or_else(|_| failed("embedding")),
        llm_handle.join().unwrap_or_else(|_| failed("llm")),
    ];
    for t in &tests {
        log::info!("[AI] capability probe: {} = {} ({})", t.kind, t.ok, t.detail);
    }
    tests
}

/// Frontend-facing capability flags from CONFIGURATION ONLY (no network).
/// LLM is an optional feature: opening the chat page must not block on a
/// gateway probe (which the settings "test" button handles explicitly).
pub fn capabilities() -> (bool, bool) {
    (embedding_enabled(), llm_enabled())
}

/// 把底层网络错误翻译成用户能据此行动的信息。
///
/// macOS 15+ 的「本地网络」隐私会把**局域网**访问拒绝成
/// `EHOSTUNREACH (os error 65)`（"No route to host"），而同机的公网/网关却
/// 正常——用户只看到一个莫名的 "No route to host"，完全无从下手（见 CHANGELOG
/// 2026-10-09）。这里补一句可操作的提示。非 macOS 或非该错误时原样返回。
fn friendly_net_error(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    let unreachable = err.contains("os error 65") || lower.contains("no route to host");
    if cfg!(target_os = "macos") && unreachable {
        format!(
            "{err} —— 若目标在局域网内，可能是 macOS「本地网络」隐私拦截：\
             系统设置 → 隐私与安全性 → 本地网络，允许 Link-Searcher（或启动它的终端）；\
             也可改用该服务的 Tailscale / 局域网外地址"
        )
    } else {
        err.to_string()
    }
}

/// Pull a provider's model list from `GET {base_url}/models`, classified by
/// name heuristics. Returns `(models, err?)` — an error string when the
/// request failed (caller keeps the old list).
pub fn list_provider_models(base_url: &str, api_key: &str) -> (Vec<crate::config::ModelConfig>, Option<String>) {
    #[derive(Deserialize)]
    struct ModelsResp {
        #[serde(default)]
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
        #[serde(default)]
        capabilities: Option<ModelCapabilities>,
    }
    #[derive(Deserialize)]
    struct ModelCapabilities {
        #[serde(default, rename = "maxOutput")]
        max_output: Option<u32>,
    }

    // SSRF 护栏：拉模型列表是用户输入 base_url 的第一站，这里挡住云元数据等
    // 绝不该出现的地址（见 `validate_gateway_url`）。
    if let Err(e) = validate_gateway_url(base_url) {
        return (Vec::new(), Some(e));
    }

    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let agent = ureq::builder().timeout(std::time::Duration::from_secs(30)).build();
    let mut req = agent.get(&url);
    if !api_key.trim().is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", api_key.trim()));
    }
    let parsed: Result<ModelsResp, String> = req
        .call()
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|body| serde_json::from_str::<ModelsResp>(&body).map_err(|e| e.to_string()));
    match parsed {
        Ok(resp) => {
            let models = resp
                .data
                .into_iter()
                .map(|m| {
                    let id = m.id.clone();
                    let max_output_tokens = m.capabilities
                        .and_then(|c| c.max_output)
                        .filter(|&v| v > 0);
                    crate::config::ModelConfig {
                        id: id.clone(),
                        model_type: classify_model_by_name(&id),
                        enabled: false,
                        max_output_tokens,
                    }
                })
                .collect::<Vec<_>>();
            (models, None)
        }
        Err(e) => (Vec::new(), Some(friendly_net_error(&e))),
    }
}

fn test_embedding() -> GatewayTest {
    let cfg = crate::config::load_config();
    if crate::config::is_local_embedding_model(&cfg.active_embedding_model_id) {
        let model_name = local_embed::local_model_dir_name(&cfg.active_embedding_model_id).unwrap_or("bge-large-zh-v1.5");
        if !local_embed::bge_model_ready(&cfg.data_dir, model_name) {
            return GatewayTest { kind: "embedding", ok: false, detail: format!("{model_name} 模型未下载"), max_output_tokens: None };
        }
        return match local_embed::init_local_embedder(&cfg.data_dir, model_name) {
            Ok(()) => GatewayTest { kind: "embedding", ok: true, detail: format!("内置 {model_name}"), max_output_tokens: None },
            Err(e) => GatewayTest { kind: "embedding", ok: false, detail: e, max_output_tokens: None },
        };
    }
    let Some(ep) = resolve_active_endpoint(&cfg, ModelType::Embedding) else {
        return GatewayTest { kind: "embedding", ok: false, detail: "未配置".into(), max_output_tokens: None };
    };
    let url = format!("{}/embeddings", ep.base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "model": ep.model_id, "input": [""] });
    match ping_post(&url, &ep.api_key, &body, std::time::Duration::from_secs(5)) {
        Ok(()) => GatewayTest { kind: "embedding", ok: true, detail: "OK".into(), max_output_tokens: None },
        Err(e) => GatewayTest { kind: "embedding", ok: false, detail: e, max_output_tokens: None },
    }
}

fn test_llm() -> GatewayTest {
    let cfg = crate::config::load_config();
    let Some(ep) = resolve_active_endpoint(&cfg, ModelType::Llm) else {
        return GatewayTest { kind: "llm", ok: false, detail: "未配置".into(), max_output_tokens: None };
    };
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": ep.model_id,
        "messages": [{"role":"user","content":"ping"}],
        "max_tokens": 1
    });
    match ping_post(&url, &ep.api_key, &body, std::time::Duration::from_secs(30)) {
        Ok(()) => {
            // 检测值优先；网关不报 capabilities 时显示实际会使用的 fallback
            let max_out = fetch_llm_max_output(&ep.base_url, &ep.api_key, &ep.model_id)
                .or_else(|| Some(active_max_tokens()));
            if let Some(m) = max_out {
                log::info!("[AI] LLM model max_output_tokens = {m}");
            }
            GatewayTest { kind: "llm", ok: true, detail: "OK".into(), max_output_tokens: max_out }
        }
        Err(e) => GatewayTest { kind: "llm", ok: false, detail: e, max_output_tokens: None },
    }
}

/// Query the gateway's `/v1/models` endpoint for the active model's
/// `capabilities.maxOutput`. Gateway model ids may carry a namespace prefix
/// (9router: `agnes/agnes-3.0-flash` vs configured `agnes-3.0-flash`), so the
/// lookup matches by exact id or by the last `/`-separated segment.
fn fetch_llm_max_output(base_url: &str, api_key: &str, model_id: &str) -> Option<u32> {
    let models_url = format!("{}/models", base_url.trim_end_matches('/'));
    let agent = ureq::builder().timeout(std::time::Duration::from_secs(10)).build();
    let resp = agent.get(&models_url)
        .set("Authorization", &format!("Bearer {}", api_key))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&resp).ok()?;
    v.get("data")?.as_array()?.iter()
        .find(|m| {
            let gid = m.get("id").and_then(|id| id.as_str()).unwrap_or("");
            gid == model_id || gid.split('/').next_back() == Some(model_id)
        })
        .and_then(|m| {
            m.get("capabilities").and_then(|c| c.get("maxOutput")).and_then(|v| v.as_u64())
                .or_else(|| m.get("max_completion_tokens").and_then(|v| v.as_u64()))
        })
        .map(|v| v as u32)
}

/// Issue a tiny POST and return Ok on any 2xx. Rejects 4xx/5xx with the
/// status text, and network errors with the transport message. `timeout`
/// differs per gateway: combo proxies (9router) take 4-12s to schedule a
/// model, so a 5s probe would flakily "fail" a healthy gateway.
fn ping_post(
    url: &str,
    key: &str,
    body: &serde_json::Value,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let agent = ureq::builder().timeout(timeout).build();
    let send = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set_auth(key)
        .send_string(&body.to_string());
    match send {
        Ok(r) => {
            let status = r.status();
            if (200..300).contains(&status) {
                Ok(())
            } else {
                let mut err = String::new();
                let _ = r.into_string().map(|s| err = s);
                Err(format!("HTTP {status}: {}", err.trim().chars().take(120).collect::<String>()))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

trait AuthSetter {
    fn set_auth(self, key: &str) -> Self;
}
impl AuthSetter for ureq::Request {
    fn set_auth(self, key: &str) -> Self {
        let key = key.trim();
        if key.is_empty() {
            self
        } else {
            self.set("Authorization", &format!("Bearer {key}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LRU 语义：容量满时淘汰**最久未用**的一条，而不是清空整个热集。
    #[test]
    fn query_embed_cache_evicts_least_recently_used() {
        let mut c = QueryEmbedCache::default();
        for i in 0..QUERY_EMBED_CACHE_CAP {
            c.insert(format!("k{i}"), vec![i as f32]);
        }
        assert_eq!(c.map.len(), QUERY_EMBED_CACHE_CAP);
        // 让 k0 变成"刚用过"，再插一个新键：应淘汰 k1（此时最久未用），k0 必须留着。
        assert!(c.get("k0").is_some());
        c.insert("fresh".to_string(), vec![0.0]);
        assert!(c.map.contains_key("k0"), "刚用过的键不能被淘汰");
        assert!(!c.map.contains_key("k1"), "应淘汰最久未用者");
        assert!(c.map.contains_key("fresh"));
        assert_eq!(c.map.len(), QUERY_EMBED_CACHE_CAP, "容量恒不超上限");
    }

    /// SSRF 护栏：只堵云元数据/链路本地，回环与私网必须**放行**
    /// （本机 Ollama、局域网的 vLLM / llama.cpp 都是合法用法）。
    #[test]
    fn gateway_url_rejects_metadata_but_allows_local_and_private() {
        // 放行：回环 / 私网 / 公网 / 域名
        for ok in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:8080",
            "https://192.168.1.20:8000/v1",
            "http://10.0.0.5:8000",
            "https://api.openai.com/v1",
            "https://my-gateway.example.com",
            "http://[::1]:11434",
        ] {
            assert!(validate_gateway_url(ok).is_ok(), "应放行: {ok}");
        }

        // 拒绝：云元数据 / 链路本地 / 非 http(s) / 空
        for bad in [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.0.1",
            "http://[fe80::1]:80",
            "http://[::ffff:169.254.169.254]/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "ftp://169.254.169.254",
            "file:///etc/passwd",
            "",
        ] {
            assert!(validate_gateway_url(bad).is_err(), "应拒绝: {bad}");
        }
    }

    #[test]
    fn cosine_identical_is_one() {
        let a = [1.0, 0.0, 2.0, -3.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        assert!((cosine(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_len_is_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn normalize_unit_length() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] * v[0] + v[1] * v[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn chunk_scan_filters_sorts_truncates() {
        let q = vec![1.0, 0.0, 0.0];
        let rows = vec![
            ("m1".to_string(), 0usize, vec![1.0, 0.0, 0.0]),      // cos 1.0
            ("m2".to_string(), 3usize, vec![0.9, 0.1, 0.0]),      // cos ~0.994
            ("m3".to_string(), 1usize, vec![0.0, 1.0, 0.0]),      // cos 0.0 (below 0.5)
            ("m4".to_string(), 2usize, vec![0.5, 0.5, 0.0]),      // cos ~0.707
        ];
        let out = chunk_vector_scan_impl(&q, &rows, 0.5, 10, 0);
        // 阈值过滤掉 m3，按相似度降序
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, "m1");
        assert_eq!(out[1].0, "m2");
        assert_eq!(out[2].0, "m4");
        // top_k 截断
        let out2 = chunk_vector_scan_impl(&q, &rows, 0.0, 2, 0);
        assert_eq!(out2.len(), 2);
        assert_eq!(out2[0].0, "m1");
        assert_eq!(out2[1].0, "m2");
    }

    #[test]
    fn chunk_scan_releases_below_threshold_candidates() {
        // 阈值只判「强相关」；只有在**强命中为空**（阈值会把通道清空）时，
        // 才保底放行相似度最高的 release_top_k 份弱候选。
        let q = vec![1.0, 0.0, 0.0];
        let rows = vec![
            ("m1".to_string(), 0usize, vec![1.0, 0.0, 0.0]),   // cos 1.0 强
            ("m2".to_string(), 1usize, vec![0.0, 1.0, 0.0]),   // cos 0.0 弱
            ("m3".to_string(), 2usize, vec![0.5, 0.5, 0.0]),   // cos ~0.707 弱
        ];
        // 无保底 → 只有强命中
        let strict = chunk_vector_scan_impl(&q, &rows, 0.8, 10, 0);
        assert_eq!(strict.len(), 1);
        assert_eq!(strict[0].0, "m1");
        // 已有强命中 → 不再追加弱候选（避免给健康查询制造噪声）
        let no_noise = chunk_vector_scan_impl(&q, &rows, 0.8, 10, 5);
        assert_eq!(no_noise.len(), 1);
        assert_eq!(no_noise[0].0, "m1");
        // 阈值清空通道（高于所有相似度）→ 保底放行最相似的弱候选
        let released = chunk_vector_scan_impl(&q, &rows, 1.5, 10, 1);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, "m1");
        assert!(released[0].2 < 1.5, "保底候选必须低于阈值");
        // top_k 仍生效
        let capped = chunk_vector_scan_impl(&q, &rows, 1.5, 1, 5);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].0, "m1");
    }

    #[test]
    fn embed_degrades_when_unconfigured() {
        // Environment-independent: embed must never panic or hang, whether
        // or not the host has a gateway configured. Without config it must
        // degrade to None; with config it may talk to a real gateway (short
        // timeout), so we only assert it completes.
        let r = embed_batch(&["hello".to_string()]);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn empty_batch_returns_same_len_none() {
        let r = embed_batch(&[]);
        assert!(r.is_empty());
    }

    #[test]
    fn truncate_for_embed_caps_length() {
        let short = "abc";
        assert_eq!(truncate_for_embed(short), "abc"); // unchanged
        let long = "x".repeat(EMBED_MAX_CHARS + 500);
        let t = truncate_for_embed(&long);
        assert_eq!(t.chars().count(), EMBED_MAX_CHARS);
        assert!(long.starts_with(&t));
    }

    #[test]
    fn tuner_grows_batch_then_concurrency() {
        use crate::config::EmbedPlan;
        let mut p = EmbedPlan { batch: 64, conc: 1, tok_per_s: 0.0, updated: 0 };
        p = next_embed_plan(p, EmbedSample { tok_per_s: 100.0, items: 256, errors: 0 });
        assert!(p.batch > 64, "batch should grow, got {}", p.batch);
        assert_eq!(p.conc, 1, "concurrency stays 1 until the batch caps");
        for _ in 0..12 {
            let improved = p.tok_per_s * 2.0 + 1.0;
            p = next_embed_plan(p, EmbedSample { tok_per_s: improved, items: 256, errors: 0 });
        }
        assert_eq!(p.batch, EMBED_BATCH_MAX, "batch should reach the cap");
        assert!(p.conc > 1, "concurrency should grow after the batch caps, got {}", p.conc);
    }

    #[test]
    fn tuner_backs_off_on_error() {
        use crate::config::EmbedPlan;
        let p = EmbedPlan { batch: 128, conc: 4, tok_per_s: 500.0, updated: 0 };
        let n = next_embed_plan(p, EmbedSample { tok_per_s: 0.0, items: 256, errors: 1 });
        assert!(n.batch < 128 && n.batch >= EMBED_BATCH_MIN, "batch {} out of range", n.batch);
        assert!(n.conc <= 2 && n.conc >= EMBED_CONC_MIN, "conc {} out of range", n.conc);
    }

    /// 落盘门槛：只有**结构**（batch/conc）变化才值得写盘 —— 平台期每轮都会改
    /// `tok_per_s`/`updated`，若按"变了就写"就变成**每次嵌入调用**都整份 fsync 写回。
    #[test]
    fn plan_persists_only_on_structural_change() {
        use crate::config::EmbedPlan;
        let base = EmbedPlan { batch: 128, conc: 2, tok_per_s: 1000.0, updated: 1 };
        // 平台期衰减 / 时间戳变化（纯数值）→ 不落盘
        let decayed = EmbedPlan { tok_per_s: 970.0, updated: 2, ..base };
        assert!(!plan_should_persist(base, decayed));
        // 爬坡台阶 / 退避（结构变化）→ 落盘
        assert!(plan_should_persist(base, EmbedPlan { batch: 192, ..base }));
        assert!(plan_should_persist(base, EmbedPlan { conc: 3, ..base }));
        assert!(plan_should_persist(base, EmbedPlan { batch: 64, conc: 1, ..base }));
    }

    #[test]
    fn tuner_backs_off_on_regression_but_holds_on_plateau() {
        use crate::config::EmbedPlan;
        let p = EmbedPlan { batch: 256, conc: 2, tok_per_s: 400.0, updated: 0 };
        // Plateau (within ±10%): hold position.
        let held = next_embed_plan(p, EmbedSample { tok_per_s: 400.0, items: 256, errors: 0 });
        assert_eq!(held.batch, 256);
        assert_eq!(held.conc, 2);
        // Clear regression: step back a notch.
        let down = next_embed_plan(p, EmbedSample { tok_per_s: 200.0, items: 256, errors: 0 });
        assert!(down.batch < 256, "regression should shrink the batch, got {}", down.batch);
    }

    #[test]
    fn tuner_ignores_zero_sample() {
        use crate::config::EmbedPlan;
        let p = EmbedPlan { batch: 64, conc: 2, tok_per_s: 100.0, updated: 0 };
        assert_eq!(next_embed_plan(p, EmbedSample::default()), p);
    }

    #[test]
    fn tuner_respects_bounds() {
        use crate::config::EmbedPlan;
        let mut p = EmbedPlan { batch: EMBED_BATCH_MIN, conc: EMBED_CONC_MIN, tok_per_s: 0.0, updated: 0 };
        for _ in 0..40 {
            let improved = p.tok_per_s + 100.0;
            p = next_embed_plan(p, EmbedSample { tok_per_s: improved, items: 256, errors: 0 });
            assert!(p.batch >= EMBED_BATCH_MIN && p.batch <= EMBED_BATCH_MAX, "batch {}", p.batch);
            assert!(p.conc >= EMBED_CONC_MIN && p.conc <= EMBED_CONC_MAX, "conc {}", p.conc);
        }
    }

    #[test]
    fn embed_plan_key_trims_trailing_slash() {
        assert_eq!(embed_plan_key("http://h:1/v1/", "bge-m3"), "http://h:1/v1|bge-m3");
    }

    #[test]
    fn classify_model_by_name_heuristics() {
        use crate::config::ModelType;
        assert_eq!(classify_model_by_name("bge-m3-mlx-fp16"), ModelType::Embedding);
        assert_eq!(classify_model_by_name("gte-m3"), ModelType::Embedding);
        assert_eq!(classify_model_by_name("minimax-m3"), ModelType::Llm, "M3 is a multimodal LLM, not embedding");
        assert_eq!(classify_model_by_name("text-embedding-v3-small"), ModelType::Embedding);
        assert_eq!(classify_model_by_name("nomic-embed-text"), ModelType::Embedding);
        assert_eq!(classify_model_by_name("qwen2.5-7b-instruct"), ModelType::Llm);
        assert_eq!(classify_model_by_name("Huihui-gemma-4-12B-it-abliterated-mlx-4bit"), ModelType::Llm);
        assert_eq!(classify_model_by_name("gpt-4o"), ModelType::Llm);
        assert_eq!(classify_model_by_name("custom-thing"), ModelType::Llm);
        assert_eq!(classify_model_by_name("Qwen-EMBED-2"), ModelType::Embedding);
    }

    #[test]
    fn friendly_net_error_hints_only_for_unreachable() {
        // 普通错误原样返回，不引入噪音。
        assert_eq!(friendly_net_error("boom"), "boom");
        assert_eq!(friendly_net_error("status code 404"), "status code 404");
        // EHOSTUNREACH：macOS 上追加可操作提示，其它平台原样。
        let e = "http://192.168.1.50:20128/v1/models: Connection Failed: Connect error: No route to host (os error 65)";
        let out = friendly_net_error(e);
        if cfg!(target_os = "macos") {
            assert!(out.starts_with(e));
            assert!(out.contains("本地网络"));
        } else {
            assert_eq!(out, e);
        }
    }

    #[test]
    fn resolve_active_endpoint_valid_and_dangling() {
        use crate::config::{AppConfig, ModelConfig, ProviderConfig};
        let cfg = AppConfig {
            providers: vec![ProviderConfig {
                id: "p1".into(),
                name: "x".into(),
                base_url: "http://x/v1".into(),
                api_key: "k".into(),
                models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Embedding, enabled: false, max_output_tokens: None }],
            }],
            active_embedding_model_id: "p1:m1".into(),
            active_llm_model_id: "p1:ghost".into(),
            ..AppConfig::default()
        };
        let ep = resolve_active_endpoint(&cfg, ModelType::Embedding).expect("valid active");
        assert_eq!(ep.base_url, "http://x/v1");
        assert_eq!(ep.api_key, "k");
        assert_eq!(ep.model_id, "m1");
        assert!(resolve_active_endpoint(&cfg, ModelType::Llm).is_none(), "dangling llm must be None");
        assert!(resolve_active_endpoint(&cfg, ModelType::Unknown).is_none());
    }

    #[test]
    fn chat_degrades_when_unconfigured() {
        // Environment-independent: chat must complete (None when not
        // configured; Some/None against a real gateway within timeout).
        let _ = chat("sys", "user");
    }

    #[test]
    fn llm_unavailable_reason_distinguishes_absent_and_dangling() {
        // 未配置（active_llm_model_id 为空）→ 提示"未配置"。
        // 指向不存在的 provider 或模型 → 提示"不可用/被删"。
        // 指向有效模型 → None（可用）。
        use crate::config::{AppConfig, ModelConfig, ProviderConfig};
        let provider = ProviderConfig {
            id: "p1".into(),
            name: "x".into(),
            base_url: "http://x/v1".into(),
            api_key: "k".into(),
            models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Llm, enabled: true, max_output_tokens: None }],
        };
        let with_active = |active: &str, providers: Vec<ProviderConfig>| AppConfig {
            active_llm_model_id: active.into(),
            providers,
            ..AppConfig::default()
        };
        assert!(llm_unavailable_reason_for(&with_active("", vec![])).is_some());
        assert_eq!(
            llm_unavailable_reason_for(&with_active("p1:ghost", vec![provider.clone()])),
            Some("当前使用的 LLM 模型不可用，请在设置页重新选择")
        );
        assert_eq!(
            llm_unavailable_reason_for(&with_active("pX:m1", vec![provider.clone()])),
            Some("当前使用的 LLM 网关已被删除，请在设置页重新选择")
        );
        assert_eq!(llm_unavailable_reason_for(&with_active("p1:m1", vec![provider])), None);
    }

    #[test]
    fn gateways_probe_reports_one_entry_per_role() {
        // 只断言**与环境无关**的结构性事实。
        //
        // ⚠️ 这里原来还断言"未配置 ⇒ ok=false"，但它依赖两样**进程级全局**，并行跑时互相打架：
        //   ① 网关探测结果有 **30s 缓存**（`test_ai_gateway` / `capabilities`），别的测试可能已把
        //      "有网关、ok=true"缓存进去；
        //   ② **`LS_CONFIG_DIR` 是进程级环境变量**，`commands/backup.rs` 的测试会临时 `set_var` 指向
        //      一个只有 `{"theme":"dark"}` 的一次性配置目录 —— 于是 `llm_enabled()` 读到"没网关"，
        //      而缓存里是"有网关" → 两者不一致，断言随机失败（2026-10-15 三次复现，且 HEAD 上同样会红）。
        //   结论：这类"读真实配置 + 真实网络探测"的断言**不可放进门禁**（AGENTS.md 门禁 ④）。
        let tests = test_gateways();
        assert_eq!(tests.len(), 2, "embedding / llm 各一条");
        for t in &tests {
            assert!(!t.kind.is_empty(), "每条都要有 kind，UI 依赖它归因");
            assert!(!t.detail.is_empty(), "每条都要有 detail，便于排查");
        }
        let _ = capabilities();
    }

    #[test]
    fn parse_chat_response_handles_plain_json() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"你好"}}]}"#;
        let resp = parse_chat_response(body).unwrap();
        assert_eq!(resp.choices[0].message.content, "你好");
    }

    #[test]
    fn max_tokens_limit_parses_agnes_error() {
        let body = r#"{"error":{"message":"max_tokens 不能超过 65536 (request id: 20260919135013169628929UKzobVY6)"}}"#;
        assert_eq!(max_tokens_limit_from_error(body, 1_000_000), Some(65536));
    }

    #[test]
    fn max_tokens_limit_ignores_echoed_request_size() {
        let body = "max_tokens=1000000 rejected; maximum allowed is 65536";
        assert_eq!(max_tokens_limit_from_error(body, 1_000_000), Some(65536));
    }

    #[test]
    fn max_tokens_limit_none_without_mention() {
        assert_eq!(max_tokens_limit_from_error("context length exceeded", 1000), None);
    }

    #[test]
    fn max_tokens_limit_never_exceeds_current() {
        assert_eq!(max_tokens_limit_from_error("max_tokens cannot exceed 65536", 4096), None);
    }

    #[test]
    fn classify_error_identifies_each_kind() {
        let agnes = r#"{"error":{"message":"max_tokens 不能超过 65536"}}"#;
        assert_eq!(classify_llm_error(agnes), LlmErrorKind::OutputLimit);
        let openai_ctx = "This model's maximum context length is 8192 tokens, however your messages resulted in 9000 tokens";
        assert_eq!(classify_llm_error(openai_ctx), LlmErrorKind::ContextLimit);
        let routing = "Model id : deepseek-ai/DeepSeek-V4-Flash , has no provider supported";
        assert_eq!(classify_llm_error(routing), LlmErrorKind::UpstreamUnavailable);
        assert_eq!(classify_llm_error("something odd"), LlmErrorKind::Other);
    }

    #[test]
    fn classify_context_wins_over_incidental_max_tokens_mention() {
        // Context-limit bodies often mention max_tokens too; shrinking the
        // output budget would never fix them.
        let body = "maximum context length exceeded: reduce the length of the messages (max_tokens was 4096)";
        assert_eq!(classify_llm_error(body), LlmErrorKind::ContextLimit);
    }

    #[test]
    fn next_max_tokens_prefers_omitting_after_rejection() {
        // Detected 1_000_000 rejected → first remedy is to omit, not to guess.
        let body = r#"{"error":{"message":"max_tokens 不能超过 65536"}}"#;
        assert_eq!(
            next_max_tokens_after_limit(body, Some(1_000_000), &[Some(1_000_000)]),
            Some(None)
        );
    }

    #[test]
    fn next_max_tokens_uses_named_ceiling_when_omission_already_rejected() {
        let body = r#"{"error":{"message":"max_tokens 不能超过 65536"}}"#;
        let tried = [Some(1_000_000), None];
        assert_eq!(
            next_max_tokens_after_limit(body, None, &tried),
            Some(Some(65536))
        );
    }

    #[test]
    fn next_max_tokens_terminates_when_all_candidates_tried() {
        let body = r#"{"error":{"message":"max_tokens 不能超过 65536"}}"#;
        let tried = [Some(1_000_000), None, Some(32768)];
        assert_eq!(next_max_tokens_after_limit(body, Some(65536), &tried), None);
    }

    #[test]
    fn chat_req_omits_max_tokens_when_none() {
        let req = ChatReq {
            model: "m".into(),
            messages: vec![],
            temperature: 0.3,
            max_tokens: None,
            stream: true,
        };
        let body = serde_json::to_string(&req).unwrap();
        assert!(!body.contains("max_tokens"), "None must be omitted, got {body}");
    }

    #[test]
    fn chat_turn_guard_tracks_in_flight_and_releases_on_drop() {
        assert!(!chat_in_flight(), "baseline should be idle");
        {
            let _a = ChatTurnGuard::begin();
            assert!(chat_in_flight());
            let _b = ChatTurnGuard::begin();
            assert!(chat_in_flight());
        }
        assert!(!chat_in_flight(), "guard must release on drop");
    }

    #[test]
    fn parse_chat_response_falls_back_to_sse_frames() {
        // Some gateways stream (SSE) even with stream:false — the last
        // non-DONE payload must win.
        let body = concat!(
            "data: {\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"早\"}}]}\n\n",
            "data: {\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"你好\"}}]}\n\n",
            "data: [DONE]\n",
        );
        let resp = parse_chat_response(body).unwrap();
        assert_eq!(resp.choices[0].message.content, "你好");
    }

    #[test]
    fn parse_chat_response_rejects_garbage() {
        assert!(parse_chat_response("<html>gateway error</html>").is_err());
    }

    #[test]
    fn parse_chat_response_reads_reasoning_content_field() {
        // 网关返回 reasoning_content 而非 reasoning（如 opencode 网关）：
        // content 为空时必须从 reasoning_content 提取正文。
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"万城公司相关资料显示..."}}]}"#;
        let resp = parse_chat_response(body).unwrap();
        assert!(resp.choices[0].message.reasoning_content.is_some());
        assert_eq!(resp.choices[0].message.reasoning_content.as_deref(), Some("万城公司相关资料显示..."));
    }

    /// 测量本地 BGE 模型加载与单次推理耗时（手动运行：cargo test -- --ignored）
    #[test]
    #[ignore]
    fn bench_local_embed_latency() {
        let cfg = crate::config::load_config();
        let model = "bge-large-zh-v1.5";
        let t0 = std::time::Instant::now();
        crate::ai::local_embed::init_local_embedder(&cfg.data_dir, model).unwrap();
        eprintln!("[BENCH] model load: {:?}", t0.elapsed());
        let t1 = std::time::Instant::now();
        let v = crate::ai::local_embed::embed_query_local("常宏");
        eprintln!("[BENCH] first infer: {:?} dim={:?}", t1.elapsed(), v.map(|x| x.len()));
        let t2 = std::time::Instant::now();
        let v2 = crate::ai::local_embed::embed_query_local("民事案件");
        eprintln!("[BENCH] second infer: {:?} dim={:?}", t2.elapsed(), v2.map(|x| x.len()));
    }
}
