//! OpenAI inbound adapter.
//!
//! Serves `/openai/v1/chat/completions`. Because OpenAI's shape is the canonical
//! shape, parsing is a lossless deserialize; response rendering mirrors the
//! OpenAI chat completion and chat-completion-chunk formats.

use crate::adapter::inbound::InboundAdapter;
use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, ContentPart, StreamEvent};

/// OpenAI inbound surface.
pub struct OpenAiAdapter;

impl InboundAdapter for OpenAiAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::OpenAI
    }

    fn parse_request(&self, body: &serde_json::Value) -> anyhow::Result<ChatRequest> {
        Ok(serde_json::from_value(body.clone())?)
    }

    fn render_response(&self, resp: &CanonicalResponse) -> anyhow::Result<serde_json::Value> {
        let mut message = serde_json::json!({ "role": "assistant", "content": resp.text });
        let mut content = Vec::new();
        if !resp.text.is_empty() {
            content.push(serde_json::json!({ "type": "text", "text": resp.text }));
        }
        let mut audio_written = false;
        for part in &resp.media {
            match part {
                ContentPart::Audio { url } => {
                    if let Some(data) = crate::translator::parse_data_url(url) {
                        if audio_written {
                            anyhow::bail!(
                                "OpenAI response can contain only one inline audio output"
                            );
                        }
                        message["audio"] = serde_json::json!({
                            "data": data.data,
                            "format": data.mime.trim_start_matches("audio/")
                        });
                        audio_written = true;
                    } else {
                        content.push(
                            serde_json::json!({ "type": "audio_url", "audio_url": { "url": url } }),
                        );
                    }
                }
                ContentPart::Image { .. }
                | ContentPart::Video { .. }
                | ContentPart::File { .. }
                | ContentPart::Text(_) => {
                    content.push(crate::translator::content_part_to_chat_json(part));
                }
            }
        }
        if !content.is_empty() {
            if content.len() == 1 && content[0]["type"] == "text" {
                message["content"] = content[0]["text"].clone();
            } else {
                message["content"] = serde_json::Value::Array(content);
            }
        }
        if !resp.tool_calls.is_empty() {
            message["tool_calls"] = serde_json::Value::Array(
                resp.tool_calls
                    .iter()
                    .map(|call| {
                        serde_json::json!({
                            "id": call.wire_id(), "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments }
                        })
                    })
                    .collect(),
            );
        }
        Ok(serde_json::json!({
            "id": resp.id,
            "object": "chat.completion",
            "model": resp.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": resp.finish_reason,
            }],
            "usage": {
                "prompt_tokens": resp.prompt_tokens,
                "completion_tokens": resp.completion_tokens,
                "total_tokens": resp.total_tokens(),
            },
        }))
    }

    fn render_stream_event(&self, ev: &StreamEvent, id: &str) -> Option<String> {
        let mut delta = serde_json::Map::new();
        if !ev.delta.is_empty() {
            delta.insert(
                "content".to_string(),
                serde_json::Value::String(ev.delta.clone()),
            );
        }
        if !ev.tool_call_deltas.is_empty() {
            delta.insert(
                "tool_calls".to_string(),
                serde_json::Value::Array(ev.tool_call_deltas.iter().map(|call| serde_json::json!({
                    "index": call.index,
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments_delta },
                })).collect()),
            );
        }
        let payload = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "model": "",
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": ev.finish_reason,
            }],
        });
        Some(format!("data: {}\n\n", payload))
    }

    fn stream_end_marker(&self) -> Option<String> {
        Some("data: [DONE]\n\n".to_string())
    }
}
