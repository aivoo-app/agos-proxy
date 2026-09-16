//! Server-side SSE helpers for the streaming path.
//!
//! The streaming handler probes each upstream with the real request and only
//! commits to a 2xx response once the body proves itself: several
//! OpenAI-compatible providers (free tiers in particular) answer
//! HTTP 200 and then deliver an *in-band* SSE error event. Both the probe loop
//! and the pump task share this module's incremental frame parser so bytes are
//! never lost between probe and pump, plus the per-provider usage extraction
//! and OpenAI-chunk rendering used to translate Anthropic/Gemini streams.

use std::collections::VecDeque;

use crate::domain::ProviderKind;
use crate::translator::StreamEvent;

/// One decoded SSE event: the `event:` name (often empty) and the joined
/// `data:` payload lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

impl SseFrame {
    /// Re-emit the frame as wire bytes. OpenAI passthrough uses this so
    /// upstream frames reach the client byte-for-byte in payload terms.
    pub fn to_wire(&self) -> String {
        format!("data: {}\n\n", self.data)
    }
}

/// Incremental SSE frame parser. Feed it raw network chunks; it hands back
/// every *complete* frame and keeps partial lines buffered until their
/// terminator arrives.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes, returning all frames completed by this chunk.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(end) = find_frame_end(&self.buf) {
            // Find the frame terminator: a blank line (\n\n or \r\n\r\n).
            let raw = self.buf.drain(..end.0).collect::<Vec<u8>>();
            // Consume the terminator itself.
            self.buf.drain(..end.1);
            if let Some(frame) = parse_frame(&raw) {
                frames.push(frame);
            }
        }
        frames
    }

    /// Flush any trailing bytes as a final frame (streams that end without a
    /// blank-line terminator).
    pub fn finish(&mut self) -> Vec<SseFrame> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        let raw = std::mem::take(&mut self.buf);
        parse_frame(&raw).into_iter().collect()
    }

    /// Whether any partial frame bytes are still buffered.
    pub fn buf_is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Locate the end of the next complete frame in `buf`. Returns
/// `(frame_len, terminator_len)` — the frame content (with its line ending)
/// and the blank-line terminator that follows it.
fn find_frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            // Peek the next byte: another \n (or \r\n) means the frame ends
            // right after this line.
            if i + 1 < buf.len() && buf[i + 1] == b'\n' {
                return Some((i + 1, 1));
            }
            if i + 2 < buf.len() && buf[i + 1] == b'\r' && buf[i + 2] == b'\n' {
                return Some((i + 2, 1));
            }
        }
        i += 1;
    }
    None
}

/// Parse one raw frame (headers + terminators included) into an [`SseFrame`].
fn parse_frame(raw: &[u8]) -> Option<SseFrame> {
    let text = String::from_utf8_lossy(raw);
    let mut event = String::new();
    let mut data_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix("event:") {
            event = rest.strip_prefix(' ').unwrap_or(rest).to_string();
        }
        // `id:`, `retry:` and comments (`: keep-alive`) are ignored.
    }
    if data_lines.is_empty() {
        return None;
    }
    Some(SseFrame {
        event,
        data: data_lines.join("\n"),
    })
}

/// Classify one upstream SSE frame's data payload.
#[derive(Debug)]
pub enum Frame<'a> {
    /// The frame carries an upstream error payload (`{"error": {...}}`) —
    /// Some providers answer 200 and then deliver an error as the first
    Error {
        code: Option<i64>,
        message: String,
        raw: &'a str,
    },
    /// Terminal `data: [DONE]` sentinel.
    Done,
    /// Any other payload (JSON or opaque).
    Other {
        raw: &'a str,
        json: Option<serde_json::Value>,
    },
}

