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

use serde::de::Error as _;

/// An OpenAI chat completions request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    /// Stable caller/proxy correlation id. It is deliberately not serialized
    /// into provider payloads; the outbound layer forwards it as `X-Request-ID`
    /// and the usage log stores it as attempt metadata instead.
    #[serde(skip)]
    pub request_id: Option<String>,

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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContentPart {
    /// A plain text segment.
    Text(String),
    /// An image reference (`http(s)` or `data:` URL).
    Image { url: String },
    /// An audio reference. OpenAI inline audio is normalized to a `data:` URL so
    /// every adapter sees one representation.
    Audio { url: String },
    /// A video reference.
    Video { url: String },
    /// A generic file reference (for example a PDF).
    File { url: String },
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
    let _header = header.strip_suffix(";base64").unwrap_or(header);
    let mime = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .map(|(header, _)| header.trim_end_matches(";base64"))
        .filter(|header| !header.is_empty())
        .unwrap_or("application/octet-stream");
    Some(DataUrl {
        mime: mime.to_string(),
        data: payload.to_string(),
    })
}

/// Best-effort MIME type for a media URL, inferred from its extension.
/// Google's `fileData` part requires an explicit MIME type and canonical media
/// references do not always carry one, so unknown extensions use a conservative
/// type for the declared modality.
pub fn infer_media_mime(kind: &ContentPart, url: &str) -> &'static str {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let extension = [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".mp3", ".wav", ".ogg", ".mp4", ".webm", ".mov",
        ".pdf",
    ]
    .into_iter()
    .find(|ext| path.ends_with(ext))
    .unwrap_or("");
    match extension {
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".ogg" => "audio/ogg",
        ".mp4" => "video/mp4",
        ".webm" => "video/webm",
        ".mov" => "video/quicktime",
        ".pdf" => "application/pdf",
        _ => match kind {
            ContentPart::Image { .. } => "image/jpeg",
            ContentPart::Audio { .. } => "audio/mpeg",
            ContentPart::Video { .. } => "video/mp4",
            ContentPart::File { .. } => "application/octet-stream",
            ContentPart::Text(_) => "text/plain",
        },
    }
}

/// Backwards-compatible image MIME inference.
pub fn infer_image_mime(url: &str) -> &'static str {
    infer_media_mime(
        &ContentPart::Image {
            url: url.to_string(),
        },
        url,
    )
}

/// Strictly parse canonical content parts. Unlike [`content_parts`], this
/// rejects unknown object/part types instead of silently omitting them.
pub fn parse_content_parts(
    content: &serde_json::Value,
) -> Result<Vec<ContentPart>, serde_json::Error> {
    let serde_json::Value::String(text) = content else {
        if content.is_null() {
            return Ok(Vec::new());
        }
        let serde_json::Value::Array(parts) = content else {
            return Err(content_error(
                "message content must be a string, array, or null",
            ));
        };
        return parts.iter().map(parse_content_part).collect();
    };
    Ok(vec![ContentPart::Text(text.clone())])
}

