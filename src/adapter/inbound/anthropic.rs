//! Anthropic inbound adapter.
//!
//! Serves `/anthropic/v1/messages`. Parses an Anthropic messages request into
//! the canonical [`ChatRequest`] and renders canonical results back as
//! Anthropic `Message` responses and SSE stream events.

use crate::adapter::inbound::InboundAdapter;
use crate::adapter::ApiKind;
use crate::translator::{CanonicalResponse, ChatRequest, ContentPart, Message, StreamEvent};

/// The Anthropic native inbound surface.
pub struct AnthropicAdapter;

/// Collapse an Anthropic content value (a plain string or an array of blocks)
/// into a single canonical text string. Text blocks contribute their `text`;
/// everything else is dropped. Used for system prompts, which stay text-only.
fn anthropic_content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Collapse an Anthropic content value into the canonical message content.
///
/// Content without image blocks stays a plain text string, so text-only
/// requests serialize byte-identically through the pipeline. Content carrying
/// image blocks becomes the OpenAI parts array — the canonical image encoding
/// the outbound adapters translate back into their native shape:
///
/// - `source.type = "url"` → an `image_url` part referencing the same URL;
/// - `source.type = "base64"` → an `image_url` part holding the payload
///   re-encoded as a `data:<media_type>;base64,<data>` URL.
fn anthropic_content(content: &serde_json::Value) -> serde_json::Value {
    let has_media = content.as_array().is_some_and(|blocks| {
        blocks.iter().any(|b| {
            matches!(
                b.get("type").and_then(|t| t.as_str()),
                Some("image" | "document")
            )
        })
    });
    if !has_media {
        return serde_json::Value::String(anthropic_content_text(content));
    }

    let parts: Vec<serde_json::Value> = content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                    Some("image") => anthropic_image_part(b),
                    Some("document") => anthropic_document_part(b),
                    _ => {
                        let text = b.get("text").and_then(|t| t.as_str()).unwrap_or_default();
                        if text.is_empty() {
                            None
                        } else {
                            Some(serde_json::json!({ "type": "text", "text": text }))
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::Value::Array(parts)
}

/// Convert an Anthropic `image` block into the canonical `image_url` part.
/// Blocks with a source this proxy cannot re-express (`file`-based Files API
/// references, for instance) are skipped — same as they were before the
/// passthrough existed.
fn anthropic_image_part(block: &serde_json::Value) -> Option<serde_json::Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(|t| t.as_str())? {
        "url" => source.get("url").and_then(|u| u.as_str())?.to_string(),
        // Re-encode the inline payload as a data URL, the one canonical image
        // reference every outbound adapter understands.
        "base64" => {
            let media = source
                .get("media_type")
                .and_then(|m| m.as_str())
                .unwrap_or("image/png");
            let data = source
                .get("data")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            format!("data:{media};base64,{data}")
        }
        _ => return None,
    };
    Some(serde_json::json!({
        "type": "image_url",
        "image_url": { "url": url },
    }))
}
fn anthropic_document_part(block: &serde_json::Value) -> Option<serde_json::Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(|t| t.as_str())? {
        "url" => source.get("url").and_then(|v| v.as_str())?.to_string(),
        "base64" => format!(
            "data:{};base64,{}",
            source
                .get("media_type")
                .and_then(|v| v.as_str())
                .unwrap_or("application/pdf"),
            source
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
        ),
        _ => return None,
    };
    Some(serde_json::json!({ "type": "file", "file": { "file_url": url } }))
}

/// Map an Anthropic finish reason onto our canonical finish reason.
pub fn canonical_stop_reason(reason: &str) -> &'static str {
    match reason {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
}

/// Map our canonical finish reason back onto an Anthropic stop reason.
pub fn anthropic_stop_reason(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    }
}