impl<'a> Frame<'a> {
    /// The top-level `error` key is the contract every OpenAI-compatible
    /// provider and both native adapters use for in-band errors.
    pub fn classify(data: &'a str) -> Self {
        let trimmed = data.trim();
        if trimmed == "[DONE]" {
            return Frame::Done;
        }
        let json: Option<serde_json::Value> = serde_json::from_str(trimmed).ok();
        if let Some(v) = &json {
            if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(|c| c.as_i64());
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("upstream reported an in-band error")
                    .to_string();
                return Frame::Error {
                    code,
                    message,
                    raw: trimmed,
                };
            }
        }
        Frame::Other { raw: trimmed, json }
    }
}

/// Extract `(prompt_tokens, completion_tokens)` from a frame's JSON, per
/// provider shape. OpenAI-compatible chunks carry a `usage` object (sent when
/// `stream_options.include_usage` is set); Anthropic splits it across
/// `message_start` (input) and `message_delta` (output); Gemini carries
/// `usageMetadata` on every chunk.
pub fn frame_usage(kind: ProviderKind, json: &serde_json::Value) -> (Option<u64>, Option<u64>) {
    let num = |v: &serde_json::Value| v.as_u64().or_else(|| v.as_i64().map(|n| n.max(0) as u64));
    match kind {
        ProviderKind::OpenAICompatible | ProviderKind::Custom => {
            let usage = json.get("usage");
            let prompt = usage.and_then(|u| u.get("prompt_tokens")).and_then(num);
            let completion = usage.and_then(|u| u.get("completion_tokens")).and_then(num);
            (prompt, completion)
        }
        // Never reached in-stream: Responses upstreams are served non-streamed.
        ProviderKind::OpenAIResponses => (None, None),
        ProviderKind::Anthropic => match json.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => (
                json.pointer("/message/usage/input_tokens").and_then(num),
                None,
            ),
            Some("message_delta") => (None, json.pointer("/usage/output_tokens").and_then(num)),
            _ => (None, None),
        },
        ProviderKind::Google => (
            json.pointer("/usageMetadata/promptTokenCount")
                .and_then(num),
            json.pointer("/usageMetadata/candidatesTokenCount")
                .and_then(num),
        ),
    }
}

/// Render a canonical [`StreamEvent`] as an OpenAI `chat.completion.chunk` JSON
/// string (the `data:` payload, without framing). Used to translate Anthropic
/// and Gemini streams for OpenAI-format clients.
pub fn render_openai_chunk(
    model: &str,
    id: &str,
    ev: &StreamEvent,
    include_role: bool,
) -> Option<String> {
    let mut delta = serde_json::Map::new();
    if include_role {
        delta.insert("role".into(), serde_json::Value::String("assistant".into()));
    }
    if !ev.delta.is_empty() {
        delta.insert(
            "content".into(),
            serde_json::Value::String(ev.delta.clone()),
        );
    }
    if !ev.tool_call_deltas.is_empty() {
        let calls: Vec<serde_json::Value> = ev
            .tool_call_deltas
            .iter()
            .map(|t| {
                let mut obj = serde_json::Map::new();
                obj.insert("index".into(), serde_json::json!(t.index));
                if let Some(id) = &t.id {
                    obj.insert("id".into(), serde_json::json!(id));
                    obj.insert("type".into(), serde_json::json!("function"));
                }
                let mut function = serde_json::Map::new();
                if let Some(name) = &t.name {
                    function.insert("name".into(), serde_json::json!(name));
                }
                if let Some(args) = &t.arguments_delta {
                    function.insert("arguments".into(), serde_json::json!(args));
                }
                if !function.is_empty() {
                    obj.insert("function".into(), serde_json::Value::Object(function));
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        delta.insert("tool_calls".into(), serde_json::Value::Array(calls));
    }
    if delta.is_empty() && ev.finish_reason.is_none() {
        // Nothing observable in this event; skip emitting an empty chunk.
        return None;
    }
    let chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": serde_json::Value::Object(delta),
            "finish_reason": ev.finish_reason,
        }],
    });
    Some(serde_json::to_string(&chunk).unwrap_or_default())
}

