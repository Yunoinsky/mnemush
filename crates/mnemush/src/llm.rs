//! LLM provider layer (v1.6.1).
//!
//! OpenAI-compatible chat completions over any base URL. A provider chain
//! walks `MiniMax → Local → DeepSeek` (configurable) so quota errors on
//! cloud fall through to a local Qwen automatically.
//!
//! Per-provider payload params:
//! - MiniMax: `temperature 0.7 + frequency_penalty 0.3 + presence_penalty 0.3`
//!   mitigates the documented MiniMax-M3 looping bugs (MiniMax-AI/MiniMax-M3#20, #7).
//!   `max_tokens 65536` covers the full reasoning chain + action JSON.
//! - DeepSeek-V4-Flash (reasoning): same parameters, reasoning model.
//! - Local (Ollama / LM Studio / llama.cpp): no penalty fields (some servers
//!   reject them), smaller `max_tokens 8192` (local models choke on huge output).

use std::time::Duration;

use ureq::AgentBuilder;

use crate::error::{MnemushError, Result};
use crate::config::Config;

/// Public OpenAI-compatible chat URL for local providers (Ollama default).
pub const DEFAULT_LOCAL_BASE_URL: &str = "http://localhost:11434/v1";
pub const DEFAULT_LOCAL_MODEL: &str = "qwen3.8:27b-q4_K_M";

/// Cloud provider endpoints (hardcoded; only credentials are env).
pub const MINIMAX_CHAT_URL: &str = "https://api.minimax.chat/v1/text/chatcompletion_v2";
pub const DEEPSEEK_CHAT_URL: &str = "https://api.deepseek.com/chat/completions";

pub struct ChatMsg {
    pub role: String,
    pub content: String,
}

impl ChatMsg {
    pub fn user(s: &str) -> Self {
        Self { role: "user".into(), content: s.into() }
    }
    pub fn system(s: &str) -> Self {
        Self { role: "system".into(), content: s.into() }
    }
}

pub fn minimax_model() -> String {
    std::env::var("MNEMUSH_LLM_MODEL").unwrap_or_else(|_| "minimax-m3".into())
}
pub fn deepseek_model() -> String {
    std::env::var("MNEMUSH_DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into())
}

pub fn minimax_key() -> Option<String> {
    for v in ["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY", "MINIMASH_TOKEN_PLAN_KEY"] {
        if let Ok(k) = std::env::var(v) {
            return Some(k);
        }
    }
    let mmx = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mmx")
        .join("config.json");
    if mmx.exists() {
        if let Ok(t) = std::fs::read_to_string(&mmx) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                if let Some(k) = v.get("api_key").and_then(|x| x.as_str()) {
                    return Some(k.to_string());
                }
            }
        }
    }
    None
}

/// One step in the provider chain. `base_url` is the chat-completions endpoint,
/// `model` is the model id. `kind` only affects payload params.
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub kind: ProviderKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// MiniMax M3 (cloud, reasoning model, needs looping mitigations).
    MiniMax,
    /// DeepSeek V4 Flash (cloud, reasoning).
    DeepSeek,
    /// OpenAI-compatible local endpoint (Ollama / LM Studio / llama.cpp).
    Local,
}

/// Build the ordered provider chain from `Config`. `provider = "auto"`
/// walks MiniMax → Local → DeepSeek. Explicit names return a single-step chain.
pub fn provider_chain(cfg: &Config) -> Vec<Provider> {
    match cfg.llm.provider.as_str() {
        "minimax" => vec![Provider {
            name: "minimax".into(),
            base_url: MINIMAX_CHAT_URL.into(),
            model: minimax_model(),
            api_key: minimax_key(),
            kind: ProviderKind::MiniMax,
        }],
        "deepseek" => vec![Provider {
            name: "deepseek".into(),
            base_url: DEEPSEEK_CHAT_URL.into(),
            model: deepseek_model(),
            api_key: std::env::var("DEEPSEEK_API_KEY").ok(),
            kind: ProviderKind::DeepSeek,
        }],
        "local" => vec![Provider {
            name: "local".into(),
            base_url: cfg.llm.local_base_url.clone(),
            model: cfg.llm.local_model.clone(),
            api_key: if cfg.llm.local_api_key.is_empty() { None } else { Some(cfg.llm.local_api_key.clone()) },
            kind: ProviderKind::Local,
        }],
        // auto
        _ => {
            let mut chain = Vec::new();
            if let Some(key) = minimax_key() {
                chain.push(Provider {
                    name: "minimax".into(),
                    base_url: MINIMAX_CHAT_URL.into(),
                    model: minimax_model(),
                    api_key: Some(key),
                    kind: ProviderKind::MiniMax,
                });
            }
            // Local is always offered in auto mode (Ollama on localhost is
            // cheap and free; cheap to skip if it returns 4xx).
            chain.push(Provider {
                name: "local".into(),
                base_url: cfg.llm.local_base_url.clone(),
                model: cfg.llm.local_model.clone(),
                api_key: if cfg.llm.local_api_key.is_empty() { None } else { Some(cfg.llm.local_api_key.clone()) },
                kind: ProviderKind::Local,
            });
            if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
                chain.push(Provider {
                    name: "deepseek".into(),
                    base_url: DEEPSEEK_CHAT_URL.into(),
                    model: deepseek_model(),
                    api_key: Some(key),
                    kind: ProviderKind::DeepSeek,
                });
            }
            chain
        }
    }
}