impl InboundAdapter for AnthropicAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::Anthropic
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
        if let Some(sys) = body.get("system") {
            let text = anthropic_content_text(sys);
            if !text.is_empty() {
                messages.push(Message::text("system", text));
            }
        }
        if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
            for m in arr {
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let raw_content = m.get("content").cloned().unwrap_or(serde_json::Value::Null);
                let calls: Vec<_> = raw_content
                    .as_array()
                    .into_iter()
                    .flat_map(|blocks| blocks.iter())
                    .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                    .map(|block| serde_json::json!({
                        "id": block.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                        "type": "function",
                        "function": { "name": block.get("name").and_then(|v| v.as_str()).unwrap_or_default(), "arguments": block.get("input").map(|v| if v.is_string() { v.clone() } else { serde_json::Value::String(v.to_string()) }).unwrap_or_else(|| serde_json::Value::String("{}".into())) }
                    }))
                    .collect();
                let result = raw_content.as_array().and_then(|blocks| {
                    blocks.iter().find(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    })
                });
                if let Some(result) = result {
                    let mut msg = Message::new(
                        "tool",
                        result
                            .get("content")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                    msg.extra = serde_json::json!({ "tool_call_id": result.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or_default(), "name": calls.first().and_then(|v| v.pointer("/function/name").cloned()).unwrap_or_default() });
                    messages.push(msg);
                } else {
                    let mut msg = Message::new(role, anthropic_content(&raw_content));
                    if !calls.is_empty() {
                        msg.extra = serde_json::json!({ "tool_calls": calls });
                    }
                    messages.push(msg);
                }
            }
        }

        // Carry generation knobs forward after normalising native field names.
        let mut extra = body.clone();
        if let Some(obj) = extra.as_object_mut() {
            obj.remove("model");
            obj.remove("messages");
            obj.remove("system");
            obj.remove("stream");
            if let Some(v) = obj.remove("stop_sequences") {
                obj.insert("stop".to_string(), v);
            }
            if let Some(tools) = obj.remove("tools") {
                let converted: Vec<_> = tools.as_array().into_iter().flat_map(|tools| tools.iter()).map(|tool| serde_json::json!({
                    "type": "function",
                    "function": { "name": tool.get("name").cloned().unwrap_or_default(), "description": tool.get("description").cloned().unwrap_or_default(), "parameters": tool.get("input_schema").cloned().unwrap_or(serde_json::json!({ "type": "object", "properties": {} })) }
                })).collect();
                obj.insert("tools".into(), serde_json::Value::Array(converted));
            }
            if let Some(choice) = obj.remove("tool_choice") {
                let choice = match choice.get("type").and_then(|v| v.as_str()) {
                    Some("any") => serde_json::json!("required"),
                    Some("none") => serde_json::json!("none"),
                    Some("tool") => {
                        serde_json::json!({ "type": "function", "function": { "name": choice.get("name").cloned().unwrap_or_default() } })
                    }
                    _ => serde_json::json!("auto"),
                };
                obj.insert("tool_choice".into(), choice);
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
        if let Some(ContentPart::Audio { .. } | ContentPart::Video { .. }) = resp
            .media
            .iter()
            .find(|part| matches!(part, ContentPart::Audio { .. } | ContentPart::Video { .. }))
        {
            anyhow::bail!("Anthropic responses cannot represent audio/video output")
        }
        let mut content = if resp.text.is_empty() {
            Vec::new()
        } else {
            vec![serde_json::json!({ "type": "text", "text": resp.text })]
        };
        content.extend(resp.media.iter().filter_map(|part| match part {
            ContentPart::Text(text) => Some(serde_json::json!({ "type": "text", "text": text })),
            ContentPart::Image { url } => Some(
                serde_json::json!({ "type": "image", "source": { "type": "url", "url": url } }),
            ),
            ContentPart::File { url } => Some(
                serde_json::json!({ "type": "document", "source": { "type": "url", "url": url } }),
            ),
            ContentPart::Audio { .. } | ContentPart::Video { .. } => None,
        }));
        content.extend(resp.tool_calls.iter().map(|call| serde_json::json!({
            "type": "tool_use", "id": call.wire_id(), "name": call.name,
            "input": serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap_or(serde_json::json!({})),
        })));
        Ok(serde_json::json!({
            "id": resp.id,
            "type": "message",
            "role": "assistant",
            "model": resp.model,
            "content": content,
            "stop_reason": anthropic_stop_reason(&resp.finish_reason),
            "stop_sequence": null,
            "usage": {
                "input_tokens": resp.prompt_tokens,
                "output_tokens": resp.completion_tokens,
            },
        }))
    }

    fn render_stream_event(&self, ev: &StreamEvent, _id: &str) -> Option<String> {
        if ev.done {
            return Some("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
        }
        let mut frames = String::new();
        if !ev.delta.is_empty() {
            let payload = serde_json::json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": ev.delta } });
            frames.push_str(&format!("event: content_block_delta\ndata: {payload}\n\n"));
        }
        for call in &ev.tool_call_deltas {
            let index = call.index + 1;
            if call.id.is_some() || call.name.is_some() {
                let payload = serde_json::json!({ "type": "content_block_start", "index": index, "content_block": { "type": "tool_use", "id": call.id.clone().unwrap_or_default(), "name": call.name.clone().unwrap_or_default(), "input": {} } });
                frames.push_str(&format!("event: content_block_start\ndata: {payload}\n\n"));
            }
            if let Some(arguments) = &call.arguments_delta {
                let payload = serde_json::json!({ "type": "content_block_delta", "index": index, "delta": { "type": "input_json_delta", "partial_json": arguments } });
                frames.push_str(&format!("event: content_block_delta\ndata: {payload}\n\n"));
            }
        }
        if let Some(reason) = &ev.finish_reason {
            let payload = serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": anthropic_stop_reason(reason), "stop_sequence": null }, "usage": { "output_tokens": ev.completion_tokens.unwrap_or(0) } });
            frames.push_str(&format!("event: message_delta\ndata: {payload}\n\n"));
        }
        (!frames.is_empty()).then_some(frames)
    }

    fn stream_end_marker(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_blocks_become_canonical_image_url_parts() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this"},
                    {"type": "image", "source": {"type": "url", "url": "https://x/cat.png"}},
                ]
            }]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "https://x/cat.png"}}
            ])
        );
    }

    #[test]
    fn base64_image_blocks_become_data_urls() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {
                        "type": "base64", "media_type": "image/jpeg", "data": "QUJD"
                    }},
                    {"type": "text", "text": "describe"},
                ]
            }]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(
            req.messages[0].content,
            serde_json::json!([
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,QUJD"}},
                {"type": "text", "text": "describe"}
            ])
        );
    }

    #[test]
    fn text_only_content_stays_a_plain_string() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hi"}, {"type": "text", "text": "there"}]
            }]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(req.messages[0].content, serde_json::json!("hi\nthere"));
    }

    #[test]
    fn system_stays_text_only() {
        let body = serde_json::json!({
            "model": "m",
            "system": "be terse",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = AnthropicAdapter.parse_request(&body).unwrap();
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[0].content, serde_json::json!("be terse"));
    }
}
