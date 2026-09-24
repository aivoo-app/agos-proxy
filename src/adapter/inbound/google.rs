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
    let has_media = parts.as_array().is_some_and(|arr| {
        arr.iter().any(|p| {
            part_field(p, "inlineData", "inline_data").is_some()
                || part_field(p, "fileData", "file_data").is_some()
        })
    });
    if !has_media {
        return serde_json::Value::String(parts_text(parts));
    }
    let canonical: Vec<serde_json::Value> = parts
        .as_array()
        .map(|arr| arr.iter().filter_map(google_part).collect())
        .unwrap_or_default();
    serde_json::Value::Array(canonical)
}

fn google_part(part: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(inline) = part_field(part, "inlineData", "inline_data") {
        let mime = inline
            .get("mimeType")
            .or_else(|| inline.get("mime_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream");
        let data = inline
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        return Some(if mime.starts_with("image/") {
            serde_json::json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{data}") } })
        } else if mime.starts_with("audio/") {
            serde_json::json!({ "type": "input_audio", "input_audio": { "data": data, "format": mime.trim_start_matches("audio/") } })
        } else if mime.starts_with("video/") {
            serde_json::json!({ "type": "video_url", "video_url": { "url": format!("data:{mime};base64,{data}") } })
        } else {
            serde_json::json!({ "type": "file", "file": { "file_data": data, "filename": "upload" } })
        });
    }
    if let Some(file) = part_field(part, "fileData", "file_data") {
        let mime = file
            .get("mimeType")
            .or_else(|| file.get("mime_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream");
        let uri = file
            .get("fileUri")
            .or_else(|| file.get("file_uri"))
            .and_then(|v| v.as_str())?;
        return Some(if mime.starts_with("image/") {
            serde_json::json!({ "type": "image_url", "image_url": { "url": uri } })
        } else if mime.starts_with("audio/") {
            serde_json::json!({ "type": "audio_url", "audio_url": { "url": uri } })
        } else if mime.starts_with("video/") {
            serde_json::json!({ "type": "video_url", "video_url": { "url": uri } })
        } else {
            serde_json::json!({ "type": "file", "file": { "file_url": uri } })
        });
    }
    part.get("text")
        .and_then(|text| text.as_str())
        .map(|text| serde_json::json!({ "type": "text", "text": text }))
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
                let parts = c.get("parts").cloned().unwrap_or(serde_json::Value::Null);
                let mut message = Message::new(role, google_content(&parts));
                let calls: Vec<_> = parts.as_array().into_iter().flat_map(|parts| parts.iter()).filter_map(|part| {
                    let call = part.get("functionCall")?;
                    Some(serde_json::json!({
                        "id": call.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                        "type": "function",
                        "function": { "name": call.get("name").and_then(|v| v.as_str()).unwrap_or_default(), "arguments": call.get("args").map(|v| if v.is_string() { v.clone() } else { serde_json::Value::String(v.to_string()) }).unwrap_or_else(|| serde_json::Value::String("{}".into())) }
                    }))
                }).collect();
                if !calls.is_empty() {
                    message.extra = serde_json::json!({ "tool_calls": calls });
                }
                let responses: Vec<_> = parts
                    .as_array()
                    .into_iter()
                    .flat_map(|parts| parts.iter())
                    .filter_map(|part| part.get("functionResponse"))
                    .collect();
                if let Some(response) = responses.first() {
                    message.role = "tool".to_string();
                    let name = response
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    message.content = response
                        .get("response")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    message.extra = serde_json::json!({ "tool_call_id": name, "name": name });
                }
                messages.push(message);
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
            if let Some(tools) = obj.remove("tools") {
                let declarations = tools
                    .pointer("/0/functionDeclarations")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let converted: Vec<_> = declarations
                    .into_iter()
                    .map(|function| {
                        serde_json::json!({
                            "type": "function", "function": function
                        })
                    })
                    .collect();
                obj.insert("tools".into(), serde_json::Value::Array(converted));
            }
            if let Some(config) = obj.remove("toolConfig") {
                if let Some(mode) = config
                    .pointer("/functionCallingConfig/mode")
                    .and_then(|v| v.as_str())
                {
                    obj.insert(
                        "tool_choice".into(),
                        serde_json::json!(match mode {
                            "ANY" => "required",
                            "NONE" => "none",
                            _ => "auto",
                        }),
                    );
                }
            }
        }

        Ok(ChatRequest {
            model,
            messages,
            stream,
            extra,
            request_id: None,
        })
    }

    fn render_response(&self, resp: &CanonicalResponse) -> anyhow::Result<serde_json::Value> {
        let mut parts = Vec::new();
        if !resp.text.is_empty() {
            parts.extend(resp.media.iter().map(|part| match part {
            crate::translator::ContentPart::Text(text) => serde_json::json!({ "text": text }),
            crate::translator::ContentPart::Image { url } | crate::translator::ContentPart::Audio { url } | crate::translator::ContentPart::Video { url } | crate::translator::ContentPart::File { url } => {
                if let Some(data) = crate::translator::parse_data_url(url) {
                    serde_json::json!({ "inlineData": { "mimeType": data.mime, "data": data.data } })
                } else {
                    serde_json::json!({ "fileData": { "mimeType": "application/octet-stream", "fileUri": url } })
                }
            }
        }));

            parts.push(serde_json::json!({ "text": resp.text }));
        }
        parts.extend(resp.tool_calls.iter().map(|call| serde_json::json!({
            "functionCall": { "name": call.name, "args": serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap_or(serde_json::json!({})) }
        })));
        Ok(serde_json::json!({
            "candidates": [{
                "content": { "parts": parts, "role": "model" },
                "finishReason": if resp.tool_calls.is_empty() { google_finish(&resp.finish_reason) } else { "STOP" },
                "index": 0,
            }],
            "usageMetadata": {
                "promptTokenCount": resp.prompt_tokens,
                "candidatesTokenCount": resp.completion_tokens,
                "totalTokenCount": resp.total_tokens(),
            },
            "modelVersion": resp.model,
        }))
    }

    fn render_stream_event(&self, ev: &StreamEvent, _id: &str) -> Option<String> {
        let mut candidate = serde_json::Map::new();
        let mut content = serde_json::Map::new();
        content.insert(
            "role".to_string(),
            serde_json::Value::String("model".to_string()),
        );
        let mut parts = Vec::new();
        if !ev.delta.is_empty() {
            parts.push(serde_json::json!({ "text": ev.delta }));
        }
        parts.extend(ev.tool_call_deltas.iter().map(|call| serde_json::json!({
            "functionCall": { "name": call.name.clone().unwrap_or_default(), "args": serde_json::from_str::<serde_json::Value>(call.arguments_delta.as_deref().unwrap_or("{}")).unwrap_or(serde_json::json!({})) }
        })));
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
