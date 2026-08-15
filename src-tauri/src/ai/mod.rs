//! OpenAI-compatible AI gateway (embeddings + chat) and vector helpers.
//!
//! The gateway is configured via `AppConfig` (`ai_api_base` etc.). An empty
//! `ai_api_base` disables the feature; every public entry point degrades
//! gracefully to `None`/empty when unconfigured or unreachable.

use std::collections::HashMap;

use crate::config::{ModelType, ProviderConfig};
use serde::{Deserialize, Serialize};

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
}

/// Resolve the endpoint for a model role from the active selection
/// (`provider_id:model_id`). Returns None when unconfigured or dangling.
fn resolve_active_endpoint(cfg: &crate::config::AppConfig, kind: ModelType) -> Option<ActiveEndpoint> {
    let active_id = match kind {
        ModelType::Embedding => &cfg.active_embedding_model_id,
        ModelType::Llm => &cfg.active_llm_model_id,
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
    Some(ActiveEndpoint::new(provider, model_id))
}

/// Classify a model id by name heuristics. Pure function, user-overridable.
pub fn classify_model_by_name(id: &str) -> ModelType {
    let lower = id.to_lowercase();
    const EMBED_HINTS: &[&str] = &[
        "embed", "text-embedding", "bge", "minilm", "e5-", "gte-", "nomic-embed",
        "jina-embeddings", "mxbai-embed", "all-minilm",
    ];
    const LLM_HINTS: &[&str] = &[
        "instruct", "chat", "llm", "gpt", "qwen", "deepseek", "gemma", "llama", "mistral",
        "mixtral", "yi-", "glm", "phi", "command-r", "claude", "gemini",
    ];
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
    resolve_active_endpoint(&crate::config::load_config(), ModelType::Embedding).is_some()
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

/// Embed texts in `batch_size` chunks; failed texts map to `None`.
pub fn embed_batched(texts: &[String], batch_size: usize) -> Vec<Option<Vec<f32>>> {
    let batch_size = batch_size.max(1);
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(batch_size) {
        let chunk: Vec<String> = chunk.iter().map(|t| truncate_for_embed(t)).collect();
        out.extend(embed_batch(&chunk));
    }
    out
}

pub fn embed_batch(texts: &[String]) -> Vec<Option<Vec<f32>>> {
    let cfg = crate::config::load_config();
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

pub fn embed(text: &str) -> Option<Vec<f32>> {
    embed_batch(&[text.to_string()]).into_iter().next().flatten()
}

/// Send a chat-completion prompt to the configured LLM and return the reply
/// text. `system` is the instruction, `user` the task/content. Returns `None`
/// when unconfigured or the request fails (downgrade, never block).
pub fn chat(system: &str, user: &str) -> Option<String> {
    let cfg = crate::config::load_config();
    let Some(ep) = resolve_active_endpoint(&cfg, crate::config::ModelType::Llm) else {
        return None;
    };
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));

    let req = ChatReq {
        model: ep.model_id.clone(),
        messages: vec![
            ChatMsg { role: "system".into(), content: system.into() },
            ChatMsg { role: "user".into(), content: user.into() },
        ],
        temperature: 0.3,
        max_tokens: 4096,
        stream: false,
    };
    let req_body = match serde_json::to_string(&req) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("[AI] chat request build failed: {e}");
            return None;
        }
    };
    log::info!(
        "[AI] chat request: model={} user_chars={}",
        ep.model_id,
        user.chars().count()
    );
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth(&ep.api_key)
        .send_string(&req_body);

    let parsed: Result<ChatResp, String> = send_result
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|body| parse_chat_response(&body));

    match parsed {
        Ok(resp) => {
            let content = resp.choices.into_iter().next().map(|c| c.message.content);
            log::info!(
                "[AI] chat response: ok, content_chars={}",
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

/// Outcome of a streaming chat call.
pub struct ChatStreamOutcome {
    pub text: Option<String>,
    pub took_ms: u64,
    pub cancelled: bool,
}

/// Send a chat-completion prompt in streaming mode (`stream: true`), invoking
/// `on_delta` for every content fragment as it arrives. Falls back to a plain
/// non-streamed response when the gateway ignores `stream` (first line is not
/// `data:`). Stops early when [`cancel_ai`] fires; the partial text is then
/// discarded by the caller.
pub fn chat_stream(
    system: &str,
    user: &str,
    on_delta: &mut dyn FnMut(&str),
) -> ChatStreamOutcome {
    use std::io::{BufRead, Read};
    let started = std::time::Instant::now();
    let failed = ChatStreamOutcome { text: None, took_ms: 0, cancelled: false };
    if !llm_enabled() {
        return failed;
    }
    let cfg = crate::config::load_config();
    let Some(ep) = resolve_active_endpoint(&cfg, crate::config::ModelType::Llm) else {
        return failed;
    };
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));

    let req_body = match serde_json::to_string(&ChatReq {
        model: ep.model_id.clone(),
        messages: vec![
            ChatMsg { role: "system".into(), content: system.into() },
            ChatMsg { role: "user".into(), content: user.into() },
        ],
        temperature: 0.3,
        max_tokens: 4096,
        stream: true,
    }) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("[AI] chat stream build failed: {e}");
            return failed;
        }
    };
    log::info!(
        "[AI] chat stream request: model={} user_chars={}",
        ep.model_id,
        user.chars().count()
    );

    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth(&ep.api_key)
        .send_string(&req_body);
    let mut reader = match send_result {
        Ok(r) if (200..300).contains(&r.status()) => r.into_reader(),
        Ok(r) => {
            let mut err = String::new();
            let _ = r.into_string().map(|s| err = s);
            log::warn!("[AI] chat stream HTTP error: {}", err.trim().chars().take(200).collect::<String>());
            return failed;
        }
        Err(e) => {
            log::warn!("[AI] chat stream failed: {e}");
            return failed;
        }
    };

    #[derive(serde::Deserialize)]
    struct StreamResp {
        choices: Vec<StreamChoice>,
    }
    #[derive(serde::Deserialize)]
    struct StreamChoice {
        delta: StreamDelta,
    }
    #[derive(serde::Deserialize)]
    struct StreamDelta {
        #[serde(default)]
        content: Option<String>,
    }

    let mut full = String::new();
    let mut cancelled = false;
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
                let mut rest = String::new();
                let _ = buf_reader.read_to_string(&mut rest);
                let mut body = line.clone();
                body.push_str(&rest);
                let text = parse_chat_response(&body)
                    .ok()
                    .and_then(|r| r.choices.into_iter().next().map(|c| c.message.content))
                    .unwrap_or_default();
                if !text.is_empty() {
                    full = text.clone();
                    on_delta(&text);
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
                if let Some(d) = sr.choices.first().and_then(|c| c.delta.content.as_ref()) {
                    if !d.is_empty() {
                        full.push_str(d);
                        on_delta(d);
                    }
                }
            }
        }
        if ai_cancelled() {
            cancelled = true;
            break;
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    if !cancelled {
        log::info!(
            "[AI] chat stream response: ok, content_chars={} took_ms={}",
            full.chars().count(),
            took_ms
        );
    }
    ChatStreamOutcome { text: Some(full), took_ms, cancelled }
}

