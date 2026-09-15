//! Inbound adapters.
//!
//! Each adapter implements one native client surface on top of the canonical
//! request/response model, so a caller can point an OpenAI, Anthropic, Gemini,
//! or Codex (Responses API) client at AGOS Proxy and speak its own native
//! dialect.

pub mod anthropic;
pub mod google;
pub mod openai;
pub mod responses;

pub use anthropic::AnthropicAdapter;
pub use google::GoogleAdapter;
pub use openai::OpenAiAdapter;
pub use responses::{ResponsesAdapter, ResponsesRenderer};

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

/// A per-stream SSE renderer for a single response.
///
/// Most surfaces are stateless: every canonical event maps to exactly one
/// frame, which is what [`StatelessRenderer`] does. The Responses API is not
/// stateless — its terminal `response.output_item.done` items carry the
/// *accumulated* text and tool-call arguments — so it supplies a renderer that
/// owns that state.
///
/// A renderer is created once per stream attempt and dropped with it, so
/// accumulated state never leaks across requests or across failover retries.
pub trait StreamRenderer: Send {
    /// Render one canonical event as one or more complete SSE frames,
    /// including framing and trailing blank lines. `None` means the event
    /// produces no wire bytes for this protocol.
    fn render(&mut self, ev: &StreamEvent, id: &str) -> Option<String>;

    /// Called once after the upstream stream ends, so a protocol that requires
    /// a closing frame can still emit one even when the upstream never sent a
    /// terminal event. Returns `None` for surfaces that simply end.
    fn finish(&mut self, id: &str) -> Option<String>;
}

/// The default [`StreamRenderer`]: one frame per event, delegating straight to
/// the adapter's own [`InboundAdapter::render_stream_event`] and
/// [`InboundAdapter::stream_end_marker`].
pub struct StatelessRenderer(&'static dyn InboundAdapter);

impl StatelessRenderer {
    pub fn new(adapter: &'static dyn InboundAdapter) -> Self {
        Self(adapter)
    }
}

impl StreamRenderer for StatelessRenderer {
    fn render(&mut self, ev: &StreamEvent, id: &str) -> Option<String> {
        self.0.render_stream_event(ev, id)
    }

    fn finish(&mut self, _id: &str) -> Option<String> {
        self.0.stream_end_marker()
    }
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
            tool_calls: Vec::new(),
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
            ..Default::default()
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

    #[test]
    fn codex_round_trip() {
        let r = registry();
        let body = serde_json::json!({
            "model": "prog/route",
            "instructions": "be terse",
            "input": [{ "type": "message", "role": "user", "content": "write a function" }],
            "store": false,
            "reasoning": { "effort": "low" },
            "tools": [{ "type": "function", "name": "sh", "parameters": { "type": "object" } }],
        });
        let req = r.parse_request(ApiKind::Responses, &body).unwrap();
        assert_eq!(req.model, "prog/route");
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[0].content, "be terse");
        assert_eq!(req.messages[1].content, "write a function");
        assert_eq!(req.extra["tools"][0]["function"]["name"], "sh");
        // Responses-only fields are parked under the reserved key.
        assert_eq!(
            req.extra["agos_responses"]["store"],
            serde_json::Value::Bool(false)
        );

        let out = r.render_response(ApiKind::Responses, &canonical());
        assert_eq!(out["object"], "response");
        assert_eq!(out["output"][0]["content"][0]["text"], "hello world");
        assert_eq!(out["usage"]["total_tokens"], 12);
    }
}
