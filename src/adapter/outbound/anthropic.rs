//! Anthropic native request/response translation.
//!
//! Translates between the OpenAI chat-completions format and
//! Anthropic's `/v1/messages` API. Reference:
//! https://docs.anthropic.com/en/api/messages

use std::collections::BTreeMap;

use anyhow::{bail, Context as _, Result};

use super::normalize_base;
use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{parse_content_parts, parse_data_url, ChatRequest, ContentPart};

/// Marker used by the router to distinguish an adapter capability mismatch
/// from an upstream/provider failure.
pub const ADAPTER_CAPABILITY_SKIP: &str = "adapter capability skip";

/// Build the upstream URL for an Anthropic request.
pub fn build_url(target: &Target) -> String {
    let base = normalize_base(&target.provider.base_url);
    format!("{base}/v1/messages")
}

/// Build the header map for an Anthropic request.
pub fn build_headers(target: &Target) -> BTreeMap<String, String> {
    let mut headers = target.provider.extra_headers.clone();
    headers.insert("x-api-key".to_string(), target.provider.auth_token.clone());
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers
        .entry("anthropic-version".to_string())
        .or_insert_with(|| "2023-06-01".to_string());
    headers
}

/// Translate an OpenAI chat request into an Anthropic messages request.
pub fn translate_request(chat_req: &ChatRequest, model_id: &str) -> Result<serde_json::Value> {
    let mut system_text = String::new();
    let mut messages = Vec::new();
    for msg in &chat_req.messages {
        if matches!(msg.role.as_str(), "system" | "developer") {
            let parts = parse_content_parts(&msg.content)
                .map_err(|e| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: invalid content: {e}"))?;
            if parts
                .iter()
                .any(|part| !matches!(part, ContentPart::Text(_)))
            {
                bail!("{ADAPTER_CAPABILITY_SKIP}: Anthropic system prompts are text-only");
            }
            let text = parts
                .into_iter()
                .filter_map(|part| match part {
                    ContentPart::Text(t) => Some(t),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() {
                if !system_text.is_empty() {
                    system_text.push_str("\n\n");
                }
                system_text.push_str(&text);
            }
            continue;
        }

        if msg.role == "tool" {
            messages.push(serde_json::json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": msg.extra.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or_default(), "content": msg.content }],
            }));
            continue;
        }
        let role = if msg.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        let mut content = message_content(&msg.content)?;
        if let Some(calls) = msg.extra.get("tool_calls").and_then(|v| v.as_array()) {
            let mut blocks = match content {
                serde_json::Value::String(text) if !text.is_empty() => {
                    vec![serde_json::json!({ "type": "text", "text": text })]
                }
                serde_json::Value::Array(parts) => parts,
                _ => Vec::new(),
            };
            for call in calls {
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": call.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                    "name": call.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "input": serde_json::from_str::<serde_json::Value>(call.pointer("/function/arguments").and_then(|v| v.as_str()).unwrap_or("{}")).unwrap_or(serde_json::json!({})),
                }));
            }
            content = serde_json::Value::Array(blocks);
        }
        messages.push(serde_json::json!({ "role": role, "content": content }));
    }

    let mut body =
        serde_json::json!({ "model": model_id, "max_tokens": 4096, "messages": messages });
    if !system_text.is_empty() {
        body["system"] = serde_json::Value::String(system_text);
    }
    if let Some(obj) = chat_req.extra.as_object() {
        for (from, to) in [
            ("max_tokens", "max_tokens"),
            ("max_completion_tokens", "max_tokens"),
            ("temperature", "temperature"),
            ("top_p", "top_p"),
            ("top_k", "top_k"),
            ("stop", "stop_sequences"),
        ] {
            if let Some(value) = obj.get(from) {
                body[to] = value.clone();
            }
        }
        if let Some(tools) = obj
            .get("tools")
            .and_then(|v| v.as_array())
            .filter(|v| !v.is_empty())
        {
            body["tools"] = serde_json::Value::Array(tools.iter().map(|tool| {
                let f = tool.get("function").unwrap_or(tool);
                serde_json::json!({ "name": f.get("name").cloned().unwrap_or_default(), "description": f.get("description").cloned().unwrap_or_default(), "input_schema": f.get("parameters").cloned().unwrap_or(serde_json::json!({ "type": "object", "properties": {} })) })
            }).collect());
        }
        if let Some(choice) = obj.get("tool_choice") {
            body["tool_choice"] = anthropic_tool_choice(choice);
        }
        if let Some(parallel) = obj.get("parallel_tool_calls").and_then(|v| v.as_bool()) {
            body["tool_choice"]["disable_parallel_tool_use"] = serde_json::Value::Bool(!parallel);
        }
        if obj
            .get("response_format")
            .is_some_and(|value| !value.is_null())
        {
            bail!("{ADAPTER_CAPABILITY_SKIP}: Anthropic has no response_format field; use a Responses/JSON-native entry");
        }
    }
    if chat_req.stream {
        body["stream"] = serde_json::Value::Bool(true);
    }
    Ok(body)
}