#[derive(Serialize, Deserialize)]
struct ChatMsg {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatReq {
    model: String,
    messages: Vec<ChatMsg>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResp {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMsg,
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
        .filter_map(|l| l.strip_prefix("data:").map(str::trim))
        .filter(|l| !l.is_empty() && *l != "[DONE]")
        .last()
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
    let mut builder = ureq::builder()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(60 * 60));
    for var in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                if let Ok(proxy) = ureq::Proxy::new(&p) {
                    builder = builder.proxy(proxy);
                }
                break;
            }
        }
    }
    builder.build()
}

/// One-shot AI cancellation flag. Set by the frontend "cancel" action; the
/// in-flight chat request completes in the background but its result is
/// discarded (the caller checks the flag after the call returns).
static AI_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cancel_ai() {
    AI_CANCEL.store(true, std::sync::atomic::Ordering::Release);
}

pub fn reset_ai_cancel() {
    AI_CANCEL.store(false, std::sync::atomic::Ordering::Release);
}

pub fn ai_cancelled() -> bool {
    AI_CANCEL.load(std::sync::atomic::Ordering::Acquire)
}

/// Per-gateway connectivity test result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewayTest {
    pub kind: &'static str,
    /// True when the gateway is configured, reachable and returned OK.
    pub ok: bool,
    /// Short reason when not ok (unconfigured / URL / HTTP status / parse).
    pub detail: String,
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
    let failed = |kind: &'static str| GatewayTest { kind, ok: false, detail: "probe panicked".into() };
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
                    let id = m.id;
                    crate::config::ModelConfig {
                        id: id.clone(),
                        model_type: classify_model_by_name(&id),
                        enabled: false,
                    }
                })
                .collect::<Vec<_>>();
            (models, None)
        }
        Err(e) => (Vec::new(), Some(e)),
    }
}

fn test_embedding() -> GatewayTest {
    let cfg = crate::config::load_config();
    let Some(ep) = resolve_active_endpoint(&cfg, ModelType::Embedding) else {
        return GatewayTest { kind: "embedding", ok: false, detail: "未配置".into() };
    };
    let url = format!("{}/embeddings", ep.base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "model": ep.model_id, "input": [""] });
    match ping_post(&url, &ep.api_key, &body, std::time::Duration::from_secs(5)) {
        Ok(()) => GatewayTest { kind: "embedding", ok: true, detail: "OK".into() },
        Err(e) => GatewayTest { kind: "embedding", ok: false, detail: e },
    }
}

fn test_llm() -> GatewayTest {
    let cfg = crate::config::load_config();
    let Some(ep) = resolve_active_endpoint(&cfg, ModelType::Llm) else {
        return GatewayTest { kind: "llm", ok: false, detail: "未配置".into() };
    };
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": ep.model_id,
        "messages": [{"role":"user","content":"ping"}],
        "max_tokens": 1
    });
    match ping_post(&url, &ep.api_key, &body, std::time::Duration::from_secs(30)) {
        Ok(()) => GatewayTest { kind: "llm", ok: true, detail: "OK".into() },
        Err(e) => GatewayTest { kind: "llm", ok: false, detail: e },
    }
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
    fn resolve_active_endpoint_valid_and_dangling() {
        use crate::config::{AppConfig, ModelConfig, ProviderConfig};
        let cfg = AppConfig {
            providers: vec![ProviderConfig {
                id: "p1".into(),
                name: "x".into(),
                base_url: "http://x/v1".into(),
                api_key: "k".into(),
                models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Embedding, enabled: false }],
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
            models: vec![ModelConfig { id: "m1".into(), model_type: ModelType::Llm, enabled: true }],
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
    fn gateways_unconfigured_report_not_ok() {
        // Environment-independent: the probe must complete without panicking,
        // and unconfigured gateways must report ok=false. If the host has
        // gateways configured, ok may be true.
        let tests = test_gateways();
        assert_eq!(tests.len(), 2);
        if !embedding_enabled() {
            assert!(!tests[0].ok);
        }
        if !llm_enabled() {
            assert!(!tests[1].ok);
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
}
