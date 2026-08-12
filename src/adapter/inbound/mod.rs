//! Inbound adapters.
//!
//! Each adapter implements one native client surface on top of the canonical
//! request/response model, so a caller can point an OpenAI, Anthropic, or
//! Gemini SDK at AGOS Proxy and speak its own native dialect.

pub mod anthropic;
pub mod google;
pub mod openai;

pub use anthropic::AnthropicAdapter;
pub use google::GoogleAdapter;
pub use openai::OpenAiAdapter;

use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, StreamEvent};

/// An inbound adapter turns a native client's request into the canonical
/// [`ChatRequest`] and renders canonical results back in that client's native
/// wire format. Adapters are stateless and thus safe to share across requests.
pub trait InboundAdapter: Send + Sync {
    /// The native surface this adapter serves.
    fn kind(&self) -> ApiKind;

    /// Parse a native request body into the canonical request.
    fn parse_request(&self, body: &serde_json::Value) -> anyhow::Result<ChatRequest>;

    /// Render a non-streaming canonical response as a native JSON value.
    fn render_response(&self, resp: &CanonicalResponse) -> serde_json::Value;

    /// Render one canonical stream event as a complete native SSE frame
    /// (including framing and the trailing blank line). `None` means the event
    /// produces no wire bytes for this protocol.
    fn render_stream_event(&self, ev: &StreamEvent, id: &str) -> Option<String>;

    /// A terminal frame the protocol needs before the stream ends (e.g. OpenAI's
    /// `data: [DONE]`). `None` when the stream simply ends.
    fn stream_end_marker(&self) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Registry;
    use crate::translator::{CanonicalResponse, StreamEvent};

    fn registry() -> Registry {
        Registry::default()
    }

    fn canonical() -> CanonicalResponse {
        CanonicalResponse {
            id: "chatcmpl-1".into(),
            model: "prog/route".into(),
            text: "hello world".into(),
            finish_reason: "stop".into(),
            prompt_tokens: 5,
            completion_tokens: 7,
        }
    }

    #[test]
    fn openai_round_trip() {
        let r = registry();
        let body = serde_json::json!({
            "model": "prog/route",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": false,
        });
        let req = r.parse_request(ApiKind::OpenAI, &body).unwrap();
        assert_eq!(req.model, "prog/route");
        assert_eq!(req.messages[0].content, "hi");
        let out = r.render_response(ApiKind::OpenAI, &canonical());
        assert_eq!(out["choices"][0]["message"]["content"], "hello world");
        assert_eq!(out["usage"]["total_tokens"], 12);
    }

    #[test]
    fn anthropic_round_trip() {
        let r = registry();
        let body = serde_json::json!({
            "model": "prog/route",
            "system": "be terse",
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "hello" }] }
            ],
            "max_tokens": 64,
            "stop_sequences": ["END"],
        });
        let req = r.parse_request(ApiKind::Anthropic, &body).unwrap();
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.messages[1].content, "hello");
        assert_eq!(req.extra["stop"], serde_json::json!(["END"]));

        let out = r.render_response(ApiKind::Anthropic, &canonical());
        assert_eq!(out["type"], "message");
        assert_eq!(out["content"][0]["text"], "hello world");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 5);

        let ev = StreamEvent {
            delta: "hi".into(),
            finish_reason: None,
            prompt_tokens: None,
            completion_tokens: None,
            done: false,
        };
        let frame = r
            .render_stream_event(ApiKind::Anthropic, &ev, "id")
            .unwrap();
        assert!(frame.starts_with("event: content_block_delta\ndata: {"));
    }

    #[test]
    fn google_round_trip() {
        let r = registry();
        let body = serde_json::json!({
            "model": "prog/route",
            "systemInstruction": { "parts": [{ "text": "be terse" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "hello" }] }
            ],
            "generationConfig": { "maxOutputTokens": 64 },
        });
        let req = r.parse_request(ApiKind::Google, &body).unwrap();
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.messages[1].content, "hello");
        assert_eq!(req.extra["max_tokens"], 64);

        let out = r.render_response(ApiKind::Google, &canonical());
        assert_eq!(
            out["candidates"][0]["content"]["parts"][0]["text"],
            "hello world"
        );
        assert_eq!(out["candidates"][0]["finishReason"], "STOP");
        assert_eq!(out["usageMetadata"]["totalTokenCount"], 12);
    }
}