/// Rebuild an Anthropic message `content` value from the canonical content.
///
/// Text-only content stays a plain JSON string, so a text-only request
/// serializes byte-identically to the pre-vision behavior. Content carrying
/// image parts becomes an ordered block array: text parts map to `text`
/// blocks and image parts map to `image` blocks — `source.type = "url"` for
/// `http(s)://` references and `source.type = "base64"` for `data:` URLs.
fn message_content(content: &serde_json::Value) -> Result<serde_json::Value> {
    let parts = parse_content_parts(content)
        .map_err(|e| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: invalid content: {e}"))?;
    if parts.len() == 1 {
        if let ContentPart::Text(text) = &parts[0] {
            return Ok(serde_json::Value::String(text.clone()));
        }
    }
    let mut blocks = Vec::new();
    for part in parts {
        blocks.push(match part {
            ContentPart::Text(text) => serde_json::json!({ "type": "text", "text": text }),
            ContentPart::Image { url } => image_block(&url),
            ContentPart::File { url } => document_block(&url),
            ContentPart::Audio { .. } | ContentPart::Video { .. } => {
                bail!("{ADAPTER_CAPABILITY_SKIP}: Anthropic messages do not accept audio/video content parts")
            }
        });
    }
    Ok(serde_json::Value::Array(blocks))
}

fn document_block(url: &str) -> serde_json::Value {
    if let Some(data) = parse_data_url(url) {
        serde_json::json!({ "type": "document", "source": { "type": "base64", "media_type": data.mime, "data": data.data } })
    } else {
        serde_json::json!({ "type": "document", "source": { "type": "url", "url": url } })
    }
}

/// Map one canonical image onto an Anthropic `image` block. A `data:` URL is
/// split into its MIME type and base64 payload; anything else is passed as a
/// URL reference for the provider to resolve (or reject — the failure
/// classifies the entry and fails over, see `demote_status_for`).
fn anthropic_tool_choice(choice: &serde_json::Value) -> serde_json::Value {
    if let Some(name) = choice.pointer("/function/name").and_then(|v| v.as_str()) {
        return serde_json::json!({ "type": "tool", "name": name });
    }
    match choice.as_str().unwrap_or("auto") {
        "none" => serde_json::json!({ "type": "none" }),
        "required" => serde_json::json!({ "type": "any" }),
        _ => serde_json::json!({ "type": "auto" }),
    }
}

fn image_block(url: &str) -> serde_json::Value {
    if let Some(data) = parse_data_url(url) {
        return serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": data.mime,
                "data": data.data,
            },
        });
    }
    serde_json::json!({
        "type": "image",
        "source": { "type": "url", "url": url },
    })
}

