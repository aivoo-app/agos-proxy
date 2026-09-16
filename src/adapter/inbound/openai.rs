//! OpenAI inbound adapter.
//!
//! Serves `/openai/v1/chat/completions`. Because OpenAI's shape is the canonical
//! shape, parsing is a lossless deserialize; response rendering mirrors the
//! OpenAI chat completion and chat-completion-chunk formats.

use crate::adapter::inbound::InboundAdapter;
use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, StreamEvent};

/// OpenAI inbound surface.
pub struct OpenAiAdapter;

impl InboundAdapter for OpenAiAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::OpenAI
    }

    fn parse_request(&self, body: &serde_json::Value) -> anyhow::Result<ChatRequest> {
        Ok(serde_json::from_value(body.clone())?)
    }

    fn render_response(&self, resp: &CanonicalResponse) -> serde_json::Value {
        serde_json::json!({
            "id": resp.id,
            "object": "chat.completion",
            "model": resp.model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": resp.text },
                "finish_reason": resp.finish_reason,
            }],
            "usage": {
                "prompt_tokens": resp.prompt_tokens,
                "completion_tokens": resp.completion_tokens,
                "total_tokens": resp.total_tokens(),
            },
        })
    }

    fn render_stream_event(&self, ev: &StreamEvent, id: &str) -> Option<String> {
        let mut delta = serde_json::Map::new();
        if !ev.delta.is_empty() {
            delta.insert(
                "content".to_string(),
                serde_json::Value::String(ev.delta.clone()),
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
