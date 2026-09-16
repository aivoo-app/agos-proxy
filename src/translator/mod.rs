//! Canonical request/response model.
//!
//! This module defines the provider-independent data types every adapter
//! speaks: [`ChatRequest`] on the way in, [`CanonicalResponse`] and
//! [`StreamEvent`] on the way back, plus the OpenAI
//! completions/embeddings payload types used by the passthrough endpoints.
//!
//! Tool calling is part of the canonical model: [`CanonicalResponse::tool_calls`]
//! and [`StreamEvent::tool_call_deltas`] carry function calls in wire form, and
//! [`Message::extra`] keeps per-message fields such as `tool_calls`,
//! `tool_call_id` and `name` intact across a round trip. Agentic clients (the
//! Codex CLI in particular) depend on this: without it the tool call would be
//! silently dropped between the caller and the upstream provider.
//!
//! It contains no provider logic — see [`crate::adapter::inbound`] for the
//! per-surface inbound adapters and [`crate::adapter::outbound`] for the
//! per-provider outbound adapters built on top of these types.

/// An OpenAI chat completions request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    /// The message content, kept as raw JSON so the OpenAI
    /// passthrough stays lossless. Plain text is a JSON string; multimodal
    /// requests use the OpenAI parts array (text / image_url entries).
    pub content: serde_json::Value,
    /// Every other per-message field the surface or provider carries, kept as
    /// raw JSON with `flatten` so nothing is dropped on the way through. This
    /// is what keeps `tool_calls` (assistant messages), `tool_call_id` and
    /// `name` (tool-result messages), and provider-specific extras such as
    /// `reasoning_content` or `cache_control` intact across a round trip.
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl Message {
    /// Build a message with no extra fields.
    pub fn new(role: impl Into<String>, content: serde_json::Value) -> Self {
        Self {
            role: role.into(),
            content,
            extra: serde_json::Value::Null,
        }
    }

    /// Build a message whose content is a plain string.
    pub fn text(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(role, serde_json::Value::String(text.into()))
    }
}

/// Extract the plain-text portion of a message content value. Multimodal
/// (array) content contributes its `text` parts; image parts are dropped.
pub fn content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A provider-independent single-turn response, produced by the outbound
/// adapters and consumed by [`crate::adapter::inbound`] inbound adapters so
/// each can render its own native response shape. This is the seam that lets a
/// single outbound result feed an OpenAI, Anthropic, or Google client
/// unchanged.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub text: String,
    pub finish_reason: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Tool calls the provider returned alongside (or instead of) text. An
    /// assistant turn that only calls tools carries an empty `text`.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

impl CanonicalResponse {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// A tool/function call produced by a provider, kept in wire form.
///
/// The OpenAI chat-completions wire format carries a single `id` per call and
/// matches tool results against it via `tool_call_id`. The Responses API
/// separates the item id (`id`, e.g. `fc_...`) from the correlation id
/// (`call_id`, e.g. `call_...`). Both are kept here so either surface can be
/// rendered without losing information; adapters that only have one value set
/// both fields to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    /// Responses-API item id (`fc_...`). Falls back to `call_id` in the
    /// OpenAI chat-completions encoding, where the two are the same value.
    pub id: String,
    /// The id the client must echo back in the tool result.
    pub call_id: String,
    pub name: String,
    /// Raw arguments exactly as the wire carried them. OpenAI always sends
    /// this as a JSON *string*, never as a parsed object.
    pub arguments: String,
}

impl ToolCall {
    /// The id to use when rendering an OpenAI chat-completions `tool_calls`
    /// entry, which the client matches against `tool_call_id`.
    pub fn wire_id(&self) -> &str {
        if self.call_id.is_empty() {
            &self.id
        } else {
            &self.call_id
        }
    }
}

/// A provider-independent chunk of a streamed completion. Outbound adapters
/// decode their provider's native SSE lines into these; inbound adapters render
/// each one in their own native SSE framing.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StreamEvent {
    /// Incremental text produced in this chunk.
    pub delta: String,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    /// Whether the stream is complete after this event.
    pub done: bool,
    /// Tool-call fragments carried by this chunk. Providers stream a call's
    /// id/name once and its arguments across many chunks, so consumers
    /// accumulate these by `index`.
    #[serde(default)]
    pub tool_call_deltas: Vec<ToolCallDelta>,
}

