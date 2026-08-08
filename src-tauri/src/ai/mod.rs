//! OpenAI-compatible AI gateway (embeddings + chat) and vector helpers.
//!
//! The gateway is configured via `AppConfig` (`ai_api_base` etc.). An empty
//! `ai_api_base` disables the feature; every public entry point degrades
//! gracefully to `None`/empty when unconfigured or unreachable.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub fn ai_enabled() -> bool {
    !crate::config::load_config().ai_api_base.trim().is_empty()
}

pub fn embed_batch(texts: &[String]) -> Vec<Option<Vec<f32>>> {
    if texts.is_empty() || !ai_enabled() {
        return vec![None; texts.len()];
    }
    let cfg = crate::config::load_config();
    let url = format!("{}/embeddings", cfg.ai_api_base.trim_end_matches('/'));

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
        input: texts.to_vec(),
    };

    let req_body = serde_json::to_string(&body).unwrap_or_default();
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth()
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
    if !ai_enabled() {
        return None;
    }
    let cfg = crate::config::load_config();
    let url = format!("{}/chat/completions", cfg.ai_api_base.trim_end_matches('/'));

    #[derive(Serialize, Deserialize)]
    struct Msg {
        role: String,
        content: String,
    }
    #[derive(Serialize)]
    struct Req {
        model: String,
        messages: Vec<Msg>,
        temperature: f32,
        max_tokens: u32,
    }
    #[derive(Deserialize)]
    struct Resp {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Msg,
    }

    let req = Req {
        model: cfg.llm_model.clone(),
        messages: vec![
            Msg { role: "system".into(), content: system.into() },
            Msg { role: "user".into(), content: user.into() },
        ],
        temperature: 0.3,
        max_tokens: 1024,
    };
    let req_body = match serde_json::to_string(&req) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("[AI] chat request build failed: {e}");
            return None;
        }
    };
    let send_result = build_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set_auth()
        .send_string(&req_body);

    let parsed: Result<Resp, String> = send_result
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|body| serde_json::from_str::<Resp>(&body).map_err(|e| e.to_string()));

    match parsed {
        Ok(resp) => resp.choices.into_iter().next().map(|c| c.message.content),
        Err(e) => {
            log::warn!("[AI] chat request failed: {e}");
            None
        }
    }
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

trait AuthSetter {
    fn set_auth(self) -> Self;
}
impl AuthSetter for ureq::Request {
    fn set_auth(self) -> Self {
        let cfg = crate::config::load_config();
        let key = cfg.ai_api_key.trim();
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
        // No gateway configured (tests don't touch the user's config): embed
        // must return None rather than panic or hang.
        let r = embed_batch(&["hello".to_string()]);
        assert!(r.len() == 1 && r[0].is_none());
    }

    #[test]
    fn empty_batch_returns_same_len_none() {
        let r = embed_batch(&[]);
        assert!(r.is_empty());
    }

    #[test]
    fn chat_degrades_when_unconfigured() {
        // No gateway in tests: chat must not panic — returns None.
        assert!(chat("sys", "user").is_none());
    }
}