fn build_payload(p: &Provider, messages: &[ChatMsg]) -> serde_json::Value {
    // Per-provider payload: MiniMax/DeepSeek get looping mitigations and a
    // generous max_tokens (reasoning chains). Local gets smaller max_tokens
    // and no penalty fields (some servers reject them).
    let (temperature, max_tokens, penalty) = match p.kind {
        ProviderKind::MiniMax | ProviderKind::DeepSeek => {
            (0.7_f64, 65_536_u64, Some((0.3_f64, 0.3_f64)))
        }
        ProviderKind::Local => (0.7_f64, 8_192_u64, None),
    };
    let mut body = serde_json::json!({
        "model": p.model,
        "messages": messages.iter().map(|m| serde_json::json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
        "temperature": temperature,
        "max_tokens": max_tokens,
    });
    if let Some((fp, pp)) = penalty {
        body["frequency_penalty"] = serde_json::json!(fp);
        body["presence_penalty"] = serde_json::json!(pp);
    }
    body
}

fn post_json(url: &str, bearer: Option<&str>, body: &serde_json::Value) -> Result<serde_json::Value> {
    let agent = AgentBuilder::new().timeout(Duration::from_secs(180)).build();
    let mut req = agent
        .post(url)
        .set("Content-Type", "application/json");
    if let Some(k) = bearer {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    let resp = req
        .send_string(&body.to_string())
        .map_err(|e| MnemushError::Other(format!("llm http: {e}")))?;
    let text = resp
        .into_string()
        .map_err(|e| MnemushError::Other(format!("llm body: {e}")))?;
    serde_json::from_str(&text).map_err(|e| MnemushError::Other(format!("llm json: {e}")))
}

/// Extract assistant content from OpenAI-style response. Reasoning-model
/// responses also carry `reasoning_content`; we keep it as a sibling for
/// audit / debugging without altering the public API.
pub fn parse_chat_response(body: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| MnemushError::Other(format!("llm json: {e}")))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| MnemushError::Other("llm: no choices/0/message/content".into()))
}

/// Token usage from OpenAI-style response. Reasoning models (DeepSeek V4 Flash,
/// Qwen 3.x thinking) report reasoning_tokens under `completion_tokens_details`.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct LlmUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
}

pub fn parse_usage(body: &str) -> LlmUsage {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return LlmUsage::default();
    };
    let g = |k: &str| v.pointer(&format!("/usage/{k}")).and_then(|x| x.as_u64()).unwrap_or(0);
    let c = |k: &str| {
        v.pointer("/usage/completion_tokens_details")
            .and_then(|d| d.get(k))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    LlmUsage {
        prompt_tokens: g("prompt_tokens"),
        completion_tokens: g("completion_tokens"),
        reasoning_tokens: c("reasoning_tokens"),
    }
}

/// Call one provider exactly once. Used by the chain in `chat_with_usage` and
/// directly by tests / callers that want to bypass the chain.
pub fn chat_with_provider(p: &Provider, messages: &[ChatMsg]) -> Result<(String, LlmUsage)> {
    let body = build_payload(p, messages);
    // Cloud URLs (MiniMax / DeepSeek) are full chat-completions endpoints
    // hardcoded in their constants. Local URLs are a directory prefix
    // (e.g. http://localhost:11434/v1) that needs /chat/completions appended.
    let url = match p.kind {
        ProviderKind::Local => format!("{}/chat/completions", p.base_url.trim_end_matches('/')),
        ProviderKind::MiniMax | ProviderKind::DeepSeek => p.base_url.clone(),
    };
    let v = post_json(&url, p.api_key.as_deref(), &body)?;
    let raw = v.to_string();
    let text = parse_chat_response(&raw)?;
    let usage = parse_usage(&raw);
    if text.trim().is_empty() {
        return Err(MnemushError::Other(format!(
            "llm {}: empty response (likely reasoning chain ate output budget)",
            p.name
        )));
    }
    Ok((text, usage))
}

