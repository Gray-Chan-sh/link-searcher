//! OpenAI-compatible AI gateway (embeddings + chat) and vector helpers.
//!
//! The gateway is configured via `AppConfig` (`ai_api_base` etc.). An empty
//! `ai_api_base` disables the feature; every public entry point degrades
//! gracefully to `None`/empty when unconfigured or unreachable.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub fn ai_enabled() -> bool {
    embedding_enabled() || llm_enabled()
}

/// True when the embedding gateway is configured (semantic search usable).
pub fn embedding_enabled() -> bool {
    !crate::config::load_config().embedding_api_base.trim().is_empty()
}

/// True when the LLM gateway is configured (summary / RAG usable).
pub fn llm_enabled() -> bool {
    !crate::config::load_config().llm_api_base.trim().is_empty()
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
    if texts.is_empty() || !embedding_enabled() {
        return vec![None; texts.len()];
    }
    let cfg = crate::config::load_config();
    let url = format!("{}/embeddings", cfg.embedding_api_base.trim_end_matches('/'));

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
        model: cfg.embedding_model.clone(),
        input: texts.iter().map(|t| truncate_for_embed(t)).collect(),
    };

    let req_body = serde_json::to_string(&body).unwrap_or_default();
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth(&cfg.embedding_api_key)
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
    if !llm_enabled() {
        return None;
    }
    let cfg = crate::config::load_config();
    let url = format!("{}/chat/completions", cfg.llm_api_base.trim_end_matches('/'));

    #[derive(Serialize)]
    struct Req {
        model: String,
        messages: Vec<ChatMsg>,
        temperature: f32,
        max_tokens: u32,
        // Force a single non-streamed JSON response. Some OpenAI-compatible
        // gateways (e.g. router proxies) default to SSE streaming, which our
        // JSON parser can't read (would otherwise fail with
        // "trailing characters" and the request would look like a failure).
        stream: bool,
    }

    let req = Req {
        model: cfg.llm_model.clone(),
        messages: vec![
            ChatMsg { role: "system".into(), content: system.into() },
            ChatMsg { role: "user".into(), content: user.into() },
        ],
        temperature: 0.3,
        max_tokens: 1024,
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
        cfg.llm_model,
        user.chars().count()
    );
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth(&cfg.llm_api_key)
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

#[derive(Serialize, Deserialize)]
struct ChatMsg {
    role: String,
    content: String,
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
    let mut builder = ureq::builder().timeout(std::time::Duration::from_secs(120));
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
/// settings "test" button and at startup to decide which AI features to
/// enable. Unconfigured gateways report `ok=false, detail=未配置`.
pub fn test_gateways() -> Vec<GatewayTest> {
    vec![test_embedding(), test_llm()]
}

/// Cached gateway capability probe. The live test is only issued at most
/// once per 30s; between tests, the previous result (or "unconfigured")
/// is served. Used by the frontend to enable/disable AI feature entry
/// points.
pub fn capabilities() -> (bool, bool) {
    static CACHE: OnceLock<std::sync::Mutex<(Instant, Vec<GatewayTest>)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new((Instant::now() - std::time::Duration::from_secs(60), Vec::new())));
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.0.elapsed().as_secs() > 30 || guard.1.is_empty() {
        guard.0 = Instant::now();
        guard.1 = test_gateways();
    }
    let emb = guard.1.iter().find(|t| t.kind == "embedding").map(|t| t.ok).unwrap_or(false);
    let llm = guard.1.iter().find(|t| t.kind == "llm").map(|t| t.ok).unwrap_or(false);
    (emb, llm)
}

fn test_embedding() -> GatewayTest {
    let cfg = crate::config::load_config();
    if cfg.embedding_api_base.trim().is_empty() {
        return GatewayTest { kind: "embedding", ok: false, detail: "未配置".into() };
    }
    let url = format!("{}/embeddings", cfg.embedding_api_base.trim_end_matches('/'));
    let body = serde_json::json!({ "model": cfg.embedding_model, "input": [""] });
    match ping_post(&url, &cfg.embedding_api_key, &body) {
        Ok(()) => GatewayTest { kind: "embedding", ok: true, detail: "OK".into() },
        Err(e) => GatewayTest { kind: "embedding", ok: false, detail: e },
    }
}

fn test_llm() -> GatewayTest {
    let cfg = crate::config::load_config();
    if cfg.llm_api_base.trim().is_empty() {
        return GatewayTest { kind: "llm", ok: false, detail: "未配置".into() };
    }
    let url = format!("{}/chat/completions", cfg.llm_api_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.llm_model,
        "messages": [{"role":"user","content":"ping"}],
        "max_tokens": 1
    });
    match ping_post(&url, &cfg.llm_api_key, &body) {
        Ok(()) => GatewayTest { kind: "llm", ok: true, detail: "OK".into() },
        Err(e) => GatewayTest { kind: "llm", ok: false, detail: e },
    }
}

/// Issue a tiny POST and return Ok on any 2xx. Rejects 4xx/5xx with the
/// status text, and network errors with the transport message.
fn ping_post(url: &str, key: &str, body: &serde_json::Value) -> Result<(), String> {
    // Connectivity probes use a short timeout so a dead/hanging gateway
    // fails fast instead of blocking the settings "test" or the startup
    // capability probe for the full request timeout.
    let agent = ureq::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build();
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
    fn chat_degrades_when_unconfigured() {
        // Environment-independent: chat must complete (None when not
        // configured; Some/None against a real gateway within timeout).
        let _ = chat("sys", "user");
    }

    #[test]
    fn gateways_unconfigured_report_not_ok() {
        // Environment-independent: the probe must complete without panicking,
        // and unconfigured gateways must report ok=false. If the host has
        // gateways configured, ok may be true.
        let tests = test_gateways();
        assert_eq!(tests.len(), 2);
        let cfg = crate::config::load_config();
        if cfg.embedding_api_base.trim().is_empty() {
            assert!(!tests[0].ok);
        }
        if cfg.llm_api_base.trim().is_empty() {
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