/// One streamed fragment of a [`ToolCall`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallDelta {
    /// Position of the call within the response; the accumulator key.
    pub index: u32,
    /// Present on the first fragment for a call.
    pub id: Option<String>,
    pub call_id: Option<String>,
    /// Present on the first fragment for a call.
    pub name: Option<String>,
    /// A fragment of the raw JSON argument string.
    pub arguments_delta: Option<String>,
}

/// An OpenAI completions request. Kept flexible with `#[serde(flatten)]`
/// so unknown provider-specific fields pass through untouched.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub prompt: serde_json::Value,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub logprobs: Option<u32>,
    #[serde(default)]
    pub echo: Option<bool>,
    #[serde(default)]
    pub stop: Option<serde_json::Value>,
    #[serde(default)]
    pub best_of: Option<u32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub logit_bias: Option<serde_json::Value>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// A single choice in a completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionChoice {
    pub text: String,
    pub index: u32,
    #[serde(default)]
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: String,
}

/// Usage stats returned with a completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// An OpenAI completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: CompletionUsage,
}

/// An OpenAI embeddings request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: serde_json::Value,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// A single embedding vector in an embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingData {
    pub object: String,
    pub index: u32,
    pub embedding: Vec<f32>,
}

/// Usage stats returned with an embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

/// An OpenAI embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_keeps_extra_fields_across_a_round_trip() {
        // An assistant tool call and its tool result must survive a
        // deserialize/serialize cycle unchanged: agentic clients such as the
        // Codex CLI cannot work if these are dropped in the middle.
        let body = serde_json::json!({
            "model": "prog/route",
            "messages": [
                { "role": "user", "content": "list the files" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "name": "shell", "content": "a.txt\nb.txt" }
            ],
            "stream": false, "tools": [{ "type": "function" }]
        });

        let req: ChatRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.messages[1].role, "assistant");
        assert_eq!(req.messages[1].extra["tool_calls"][0]["id"], "call_1");
        assert_eq!(req.messages[2].extra["tool_call_id"], "call_1");
        assert_eq!(req.messages[2].extra["name"], "shell");
        // Unmapped top-level fields still land in `extra`.
        assert_eq!(req.extra["tools"][0]["type"], "function");

        let out = serde_json::to_value(&req).unwrap();
        assert_eq!(
            out["messages"][1]["tool_calls"][0]["function"]["name"],
            "shell"
        );
        assert_eq!(out["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(out["messages"][2]["name"], "shell");
    }

    #[test]
    fn plain_message_has_no_extra_fields_and_builds_from_text() {
        let msg = Message::text("user", "hi");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.extra, serde_json::Value::Null);
        // No spurious keys leak into the serialized form.
        assert_eq!(
            serde_json::to_value(&msg).unwrap(),
            serde_json::json!({
                "role": "user", "content": "hi"
            })
        );
    }

    #[test]
    fn tool_call_uses_call_id_as_the_wire_id() {
        // Providers that only carry one id set both fields to it, so the wire
        // id stays stable whichever the upstream populated.
        let both = ToolCall {
            id: "fc_1".into(),
            call_id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        };
        assert_eq!(both.wire_id(), "call_1");

        let capture_only = ToolCall {
            id: "call_1".into(),
            call_id: String::new(),
            name: "shell".into(),
            arguments: "{}".into(),
        };
        assert_eq!(capture_only.wire_id(), "call_1");
    }

    #[test]
    fn canonical_and_stream_types_default_to_empty_tool_state() {
        // `Default` keeps existing construction sites terse and guarantees a
        // text-only turn carries no phantom tool calls.
        let resp = CanonicalResponse::default();
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.total_tokens(), 0);

        let ev = StreamEvent::default();
        assert!(ev.tool_call_deltas.is_empty());
        assert!(!ev.done);
    }

    #[test]
    fn tool_call_deltas_default_in_from_json() {
        // Older payloads (and every text-only chunk) omit the field entirely.
        let ev: StreamEvent = serde_json::from_value(serde_json::json!({
            "delta": "hi", "finish_reason": null, "done": false
        }))
        .unwrap();
        assert!(ev.tool_call_deltas.is_empty());
    }
}