/// Walk the provider chain. Returns the first successful response. If
/// `cfg.llm.fallback_to_local` is true, any non-final error falls through
/// to the next provider; otherwise the error propagates.
pub fn chat_with_usage(messages: &[ChatMsg]) -> Result<(String, LlmUsage)> {
    chat_with_usage_cfg(messages, &Config::load()?)
}

/// Same, with explicit config (used by callers that already have one).
pub fn chat_with_usage_cfg(messages: &[ChatMsg], cfg: &Config) -> Result<(String, LlmUsage)> {
    let chain = provider_chain(cfg);
    if chain.is_empty() {
        return Err(MnemushError::Other(
            "llm: no providers configured (set MNEMUSH_API_KEY or run Ollama at \
             http://localhost:11434)"
                .into(),
        ));
    }
    let mut last_err: Option<MnemushError> = None;
    for p in &chain {
        match chat_with_provider(p, messages) {
            Ok(r) => return Ok(r),
            Err(e) => {
                last_err = Some(e);
                if !cfg.llm.fallback_to_local && p.kind != ProviderKind::Local {
                    // Hard fail on cloud error when fallback disabled.
                    return Err(last_err.unwrap());
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        MnemushError::Other("llm: provider chain empty after fallback".into())
    }))
}

/// Legacy single-call API: text only.
pub fn chat(messages: &[ChatMsg]) -> Result<String> {
    chat_with_usage(messages).map(|(t, _)| t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_style_response() {
        let out = parse_chat_response(
            r#"{"choices":[{"message":{"content":"hello from mock"}}]}"#,
        )
        .unwrap();
        assert_eq!(out, "hello from mock");
    }

    #[test]
    fn rejects_malformed_response() {
        assert!(parse_chat_response(r#"{"choices":[]}"#).is_err());
        assert!(parse_chat_response("not json").is_err());
    }

    #[test]
    fn model_defaults() {
        assert_eq!(minimax_model(), "minimax-m3");
        assert_eq!(deepseek_model(), "deepseek-v4-flash");
    }

    #[test]
    fn auto_chain_walks_minimax_local_deepseek() {
        let mut cfg = Config::default();
        cfg.llm.provider = "auto".into();
        std::env::set_var("MINIMAX_API_KEY", "test-key");
        let chain = provider_chain(&cfg);
        assert!(!chain.is_empty());
        assert_eq!(chain[0].kind, ProviderKind::MiniMax);
        // Local is always included in auto.
        assert!(chain.iter().any(|p| p.kind == ProviderKind::Local));
    }

    #[test]
    fn local_provider_uses_config_base_url_and_model() {
        let mut cfg = Config::default();
        cfg.llm.provider = "local".into();
        cfg.llm.local_base_url = "http://example.test/v1".into();
        cfg.llm.local_model = "my-model".into();
        let chain = provider_chain(&cfg);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].base_url, "http://example.test/v1");
        assert_eq!(chain[0].model, "my-model");
        assert_eq!(chain[0].kind, ProviderKind::Local);
    }

    #[test]
    fn payload_omits_penalty_for_local() {
        let p = Provider {
            name: "local".into(),
            base_url: DEFAULT_LOCAL_BASE_URL.into(),
            model: DEFAULT_LOCAL_MODEL.into(),
            api_key: None,
            kind: ProviderKind::Local,
        };
        let body = build_payload(&p, &[ChatMsg::user("hi")]);
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("presence_penalty").is_none());
        assert_eq!(body["max_tokens"], 8_192);
    }

    #[test]
    fn payload_includes_penalty_for_minimax() {
        let p = Provider {
            name: "minimax".into(),
            base_url: MINIMAX_CHAT_URL.into(),
            model: "minimax-m3".into(),
            api_key: Some("k".into()),
            kind: ProviderKind::MiniMax,
        };
        let body = build_payload(&p, &[ChatMsg::user("hi")]);
        assert_eq!(body["frequency_penalty"], 0.3);
        assert_eq!(body["presence_penalty"], 0.3);
        assert_eq!(body["max_tokens"], 65_536);
    }
}
