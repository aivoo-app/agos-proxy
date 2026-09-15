//! Anthropic (Claude) inbound adapter.
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
/// into a single canonical text string.
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
                    anthropic_content_text(m.get("content").unwrap_or(&serde_json::Value::Null));
                messages.push(Message::text(role, content));
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
