//! Anthropic inbound adapter.
//!
//! Serves `/anthropic/v1/messages`. Parses an Anthropic messages request into
//! the canonical [`ChatRequest`] and renders canonical results back as
//! Anthropic `Message` responses and SSE stream events.

use crate::adapter::inbound::InboundAdapter;
use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, Message, StreamEvent};

/// The Anthropic native inbound surface.
pub struct AnthropicAdapter;

/// Collapse an Anthropic content value (a plain string or an array of blocks)
/// into a single canonical text string. Text blocks contribute their `text`;
/// everything else is dropped. Used for system prompts, which stay text-only.
fn anthropic_content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Collapse an Anthropic content value into the canonical message content.
///
/// Content without image blocks stays a plain text string, so text-only
/// requests serialize byte-identically through the pipeline. Content carrying
/// image blocks becomes the OpenAI parts array — the canonical image encoding
/// the outbound adapters translate back into their native shape:
///
/// - `source.type = "url"` → an `image_url` part referencing the same URL;
/// - `source.type = "base64"` → an `image_url` part holding the payload
///   re-encoded as a `data:<media_type>;base64,<data>` URL.
fn anthropic_content(content: &serde_json::Value) -> serde_json::Value {
    let has_image = content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"))
    });
    if !has_image {
        return serde_json::Value::String(anthropic_content_text(content));
    }

    let parts: Vec<serde_json::Value> = content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                    Some("image") => anthropic_image_part(b),
                    _ => {
                        let text = b.get("text").and_then(|t| t.as_str()).unwrap_or_default();
                        if text.is_empty() {
                            None
                        } else {
                            Some(serde_json::json!({ "type": "text", "text": text }))
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::Value::Array(parts)
}

/// Convert an Anthropic `image` block into the canonical `image_url` part.
/// Blocks with a source this proxy cannot re-express (`file`-based Files API
/// references, for instance) are skipped — same as they were before the
/// passthrough existed.
fn anthropic_image_part(block: &serde_json::Value) -> Option<serde_json::Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(|t| t.as_str())? {
        "url" => source.get("url").and_then(|u| u.as_str())?.to_string(),
        // Re-encode the inline payload as a data URL, the one canonical image
        // reference every outbound adapter understands.
        "base64" => {
            let media = source
                .get("media_type")
                .and_then(|m| m.as_str())
                .unwrap_or("image/png");
            let data = source
                .get("data")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            format!("data:{media};base64,{data}")
        }
        _ => return None,
    };
    Some(serde_json::json!({
        "type": "image_url",
        "image_url": { "url": url },
    }))
}

/// Map an Anthropic finish reason onto our canonical finish reason.
pub fn canonical_stop_reason(reason: &str) -> &'static str {
    match reason {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
}

/// Map our canonical finish reason back onto an Anthropic stop reason.
pub fn anthropic_stop_reason(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    }
}

impl InboundAdapter for AnthropicAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::Anthropic
    }

    fn parse_request(&self, body: &serde_json::Value) -> anyhow::Result<ChatRequest> {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let stream = body
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);

        let mut messages: Vec<Message> = Vec::new();
        if let Some(sys) = body.get("system") {
            let text = anthropic_content_text(sys);
            if !text.is_empty() {
                messages.push(Message::text("system", text));
            }
        }
        if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
            for m in arr {
                let role = m
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("user")
                    .to_string();
                let content =
                    anthropic_content(m.get("content").unwrap_or(&serde_json::Value::Null));
                messages.push(Message::new(role, content));
            }
        }

        // Carry generation knobs forward after normalising native field names.
        let mut extra = body.clone();
        if let Some(obj) = extra.as_object_mut() {
            obj.remove("model");
            obj.remove("messages");
            obj.remove("system");
            obj.remove("stream");
            if let Some(v) = obj.remove("stop_sequences") {
                obj.insert("stop".to_string(), v);
            }
        }

        Ok(ChatRequest {
            model,
            messages,
            stream,
            extra,
        })
    }

    fn render_response(&self, resp: &CanonicalResponse) -> serde_json::Value {
        serde_json::json!({
            "id": resp.id,
            "type": "message",
            "role": "assistant",
            "model": resp.model,
            "content": [{ "type": "text", "text": resp.text }],
            "stop_reason": anthropic_stop_reason(&resp.finish_reason),
            "stop_sequence": null,
            "usage": {
                "input_tokens": resp.prompt_tokens,
                "output_tokens": resp.completion_tokens,
            },
        })
    }

    fn render_stream_event(&self, ev: &StreamEvent, _id: &str) -> Option<String> {
        if ev.done {
            return Some("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
        }
        if !ev.delta.is_empty() {
            let payload = serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": ev.delta },
            });
            return Some(format!("event: content_block_delta\ndata: {}\n\n", payload));
        }
        if let Some(reason) = &ev.finish_reason {
            let payload = serde_json::json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": anthropic_stop_reason(reason),
                    "stop_sequence": null,
                },
                "usage": { "output_tokens": ev.completion_tokens.unwrap_or(0) },
            });
            return Some(format!("event: message_delta\ndata: {}\n\n", payload));
        }
        None
    }

    fn stream_end_marker(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_blocks_become_canonical_image_url_parts() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this"},
                    {"type": "image", "source": {"type": "url", "url": "https://x/cat.png"}},
                ]
            }]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "https://x/cat.png"}}
            ])
        );
    }

    #[test]
    fn base64_image_blocks_become_data_urls() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {
                        "type": "base64", "media_type": "image/jpeg", "data": "QUJD"
                    }},
                    {"type": "text", "text": "describe"},
                ]
            }]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,QUJD"}},
                {"type": "text", "text": "describe"}
            ])
        );
    }

    #[test]
    fn text_only_content_stays_a_plain_string() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hi"}, {"type": "text", "text": "there"}]
            }]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(req.messages[0].content, serde_json::json!("hi\nthere"));
    }

    #[test]
    fn system_stays_text_only() {
        let body = serde_json::json!({
            "model": "m",
            "system": "be terse",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[0].content, serde_json::json!("be terse"));
    }
}
