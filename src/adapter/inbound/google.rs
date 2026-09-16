//! Google (Google) inbound adapter.
//!
//! Serves `/google/v1beta/models/{model}:generateContent` and
//! `:streamGenerateContent`. The Google SDK places the model in the URL path;
//! the server injects it into the body before calling [`parse_request`] so the
//! adapter stays focused on request/response shape.

use crate::adapter::inbound::InboundAdapter;
use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, Message, StreamEvent};

/// The Google native inbound surface.
pub struct GoogleAdapter;

fn parts_text(parts: &serde_json::Value) -> String {
    parts
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Read a part field that the Gemini wire format spells in either camelCase
/// (`inlineData`) or snake_case (`inline_data`).
fn part_field<'a>(
    part: &'a serde_json::Value,
    camel: &str,
    snake: &str,
) -> Option<&'a serde_json::Value> {
    part.get(camel).or_else(|| part.get(snake))
}

/// Collapse Google `parts` into the canonical message content.
///
/// Parts without any image (`inlineData` / `fileData`) stay a single joined
/// text string, so text-only requests serialize byte-identically through the
/// pipeline. Parts carrying images become the OpenAI parts array — the
/// canonical image encoding the outbound adapters translate back into their
/// native shape: `inlineData` re-encodes as a `data:<mime>;base64,<data>` URL
/// and `fileData` becomes an `image_url` part referencing the `fileUri`.
fn google_content(parts: &serde_json::Value) -> serde_json::Value {
    let has_image = parts.as_array().is_some_and(|arr| {
        arr.iter().any(|p| {
            part_field(p, "inlineData", "inline_data").is_some()
                || part_field(p, "fileData", "file_data").is_some()
        })
    });
    if !has_image {
        return serde_json::Value::String(parts_text(parts));
    }

    let canonical: Vec<serde_json::Value> = parts
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    if let Some(inline) = part_field(p, "inlineData", "inline_data") {
                        let mime = inline
                            .get("mimeType")
                            .or_else(|| inline.get("mime_type"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("image/png");
                        let data = inline
                            .get("data")
                            .and_then(|d| d.as_str())
                            .unwrap_or_default();
                        return Some(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{mime};base64,{data}") },
                        }));
                    }
                    if let Some(file) = part_field(p, "fileData", "file_data") {
                        let uri = file
                            .get("fileUri")
                            .or_else(|| file.get("file_uri"))
                            .and_then(|u| u.as_str())?;
                        return Some(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": uri },
                        }));
                    }
                    let text = p.get("text").and_then(|t| t.as_str())?;
                    Some(serde_json::json!({ "type": "text", "text": text }))
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::Value::Array(canonical)
}

/// Map a Google finish reason onto our canonical finish reason.
pub fn canonical_finish(reason: &str) -> &'static str {
    match reason {
        "MAX_TOKENS" => "length",
        _ => "stop",
    }
}

/// Map our canonical finish reason back onto a Google finish reason.
pub fn google_finish(reason: &str) -> &'static str {
    match reason {
        "length" => "MAX_TOKENS",
        _ => "STOP",
    }
}

impl InboundAdapter for GoogleAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::Google
    }

    fn parse_request(&self, body: &serde_json::Value) -> anyhow::Result<ChatRequest> {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let stream = body
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);

        let mut messages: Vec<Message> = Vec::new();
        if let Some(sys) = body.pointer("/systemInstruction/parts") {
            let text = parts_text(sys);
            if !text.is_empty() {
                messages.push(Message::text("system", text));
            }
        }
        if let Some(contents) = body.get("contents").and_then(|c| c.as_array()) {
            for c in contents {
                let role = match c.get("role").and_then(|r| r.as_str()) {
                    Some("model") => "assistant",
                    _ => "user",
                };
                let content = google_content(c.get("parts").unwrap_or(&serde_json::Value::Null));
                messages.push(Message::new(role, content));
            }
        }

        let mut extra = body.clone();
        if let Some(obj) = extra.as_object_mut() {
            obj.remove("model");
            obj.remove("contents");
            obj.remove("systemInstruction");
            obj.remove("stream");
            if let Some(cfg) = obj
                .remove("generationConfig")
                .and_then(|g| g.as_object().cloned())
            {
                for (k, v) in cfg {
                    let mapped = match k.as_str() {
                        "maxOutputTokens" => "max_tokens",
                        "stopSequences" => "stop",
                        "topP" => "top_p",
                        "topK" => "top_k",
                        other => other,
                    };
                    obj.insert(mapped.to_string(), v);
                }
            }
        }

        Ok(ChatRequest {
            model,
            messages,
            stream,
            extra,
        })
    }

    fn render_response(&self, resp: &CanonicalResponse) -> serde_json::Value {
        serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": resp.text }],
                    "role": "model",
                },
                "finishReason": google_finish(&resp.finish_reason),
                "index": 0,
            }],
            "usageMetadata": {
                "promptTokenCount": resp.prompt_tokens,
                "candidatesTokenCount": resp.completion_tokens,
                "totalTokenCount": resp.total_tokens(),
            },
            "modelVersion": resp.model,
        })
    }

    fn render_stream_event(&self, ev: &StreamEvent, _id: &str) -> Option<String> {
        let mut candidate = serde_json::Map::new();
        let mut content = serde_json::Map::new();
        content.insert(
            "role".to_string(),
            serde_json::Value::String("model".to_string()),
        );
        let parts = if ev.delta.is_empty() {
            Vec::new()
        } else {
            vec![serde_json::json!({ "text": ev.delta })]
        };
        content.insert("parts".to_string(), serde_json::Value::Array(parts));
        candidate.insert("content".to_string(), serde_json::Value::Object(content));
        if let Some(reason) = &ev.finish_reason {
            candidate.insert(
                "finishReason".to_string(),
                serde_json::Value::String(google_finish(reason).to_string()),
            );
        }
        let payload = serde_json::json!({ "candidates": [serde_json::Value::Object(candidate)] });
        Some(format!("data: {}\n\n", payload))
    }

    fn stream_end_marker(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_data_becomes_a_data_url_part() {
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "text": "what is this" },
                    { "inlineData": { "mimeType": "image/png", "data": "QUJD" } },
                ]
            }]
        });
        let req = GoogleAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}
            ])
        );
    }

    #[test]
    fn file_data_becomes_an_image_url_part() {
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "fileData": { "mimeType": "image/jpeg", "fileUri": "https://x/cat.jpg" } },
                    { "text": "describe" },
                ]
            }]
        });
        let req = GoogleAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "image_url", "image_url": {"url": "https://x/cat.jpg"}},
                {"type": "text", "text": "describe"}
            ])
        );
    }

    #[test]
    fn snake_case_parts_are_recognized_too() {
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "inline_data": { "mime_type": "image/webp", "data": "QUJD" } }
                ]
            }]
        });
        let req = GoogleAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "image_url", "image_url": {"url": "data:image/webp;base64,QUJD"}}
            ])
        );
    }

    #[test]
    fn text_only_parts_stay_a_plain_string() {
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": "hi" }, { "text": "there" }]
            }]
        });
        let req = GoogleAdapter.parse_request(&body).unwrap();
        assert_eq!(req.messages[0].content, serde_json::json!("hi\nthere"));
    }

    #[test]
    fn system_stays_text_only() {
        let body = serde_json::json!({
            "systemInstruction": { "parts": [{ "text": "be terse" }] },
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
        });
        let req = GoogleAdapter.parse_request(&body).unwrap();
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[0].content, serde_json::json!("be terse"));
    }
}
