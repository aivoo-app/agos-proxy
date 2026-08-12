//! Canonical request/response model.
//!
//! This module defines the provider-independent data types every adapter
//! speaks: [`ChatRequest`] on the way in, [`CanonicalResponse`] and
//! [`StreamEvent`] on the way back, plus the OpenAI-compatible
//! completions/embeddings payload types used by the passthrough endpoints.
//! It contains no provider logic — see [`crate::adapter::inbound`] for the
//! per-surface inbound adapters and [`crate::adapter::outbound`] for the
//! per-provider outbound adapters built on top of these types.

/// An OpenAI-compatible chat completions request.
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
    /// The message content, kept as raw JSON so the OpenAI-compatible
    /// passthrough stays lossless. Plain text is a JSON string; multimodal
    /// requests use the OpenAI parts array (text / image_url entries).
    pub content: serde_json::Value,
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
/// single outbound result feed an OpenAI, Anthropic, or Gemini client
/// unchanged.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub text: String,
    pub finish_reason: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl CanonicalResponse {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// A provider-independent chunk of a streamed completion. Outbound adapters
/// decode their provider's native SSE lines into these; inbound adapters render
/// each one in their own native SSE framing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamEvent {
    /// Incremental text produced in this chunk.
    pub delta: String,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    /// Whether the stream is complete after this event.
    pub done: bool,
}

/// An OpenAI-compatible completions request. Kept flexible with `#[serde(flatten)]`
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

/// An OpenAI-compatible completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: CompletionUsage,
}

/// An OpenAI-compatible embeddings request.
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

/// An OpenAI-compatible embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

// ---------------------------------------------------------------------------
// Back-compat re-exports: the provider codecs moved to `crate::adapter::
// outbound`, but callers across the codebase still import them from here.
// These aliases are temporary; callers will be migrated to the adapter
// module in a follow-up commit and this shim removed.
// ---------------------------------------------------------------------------
#[allow(unused_imports)]
pub use crate::adapter::outbound::{
    anthropic, google, build_upstream_request, forward_non_streaming,
    normalize_base, parse_canonical, translate_response, ProviderError,
};
#[allow(unused_imports)]
pub use crate::adapter::outbound::openai::parse_stream_chunk as decode_openai_sse;