/// Translate an Anthropic messages response back into OpenAI format.
pub fn translate_response(resp: &serde_json::Value) -> Result<serde_json::Value> {
    let content = resp
        .get("content")
        .and_then(|c| c.as_array())
        .context("anthropic response missing content array")?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) { text.push_str(t); }
            }
            Some("tool_use") => tool_calls.push(serde_json::json!({
                "id": block.get("id").cloned().unwrap_or_default(),
                "type": "function",
                "function": {
                    "name": block.get("name").cloned().unwrap_or_default(),
                    "arguments": block.get("input").map(|v| if v.is_string() { v.clone() } else { serde_json::Value::String(v.to_string()) }).unwrap_or_else(|| "{}".into()),
                }
            })),
            _ => {}
        }
    }
    if text.trim().is_empty() && tool_calls.is_empty() {
        bail!("anthropic response carried no assistant text or tool calls");
    }

    let usage = resp.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let stop_reason = resp
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop");
    let finish_reason = match stop_reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "stop_sequence" => "stop",
        "tool_use" => "tool_calls",
        _ => "stop",
    };

    let mut message = serde_json::json!({ "role": "assistant", "content": text });
    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::Value::Array(tool_calls);
    }
    Ok(serde_json::json!({
        "id": resp.get("id").cloned().unwrap_or_else(|| serde_json::Value::String("msg_agos".into())),
        "object": "chat.completion",
        "model": resp.get("model").cloned().unwrap_or(serde_json::Value::Null),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    }))
}

/// Decode one Anthropic SSE `data:` payload into a [`StreamEvent`]. Handles
/// `content_block_delta` (text), `message_delta` (finish reason + usage) and
/// `message_stop` events; all other bookkeeping events yield `None`.
pub fn parse_stream_chunk(data: &str) -> Option<crate::translator::StreamEvent> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    match kind {
        "content_block_start"
            if v.pointer("/content_block/type").and_then(|v| v.as_str()) == Some("tool_use") =>
        {
            Some(crate::translator::StreamEvent {
                tool_call_deltas: vec![crate::translator::ToolCallDelta {
                    index: v.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    id: v
                        .pointer("/content_block/id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    call_id: v
                        .pointer("/content_block/id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    name: v
                        .pointer("/content_block/name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    arguments_delta: None,
                }],
                ..Default::default()
            })
        }
        "content_block_delta" => {
            let delta_type = v
                .pointer("/delta/type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if delta_type == "input_json_delta" {
                return Some(crate::translator::StreamEvent {
                    tool_call_deltas: vec![crate::translator::ToolCallDelta {
                        index: v.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                        arguments_delta: v
                            .pointer("/delta/partial_json")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        ..Default::default()
                    }],
                    ..Default::default()
                });
            }
            let delta = v
                .pointer("/delta/text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            Some(crate::translator::StreamEvent {
                delta,
                ..Default::default()
            })
        }
        "message_start" => {
            // The input token count arrives once, on the opening event.
            Some(crate::translator::StreamEvent {
                prompt_tokens: v
                    .pointer("/message/usage/input_tokens")
                    .and_then(|t| t.as_u64()),
                ..Default::default()
            })
        }
        "message_delta" => {
            let stop_reason = v
                .pointer("/delta/stop_reason")
                .and_then(|t| t.as_str())
                .map(|r| match r {
                    "max_tokens" => "length".to_string(),
                    "tool_use" => "tool_calls".to_string(),
                    _ => "stop".to_string(),
                });
            let completion = v.pointer("/usage/output_tokens").and_then(|t| t.as_u64());
            Some(crate::translator::StreamEvent {
                delta: String::new(),
                finish_reason: stop_reason,
                prompt_tokens: None,
                completion_tokens: completion,
                done: false,
                ..Default::default()
            })
        }
        "message_stop" => Some(crate::translator::StreamEvent {
            delta: String::new(),
            finish_reason: Some("stop".to_string()),
            prompt_tokens: None,
            completion_tokens: None,
            done: true,
            ..Default::default()
        }),
        _ => None,
    }
}

pub fn is_supported(_kind: ProviderKind) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::Message;

    fn chat(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "prog/route".into(),
            messages,
            stream: false,
            extra: serde_json::Value::Null,
            request_id: None,
        }
    }

    #[test]
    fn text_only_request_keeps_string_content() {
        // No image parts: content must remain a plain JSON string, exactly as
        // before the vision passthrough existed.
        let body = translate_request(
            &chat(vec![
                Message::text("system", "be terse"),
                Message::text("user", "hi"),
            ]),
            "model-id",
        )
        .expect("translate");
        assert_eq!(body["system"], "be terse");
        assert!(body["messages"][0]["content"].is_string());
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn image_url_parts_map_to_anthropic_blocks() {
        let body = translate_request(
            &chat(vec![Message::new(
                "user",
                serde_json::json!([
                    {"type": "text", "text": "what is this"},
                    {"type": "image_url", "image_url": {"url": "https://x/cat.png"}},
                ]),
            )]),
            "model-id",
        )
        .expect("translate");
        let content = &body["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is this");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "url");
        assert_eq!(content[1]["source"]["url"], "https://x/cat.png");
    }

    #[test]
    fn data_url_image_maps_to_a_base64_source_block() {
        let body = translate_request(
            &chat(vec![Message::new(
                "user",
                serde_json::json!([
                    {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,QUJD"}},
                    {"type": "text", "text": "describe"},
                ]),
            )]),
            "model-id",
        )
        .expect("translate");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[0]["source"]["data"], "QUJD");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "describe");
    }
    #[test]
    fn tool_definitions_and_history_map_to_native_anthropic_blocks() {
        let mut assistant = Message::new("assistant", serde_json::Value::Null);
        assistant.extra = serde_json::json!({ "tool_calls": [{
            "id": "call_1", "type": "function",
            "function": { "name": "shell", "arguments": r#"{"cmd":"ls"}"# }
        }] });
        let mut req = chat(vec![Message::text("user", "run it"), assistant]);
        req.extra = serde_json::json!({
            "tools": [{ "type": "function", "function": { "name": "shell", "parameters": { "type": "object" } } }],
            "tool_choice": "required"
        });
        let body = translate_request(&req, "claude").expect("translate");
        assert_eq!(body["tools"][0]["name"], "shell");
        assert_eq!(body["tool_choice"]["type"], "any");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    }

    #[test]
    fn response_tool_use_becomes_openai_tool_calls() {
        let response = serde_json::json!({
            "id": "msg_1", "content": [{ "type": "tool_use", "id": "call_1", "name": "shell", "input": { "cmd": "ls" } }],
            "stop_reason": "tool_use", "usage": { "input_tokens": 2, "output_tokens": 3 }
        });
        let body = translate_response(&response).expect("translate");
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "shell"
        );
    }
}