/// Queue of frames the probe reader decoded after it committed but before the
/// pump took ownership of the stream. Drained first so no bytes are lost.
pub type PendingFrames = VecDeque<SseFrame>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_handles_split_chunks_and_crlf() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"ch").is_empty());
        let frames = p.feed(b"unk\":1}\r\n\r\ndata: [DONE]\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, r#"{"chunk":1}"#);
        assert_eq!(frames[1].data, "[DONE]");
    }

    #[test]
    fn parser_skips_comments_and_multi_line_data() {
        let mut p = SseParser::new();
        let frames = p.feed(b": PROCESSING\n\nevent: delta\ndata: a\ndata: b\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "delta");
        assert_eq!(frames[0].data, "a\nb");
    }

    #[test]
    fn finish_flushes_unterminated_frame() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"x\":1}").is_empty());
        let frames = p.finish();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, r#"{"x":1}"#);
    }

    #[test]
    fn classify_detects_in_band_errors() {
        let f = Frame::classify(r#"{"error": {"message": "Rate limit exceeded", "code": 429}}"#);
        match f {
            Frame::Error { code, message, .. } => {
                assert_eq!(code, Some(429));
                assert_eq!(message, "Rate limit exceeded");
            }
            _ => panic!("expected error"),
        }
        // Anthropic-style error events carry the same top-level key.
        assert!(matches!(
            Frame::classify(r#"{"type":"error","error":{"type":"overloaded_error"}}"#),
            Frame::Error { .. }
        ));
    }

    #[test]
    fn classify_content_and_done() {
        assert!(matches!(Frame::classify("[DONE]"), Frame::Done));
        let f = Frame::classify(
            r#"{"id":"1","choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#,
        );
        assert!(matches!(f, Frame::Other { .. }));
    }

    #[test]
    fn usage_extraction_per_provider() {
        let openai: serde_json::Value =
            serde_json::from_str(r#"{"usage":{"prompt_tokens":5,"completion_tokens":7}}"#).unwrap();
        assert_eq!(
            frame_usage(ProviderKind::OpenAICompatible, &openai),
            (Some(5), Some(7))
        );
        let start: serde_json::Value = serde_json::from_str(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":11}}}"#,
        )
        .unwrap();
        assert_eq!(
            frame_usage(ProviderKind::Anthropic, &start),
            (Some(11), None)
        );
        let delta: serde_json::Value =
            serde_json::from_str(r#"{"type":"message_delta","usage":{"output_tokens":3}}"#)
                .unwrap();
        assert_eq!(
            frame_usage(ProviderKind::Anthropic, &delta),
            (None, Some(3))
        );
        let gemini: serde_json::Value = serde_json::from_str(
            r#"{"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":4}}"#,
        )
        .unwrap();
        assert_eq!(
            frame_usage(ProviderKind::Google, &gemini),
            (Some(2), Some(4))
        );
    }

    #[test]
    fn chunk_rendering_includes_role_once_and_tool_calls() {
        let ev = StreamEvent {
            delta: "hi".into(),
            ..Default::default()
        };
        let first = render_openai_chunk("prog/r", "id1", &ev, true).unwrap();
        assert!(first.contains(r#""role":"assistant""#));
        assert!(first.contains(r#""content":"hi""#));

        let tool = StreamEvent {
            tool_call_deltas: vec![crate::translator::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                call_id: None,
                name: Some("shell".into()),
                arguments_delta: Some(r#"{"cmd":"#.into()),
            }],
            ..Default::default()
        };
        let chunk = render_openai_chunk("prog/r", "id1", &tool, false).unwrap();
        assert!(chunk.contains(r#""tool_calls""#));
        let parsed: serde_json::Value = serde_json::from_str(&chunk).unwrap();
        assert_eq!(
            parsed["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str(),
            Some("{\"cmd\":")
        );
    }
}
