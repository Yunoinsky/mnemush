//! llm —— 聊天客户端(MiniMax M3 主, DeepSeek V4 Flash fallback)。
//!
//! 供 consolidate(记忆巩固)等 LLM 驱动功能使用。180s 超时
//! (dream 采样候选 ≤90 条 + 推理模型耗时高),
//! MiniMax 失败自动 fallback DeepSeek;两者都失败 → 报错。

use std::time::Duration;

use ureq::AgentBuilder;

use crate::error::Result;

pub const MINIMAX_CHAT_URL: &str = "https://api.minimax.chat/v1/text/chatcompletion_v2";
pub const DEEPSEEK_CHAT_URL: &str = "https://api.deepseek.com/chat/completions";

pub struct ChatMsg {
    pub role: String,
    pub content: String,
}

impl ChatMsg {
    pub fn user(s: &str) -> Self {
        Self {
            role: "user".into(),
            content: s.into(),
        }
    }
    pub fn system(s: &str) -> Self {
        Self {
            role: "system".into(),
            content: s.into(),
        }
    }
}

pub fn minimax_model() -> String {
    std::env::var("MNEMUSH_LLM_MODEL").unwrap_or_else(|_| "minimax-m3".into())
}

pub fn deepseek_model() -> String {
    // deepseek-v4-flash 是推理模型(reasoning_content + content);
    // max_tokens 需覆盖推理链 + 输出, 否则 content 为空。
    std::env::var("MNEMUSH_DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into())
}

fn minimax_key() -> Option<String> {
    // 官方 cn 环境变量名(MINIMAX_CN_API_KEY / MINIMAX_TOKEN_PLAN_KEY)
    for v in [
        "MINIMAX_API_KEY",
        "MINIMAX_CN_API_KEY",
        "MINIMAX_TOKEN_PLAN_KEY",
    ] {
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

fn post_json(url: &str, bearer: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    let agent = AgentBuilder::new()
        .timeout(Duration::from_secs(180))
        .build();
    let resp = agent
        .post(url)
        .set("Authorization", &format!("Bearer {bearer}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| crate::error::MnemushError::Other(format!("llm http: {e}")))?;
    let text = resp
        .into_string()
        .map_err(|e| crate::error::MnemushError::Other(format!("llm body: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| crate::error::MnemushError::Other(format!("llm json: {e}")))
}

/// 从 OpenAI 风格响应提取助手文本。
pub fn parse_chat_response(body: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| crate::error::MnemushError::Other(format!("llm json: {e}")))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            crate::error::MnemushError::Other("llm: no choices/0/message/content".into())
        })
}

/// LLM 调用用量(prompt/completion/reasoning tokens)。
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct LlmUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
}

/// 从 OpenAI 风格响应提取 usage(缺失时返回默认)。
pub fn parse_usage(body: &str) -> LlmUsage {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return LlmUsage::default();
    };
    let g = |k: &str| {
        v.pointer(&format!("/usage/{k}"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
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

/// 聊天调用: MiniMax 优先, fallback DeepSeek。返回 (文本, 用量)。
pub fn chat_with_usage(messages: &[ChatMsg]) -> Result<(String, LlmUsage)> {
    let payload = |model: &str| {
        serde_json::json!({
            "model": model,
            "messages": messages.iter().map(|m| serde_json::json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
            // max_tokens 覆盖推理链 + 动作输出; dream 采样 ≤90 条候选时
            // 推理(v4-flash reasoning_content)耗 token 多, 16000 会截断
            "max_tokens": 65536,
            // MiniMax M3 官方确认的重复/循环问题缓解: 低温度 + 重复惩罚
            // (MiniMax-AI/MiniMax-M3#20 ALLALLALL 循环; #7 agent loops)
            "temperature": 0.7,
            "frequency_penalty": 0.3,
            "presence_penalty": 0.3,
        })
    };
    // 1) MiniMax
    if let Some(key) = minimax_key() {
        if let Ok(v) = post_json(MINIMAX_CHAT_URL, &key, &payload(&minimax_model())) {
            if let Ok(text) = parse_chat_response(&v.to_string()) {
                if !text.trim().is_empty() {
                    return Ok((text, parse_usage(&v.to_string())));
                }
            }
        }
    }
    // 2) DeepSeek fallback
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        let v = post_json(DEEPSEEK_CHAT_URL, &key, &payload(&deepseek_model()))?;
        let usage = parse_usage(&v.to_string());
        return Ok((parse_chat_response(&v.to_string())?, usage));
    }
    Err(crate::error::MnemushError::Other(
        "llm: no usable key (MINIMAX_API_KEY / DEEPSEEK_API_KEY)".into(),
    ))
}

/// 兼容旧调用方: 只返回文本。
pub fn chat(messages: &[ChatMsg]) -> Result<String> {
    chat_with_usage(messages).map(|(text, _)| text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_style_response() {
        let out = parse_chat_response(r#"{"choices":[{"message":{"content":"hello from mock"}}]}"#)
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
}
