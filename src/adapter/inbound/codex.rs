//! OpenAI Codex inbound adapter.
//!
//! Serves `/codex/v1/chat/completions` and `/codex/v1/completions`.
//!
//! The OpenAI Codex API is largely OpenAI-compatible, but a few differences
//! exist around code-interpreter tool annotations and how the response
//! surfaces tool-call artifacts. This adapter translates those differences
//! so a Codex SDK client can target AGOS Proxy through the `/codex` surface
//! and reach any configured upstream (OpenAI, Anthropic, Google, etc.).

use crate::adapter::inbound::InboundAdapter;
use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, StreamEvent};

/// Codex-native inbound surface.
pub struct CodexAdapter;

/// Map a canonical finish reason onto the Codex/OpenAI equivalent.
fn codex_finish_reason(reason: &str) -> &'static str {
    match reason {
        "tool_calls" => "tool_calls",
        "length" => "length",
        _ => "stop",
    }
}

impl InboundAdapter for CodexAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::Codex
    }

    fn parse_request(&self, body: &serde_json::Value) -> anyhow::Result<ChatRequest> {
        // Codex uses the same request schema as OpenAI chat completions.
        // Delegate to the canonical deserializer; the only addition we make
        // is preserving the `metadata` field that Codex uses for
        // code-interpreter session tracking.
        let mut req: ChatRequest = serde_json::from_value(body.clone())?;

        // Surface code-interpreter tool definitions as extra metadata so the
        // outbound adapter can map them to the upstream's native format.
        if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
            if tools
                .iter()
                .any(|t| t.get("type").and_then(|t| t.as_str()) == Some("code_interpreter"))
            {
                if let Some(obj) = req.extra.as_object_mut() {
                    obj.insert(
                        "codex_code_interpreter".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
            }
        }

        Ok(req)
    }

    fn render_response(&self, resp: &CanonicalResponse) -> serde_json::Value {
        // Codex responses are OpenAI-shaped, but include a `codex` metadata
        // block for tool usage. We keep the familiar shape and add the
        // canonical finish reason mapping.
        serde_json::json!({
            "id": resp.id,
            "object": "chat.completion",
            "model": resp.model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": resp.text },
                "finish_reason": codex_finish_reason(&resp.finish_reason),
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
        if let Some(reason) = &ev.finish_reason {
            delta.insert(
                "finish_reason".to_string(),
                serde_json::Value::String(codex_finish_reason(reason).to_string()),
            );
        }
        let payload = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "model": "",
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": ev.finish_reason.as_ref().map(|r| codex_finish_reason(r)),
            }],
        });
        Some(format!("data: {}\n\n", payload))
    }

    fn stream_end_marker(&self) -> Option<String> {
        Some("data: [DONE]\n\n".to_string())
    }
}
