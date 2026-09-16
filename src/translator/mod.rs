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

/// One ordered segment of a multimodal message content.
///
/// The canonical request keeps message content as raw JSON — a plain string,
/// or the OpenAI parts array. [`content_parts`] reduces that raw form into
/// these typed parts so outbound adapters can rebuild their native shape
/// (Anthropic blocks, Google parts) without re-parsing the OpenAI wire format
/// themselves. Plain-text-only content still flows through [`content_text`],
/// which is built on the same parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentPart {
    /// A plain text segment.
    Text(String),
    /// An image, referenced either by an `http(s)://` URL or by a
    /// `data:<mime>[;base64],<payload>` data URL. Both live verbatim in
    /// `url`; see [`parse_data_url`] to split the data-URL form.
    Image { url: String },
}

/// A decoded `data:` URL: the MIME type from the header and the (base64)
/// payload after the comma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataUrl {
    pub mime: String,
    pub data: String,
}

/// Split a `data:<mime>[;base64],<payload>` URL into its MIME type and
/// payload. Returns `None` for anything that is not a data URL. A payload
/// without the `;base64` marker is still accepted — its header is kept as the
/// MIME type verbatim, mirroring the URL standard.
pub fn parse_data_url(url: &str) -> Option<DataUrl> {
    let rest = url.strip_prefix("data:")?;
    let (header, payload) = rest.split_once(',')?;
    let header = header.strip_suffix(";base64").unwrap_or(header);
    let mime = if header.is_empty() {
        "application/octet-stream".to_string()
    } else {
        header.to_string()
    };
    Some(DataUrl {
        mime,
        data: payload.to_string(),
    })
}

/// Best-effort MIME type for an image URL, inferred from the file extension.
/// Google's `fileData` part requires an explicit MIME type and the OpenAI wire
/// format does not carry one, so unknown extensions fall back to `image/jpeg`.
pub fn infer_image_mime(url: &str) -> &'static str {
    // Strip any query string / fragment before matching the extension.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// Extract the ordered content parts of a message content value.
///
/// A plain string becomes a single [`ContentPart::Text`]. The OpenAI parts
/// array contributes its `text` and `image_url` entries in order; parts of
/// any other type are ignored. Non-string, non-array content (null, objects)
/// yields no parts.
pub fn content_parts(content: &serde_json::Value) -> Vec<ContentPart> {
    match content {
        serde_json::Value::String(s) => vec![ContentPart::Text(s.clone())],
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(|t| t.as_str()) {
                Some("text") => p
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(|t| ContentPart::Text(t.to_string())),
                Some("image_url") => Some(ContentPart::Image {
                    url: image_part_url(p),
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The URL of an OpenAI `image_url` part. The wire format nests it under
/// `image_url.url`, but a bare string
/// (`{"type": "image_url", "image_url": "https://…"}`) is accepted defensively.
fn image_part_url(part: &serde_json::Value) -> String {
    match part.get("image_url") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string(),
        None => String::new(),
    }
}

/// Whether the message content carries any image part.
pub fn content_has_image(content: &serde_json::Value) -> bool {
    content_parts(content)
        .iter()
        .any(|p| matches!(p, ContentPart::Image { .. }))
}

/// Extract the plain-text portion of a message content value. Multimodal
/// (array) content contributes its `text` parts; image parts are dropped.
pub fn content_text(content: &serde_json::Value) -> String {
    content_parts(content)
        .into_iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) => Some(t),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn content_parts_orders_text_and_images() {
        let plain = serde_json::json!("hello");
        assert_eq!(
            content_parts(&plain),
            vec![ContentPart::Text("hello".into())]
        );

        let mixed = serde_json::json!([
            {"type": "text", "text": "what is this"},
            {"type": "image_url", "image_url": {"url": "https://x/cat.png"}},
            {"type": "text", "text": "answer briefly"},
            // Unknown part types are ignored.
            {"type": "input_audio", "input_audio": {"data": "..."}}
        ]);
        assert_eq!(
            content_parts(&mixed),
            vec![
                ContentPart::Text("what is this".into()),
                ContentPart::Image {
                    url: "https://x/cat.png".into()
                },
                ContentPart::Text("answer briefly".into()),
            ]
        );
    }

    #[test]
    fn content_parts_accepts_a_bare_string_image_url() {
        let arr = serde_json::json!([
            {"type": "image_url", "image_url": "https://x/cat.png"}
        ]);
        assert_eq!(
            content_parts(&arr),
            vec![ContentPart::Image {
                url: "https://x/cat.png".into()
            }]
        );
    }

    #[test]
    fn content_has_image_detects_images_only() {
        assert!(!content_has_image(&serde_json::json!("hi")));
        assert!(!content_has_image(&serde_json::json!(["plain string"])));
        assert!(!content_has_image(&serde_json::json!([
            {"type": "text", "text": "hi"}
        ])));
        assert!(content_has_image(&serde_json::json!([
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
        ])));
    }

    #[test]
    fn content_text_is_unchanged_by_the_parts_model() {
        // The text-only output must stay byte-identical to the pre-parts
        // implementation: strings pass through, text parts join with "\n",
        // images and other values contribute nothing.
        assert_eq!(content_text(&serde_json::json!("plain")), "plain");
        assert_eq!(content_text(&serde_json::json!(null)), "");
        assert_eq!(
            content_text(&serde_json::json!([
                {"type": "text", "text": "line one"},
                {"type": "image_url", "image_url": {"url": "https://x/i.png"}},
                {"type": "text", "text": "line two"}
            ])),
            "line one\nline two"
        );
    }

    #[test]
    fn data_urls_split_into_mime_and_payload() {
        let d = parse_data_url("data:image/png;base64,QUJD").unwrap();
        assert_eq!(d.mime, "image/png");
        assert_eq!(d.data, "QUJD");

        // Payloads without the ;base64 marker keep their header verbatim.
        let d = parse_data_url("data:text/plain,hello").unwrap();
        assert_eq!(d.mime, "text/plain");
        assert_eq!(d.data, "hello");

        // An empty header falls back to the generic MIME.
        assert_eq!(
            parse_data_url("data:,QUJD").unwrap().mime,
            "application/octet-stream"
        );

        // Anything that is not a data URL is rejected.
        assert!(parse_data_url("https://x/cat.png").is_none());
        assert!(parse_data_url("data:").is_none());
        assert!(parse_data_url("data:image/png").is_none());
    }

    #[test]
    fn image_mime_is_inferred_from_the_extension() {
        assert_eq!(infer_image_mime("https://x/cat.png"), "image/png");
        assert_eq!(infer_image_mime("https://x/cat.PNG?v=1#f"), "image/png");
        assert_eq!(infer_image_mime("https://x/cat.jpg"), "image/jpeg");
        assert_eq!(infer_image_mime("https://x/cat.jpeg"), "image/jpeg");
        assert_eq!(infer_image_mime("https://x/cat.gif"), "image/gif");
        assert_eq!(infer_image_mime("https://x/cat.webp"), "image/webp");
        // Unknown extension — and HTTP URLs carry no MIME — fall back.
        assert_eq!(infer_image_mime("https://x/cat.heic"), "image/jpeg");
        assert_eq!(infer_image_mime("https://x/image"), "image/jpeg");
    }
}
