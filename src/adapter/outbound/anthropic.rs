//! Anthropic native request/response translation.
//!
//! Translates between the OpenAI chat-completions format and
//! Anthropic's `/v1/messages` API. Reference:
//! https://docs.anthropic.com/en/api/messages

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use super::normalize_base;
use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{content_parts, content_text, parse_data_url, ChatRequest, ContentPart};

/// Build the upstream URL for an Anthropic request.
pub fn build_url(target: &Target) -> String {
    let base = normalize_base(&target.provider.base_url);
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

/// Translate an OpenAI chat request into an Anthropic messages request.
pub fn translate_request(chat_req: &ChatRequest, model_id: &str) -> serde_json::Value {
    let mut system_text = String::new();
    let mut messages = Vec::new();
    for msg in &chat_req.messages {
        if msg.role == "system" {
            if !system_text.is_empty() {
                system_text.push('\n');
            }
            // Anthropic system prompts are text-only; image parts there are
            // dropped (see [`message_content`] for the message-level mapping).
            system_text.push_str(&content_text(&msg.content));
        } else {
            messages.push(serde_json::json!({
                "role": msg.role,
                "content": message_content(&msg.content),
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

/// Rebuild an Anthropic message `content` value from the canonical content.
///
/// Text-only content stays a plain JSON string, so a text-only request
/// serializes byte-identically to the pre-vision behavior. Content carrying
/// image parts becomes an ordered block array: text parts map to `text`
/// blocks and image parts map to `image` blocks — `source.type = "url"` for
/// `http(s)://` references and `source.type = "base64"` for `data:` URLs.
fn message_content(content: &serde_json::Value) -> serde_json::Value {
    let has_image = content_parts(content)
        .iter()
        .any(|p| matches!(p, ContentPart::Image { .. }));
    if !has_image {
        return serde_json::Value::String(content_text(content));
    }

    let blocks: Vec<serde_json::Value> = content_parts(content)
        .into_iter()
        .map(|p| match p {
            ContentPart::Text(text) => serde_json::json!({ "type": "text", "text": text }),
            ContentPart::Image { url } => image_block(&url),
        })
        .collect();
    serde_json::Value::Array(blocks)
}

/// Map one canonical image onto an Anthropic `image` block. A `data:` URL is
/// split into its MIME type and base64 payload; anything else is passed as a
/// URL reference for the provider to resolve (or reject — the failure
/// classifies the entry and fails over, see `demote_status_for`).
fn image_block(url: &str) -> serde_json::Value {
    if let Some(data) = parse_data_url(url) {
        return serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": data.mime,
                "data": data.data,
            },
        });
    }
    serde_json::json!({
        "type": "image",
        "source": { "type": "url", "url": url },
    })
}

/// Translate an Anthropic messages response back into OpenAI format.
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
        "model": resp.get("model").cloned().unwrap_or(serde_json::Value::Null),
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

/// Decode one Anthropic SSE `data:` payload into a [`StreamEvent`]. Handles
/// `content_block_delta` (text), `message_delta` (finish reason + usage) and
/// `message_stop` events; all other bookkeeping events yield `None`.
pub fn parse_stream_chunk(data: &str) -> Option<crate::translator::StreamEvent> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    match kind {
        "content_block_delta" => {
            let delta = v
                .pointer("/delta/text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            Some(crate::translator::StreamEvent {
                delta,
                finish_reason: None,
                prompt_tokens: None,
                completion_tokens: None,
                done: false,
                ..Default::default()
            })
        }
        "message_start" => {
            // The input token count arrives once, on the opening event.
            Some(crate::translator::StreamEvent {
                prompt_tokens: v
                    .pointer("/message/usage/input_tokens")
                    .and_then(|t| t.as_u64()),
                ..Default::default()
            })
        }
        "message_delta" => {
            let stop_reason = v
                .pointer("/delta/stop_reason")
                .and_then(|t| t.as_str())
                .map(|r| match r {
                    "max_tokens" => "length".to_string(),
                    "tool_use" => "tool_calls".to_string(),
                    _ => "stop".to_string(),
                });
            let completion = v.pointer("/usage/output_tokens").and_then(|t| t.as_u64());
            Some(crate::translator::StreamEvent {
                delta: String::new(),
                finish_reason: stop_reason,
                prompt_tokens: None,
                completion_tokens: completion,
                done: false,
                ..Default::default()
            })
        }
        "message_stop" => Some(crate::translator::StreamEvent {
            delta: String::new(),
            finish_reason: Some("stop".to_string()),
            prompt_tokens: None,
            completion_tokens: None,
            done: true,
            ..Default::default()
        }),
        _ => None,
    }
}

pub fn is_supported(_kind: ProviderKind) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::Message;

    fn chat(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "prog/route".into(),
            messages,
            stream: false,
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn text_only_request_keeps_string_content() {
        // No image parts: content must remain a plain JSON string, exactly as
        // before the vision passthrough existed.
        let body = translate_request(
            &chat(vec![
                Message::text("system", "be terse"),
                Message::text("user", "hi"),
            ]),
            "model-id",
        );
        assert_eq!(body["system"], "be terse");
        assert!(body["messages"][0]["content"].is_string());
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn image_url_parts_map_to_anthropic_blocks() {
        let body = translate_request(
            &chat(vec![Message::new(
                "user",
                serde_json::json!([
                    {"type": "text", "text": "what is this"},
                    {"type": "image_url", "image_url": {"url": "https://x/cat.png"}},
                ]),
            )]),
            "model-id",
        );
        let content = &body["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is this");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "url");
        assert_eq!(content[1]["source"]["url"], "https://x/cat.png");
    }

    #[test]
    fn data_url_image_maps_to_a_base64_source_block() {
        let body = translate_request(
            &chat(vec![Message::new(
                "user",
                serde_json::json!([
                    {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,QUJD"}},
                    {"type": "text", "text": "describe"},
                ]),
            )]),
            "model-id",
        );
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[0]["source"]["data"], "QUJD");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "describe");
    }
}

#[cfg(test)]
mod stream_tests {
    use super::parse_stream_chunk;

    #[test]
    fn decodes_content_block_delta() {
        let ev = parse_stream_chunk(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap();
        assert_eq!(ev.delta, "Hello");
        assert!(!ev.done);
        assert!(ev.finish_reason.is_none());
    }

    #[test]
    fn decodes_message_stop_as_terminal() {
        let ev = parse_stream_chunk(r#"{"type":"message_stop"}"#).unwrap();
        assert!(ev.done);
        assert_eq!(ev.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn maps_max_tokens_finish_to_length() {
        let ev = parse_stream_chunk(
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":300}}"#,
        )
        .unwrap();
        assert_eq!(ev.finish_reason.as_deref(), Some("length"));
        assert_eq!(ev.completion_tokens, Some(300));
    }

    #[test]
    fn ignores_bookkeeping_events() {
        assert!(parse_stream_chunk(r#"{"type":"ping"}"#).is_none());
        assert!(parse_stream_chunk("").is_none());
    }
}