fn parse_content_part(part: &serde_json::Value) -> Result<ContentPart, serde_json::Error> {
    let kind = part
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| serde_json::Error::custom("content part is missing a string `type`"))?;
    match kind {
        "text" | "input_text" | "output_text" => Ok(ContentPart::Text(
            part.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        )),
        "image_url" | "input_image" => Ok(ContentPart::Image {
            url: media_url(part, "image_url")?,
        }),
        "audio_url" => Ok(ContentPart::Audio {
            url: media_url(part, "audio_url")?,
        }),
        "video_url" => Ok(ContentPart::Video {
            url: media_url(part, "video_url")?,
        }),
        "input_audio" => {
            let audio = part
                .get("input_audio")
                .ok_or_else(|| content_error("input_audio is missing `input_audio`"))?;
            let data = audio
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| content_error("input_audio is missing base64 `data`"))?;
            let format = audio
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("mpeg");
            let url = format!("data:audio/{format};base64,{data}");
            Ok(ContentPart::Audio { url })
        }
        "file" | "input_file" => {
            let url = part
                .get("file")
                .and_then(|v| media_url(v, "file").ok())
                .or_else(|| {
                    part.get("file_data")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .or_else(|| {
                    part.get("file_url")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .ok_or_else(|| content_error("file/input_file needs file_data or file_url"))?;
            Ok(ContentPart::File { url })
        }
        other => Err(content_error(&format!(
            "unsupported content part type {other:?}"
        ))),
    }
}

fn content_error(message: &str) -> serde_json::Error {
    serde_json::Error::custom(message)
}

fn media_url(part: &serde_json::Value, key: &str) -> Result<String, serde_json::Error> {
    let value = part
        .get(key)
        .ok_or_else(|| content_error(&format!("content part is missing `{key}`")))?;
    match value {
        serde_json::Value::String(url) => Ok(url.clone()),
        serde_json::Value::Object(_) if key == "file" => value
            .get("file_url")
            .or_else(|| value.get("file_data"))
            .and_then(|url| url.as_str())
            .map(str::to_string)
            .ok_or_else(|| content_error("file must carry `file_url` or `file_data`")),
        serde_json::Value::Object(_) => value
            .get("url")
            .or_else(|| value.get("file_url"))
            .or_else(|| value.get("file_data"))
            .and_then(|url| url.as_str())
            .map(str::to_string)
            .ok_or_else(|| content_error(&format!("`{key}` must carry a URL or inline data"))),
        _ => Err(content_error(&format!(
            "`{key}` must be a string or object"
        ))),
    }
}

/// Render one canonical media part as an OpenAI chat content part.
pub fn content_part_to_chat_json(part: &ContentPart) -> serde_json::Value {
    match part {
        ContentPart::Text(text) => serde_json::json!({ "type": "text", "text": text }),
        ContentPart::Image { url } => {
            serde_json::json!({ "type": "image_url", "image_url": { "url": url } })
        }
        ContentPart::Audio { url } => {
            serde_json::json!({ "type": "audio_url", "audio_url": { "url": url } })
        }
        ContentPart::Video { url } => {
            serde_json::json!({ "type": "video_url", "video_url": { "url": url } })
        }
        ContentPart::File { url } => {
            serde_json::json!({ "type": "file", "file": { "file_url": url } })
        }
    }
}

/// Extract the known ordered content parts of a message. Malformed/unknown
/// parts are omitted; cross-protocol adapters use [`parse_content_parts`] so
/// they can reject those parts instead of losing them.
pub fn content_parts(content: &serde_json::Value) -> Vec<ContentPart> {
    parse_content_parts(content).unwrap_or_default()
}

/// Whether the message content carries any image part.
pub fn content_has_image(content: &serde_json::Value) -> bool {
    content_parts(content)
        .iter()
        .any(|p| matches!(p, ContentPart::Image { .. }))
}

/// Whether the message carries audio, video, or file input.
pub fn content_has_non_image_media(content: &serde_json::Value) -> bool {
    content_parts(content).iter().any(|p| {
        matches!(
            p,
            ContentPart::Audio { .. } | ContentPart::Video { .. } | ContentPart::File { .. }
        )
    })
}

/// Extract the plain-text portion of a message content value. Multimodal
/// (array) content contributes its `text` parts; media parts are omitted.
pub fn content_text(content: &serde_json::Value) -> String {
    content_parts(content)
        .into_iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) => Some(t),
            ContentPart::Image { .. }
            | ContentPart::Audio { .. }
            | ContentPart::Video { .. }
            | ContentPart::File { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Token accounting pulled from an upstream response body.
///
/// `(prompt_tokens, completion_tokens, cached_prompt_tokens)`. The cached count
/// is `None` when the upstream did not report one — which is *not* the same as
/// "nothing was cached", so it is stored as NULL rather than 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTokens {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_prompt_tokens: Option<i64>,
}

/// Extract token usage from a response body in any of the three wire shapes the
/// proxy can be handed back: OpenAI chat-completions, Anthropic messages, and
/// Google generateContent.
///
/// Cache-read counts are read per provider because each names them differently:
/// OpenAI `usage.prompt_tokens_details.cached_tokens`, Anthropic
/// `usage.cache_read_input_tokens`, Google `usageMetadata.cachedContentTokenCount`.
/// Reporting them is what makes prompt caching verifiable from `agos-proxy stats`
/// instead of a matter of trust.
pub fn extract_usage_tokens(body: &[u8]) -> (Option<i64>, Option<i64>, Option<i64>) {
    let u = parse_usage_tokens(body);
    (u.prompt_tokens, u.completion_tokens, u.cached_prompt_tokens)
}

/// Same as [`extract_usage_tokens`], but returns the named struct.
pub fn parse_usage_tokens(body: &[u8]) -> UsageTokens {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return UsageTokens::default();
    };
    let usage = v.get("usage").or_else(|| v.get("usageMetadata"));
    let Some(usage) = usage else {
        return UsageTokens::default();
    };
    let get = |keys: &[&str]| -> Option<i64> {
        keys.iter()
            .find_map(|k| usage.get(*k).and_then(|t| t.as_i64()))
    };
    // OpenAI nests the cache-read count one level down; the other two are flat.
    let openai_cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|t| t.as_i64());
    UsageTokens {
        prompt_tokens: get(&["prompt_tokens", "input_tokens", "promptTokenCount"]),
        completion_tokens: get(&["completion_tokens", "output_tokens", "candidatesTokenCount"]),
        cached_prompt_tokens: get(&["cache_read_input_tokens", "cachedContentTokenCount"])
            .or(openai_cached),
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
    /// Non-text output parts returned by a multimodal provider.
    #[serde(default)]
    pub media: Vec<ContentPart>,
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
            // Audio is now a first-class canonical part.
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
                ContentPart::Audio {
                    url: "data:audio/mpeg;base64,...".into()
                },
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