#[cfg(test)]
mod stream_tests {
    use super::parse_stream_chunk;

    #[test]
    fn decodes_content_block_delta() {
        let ev = parse_stream_chunk(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap();
        assert_eq!(ev.delta, "Hello");
        assert!(!ev.done);
        assert!(ev.finish_reason.is_none());
    }

    #[test]
    fn decodes_tool_use_start_and_argument_delta() {
        let start = parse_stream_chunk(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call_1","name":"shell"}}"#,
        ).unwrap();
        assert_eq!(start.tool_call_deltas[0].name.as_deref(), Some("shell"));
        let args = parse_stream_chunk(
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}"#,
        ).unwrap();
        assert_eq!(
            args.tool_call_deltas[0].arguments_delta.as_deref(),
            Some("{\"cmd\":")
        );
    }

    #[test]
    fn decodes_message_stop_as_terminal() {
        let ev = parse_stream_chunk(r#"{"type":"message_stop"}"#).unwrap();
        assert!(ev.done);
        assert_eq!(ev.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn maps_max_tokens_finish_to_length() {
        let ev = parse_stream_chunk(
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":300}}"#,
        )
        .unwrap();
        assert_eq!(ev.finish_reason.as_deref(), Some("length"));
        assert_eq!(ev.completion_tokens, Some(300));
    }

    #[test]
    fn ignores_bookkeeping_events() {
        assert!(parse_stream_chunk(r#"{"type":"ping"}"#).is_none());
        assert!(parse_stream_chunk("").is_none());
    }
}
