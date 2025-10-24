//! Anthropic (Claude) native request/response translation.
//!
//! Translates between the OpenAI-compatible chat-completions format and
//! Anthropic's `/v1/messages` API. Reference:
//! https://docs.anthropic.com/en/api/messages

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{ChatRequest, Message};

/// Build the upstream URL for an Anthropic request.
pub fn build_url(target: &Target) -> String {
    let base = target.provider.base_url.trim_end_matches('/');
    format!("{base}/v1/messages")
}

/// Build the header map for an Anthropic request.
pub fn build_headers(target: &Target) -> BTreeMap<String, String> {
    let mut headers = target.provider.extra_headers.clone();
    headers.insert("x-api-key".to_string(), target.provider.auth_token.clone());
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers
        .entry("anthropic-version".to_string())
        .or_insert_with(|| "2023-06-01".to_string());
    headers
}

/// Translate an OpenAI-compatible chat request into an Anthropic messages request.
pub fn translate_request(chat_req: &ChatRequest, model_id: &str) -> serde_json::Value {
    let mut system_text = String::new();
    let mut messages = Vec::new();
    for msg in &chat_req.messages {
        if msg.role == "system" {
            if !system_text.is_empty() {
                system_text.push('\n');
            }
            system_text.push_str(&msg.content);
        } else {
            messages.push(serde_json::json!({
                "role": msg.role,
                "content": msg.content,
            }));
        }
    }

    let mut body = serde_json::json!({
        "model": model_id,
        "max_tokens": 4096,
        "messages": messages,
    });

    if !system_text.is_empty() {
        body["system"] = serde_json::Value::String(system_text);
    }

    if let Some(obj) = chat_req.extra.as_object() {
        if let Some(v) = obj.get("max_tokens") {
            body["max_tokens"] = v.clone();
        }
        if let Some(v) = obj.get("temperature") {
            body["temperature"] = v.clone();
        }
        if let Some(v) = obj.get("top_p") {
            body["top_p"] = v.clone();
        }
        if let Some(v) = obj.get("stop") {
            body["stop_sequences"] = v.clone();
        }
    }

    if chat_req.stream {
        body["stream"] = serde_json::Value::Bool(true);
    }

    body
}

/// Translate an Anthropic messages response back into OpenAI-compatible format.
pub fn translate_response(resp: &serde_json::Value) -> Result<serde_json::Value> {
    let content = resp
        .get("content")
        .and_then(|c| c.as_array())
        .context("anthropic response missing content array")?;

    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
            }
        }
    }

    let usage = resp.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let stop_reason = resp
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop");
    let finish_reason = match stop_reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "stop_sequence" => "stop",
        "tool_use" => "tool_calls",
        _ => "stop",
    };

    Ok(serde_json::json!({
        "id": resp.get("id").cloned().unwrap_or_else(|| serde_json::Value::String("msg_agos".into())),
        "object": "chat.completion",
        "model": resp.get("model").cloned().unwrap_or_else(|| serde_json::Value::Null),
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text,
            },
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    }))
}

pub fn is_supported(_kind: ProviderKind) -> bool {
    true
}