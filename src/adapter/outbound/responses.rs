//! Outbound adapter for the OpenAI *Responses* API (`POST {base}/v1/responses`).
//!
//! Some models (notably free tiers) only serve the Responses
//! endpoint. Chat-completions traffic is translated on the way out: system
//! messages go to `instructions`, user/assistant turns go to `input`, sampling
//! parameters are passed through, and the reply's `output[]` items are walked
//! to recover the assistant text, which is reshaped into a standard
//! chat-completion response.
//!
//! The adapter translates ordered messages, tool definitions/history, media,
//! structured output, and Responses-native fields without flattening them into
//! text. Responses-only destinations reject features they cannot represent so
//! the router can fail over to a compatible entry.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::normalize_base;
use crate::router::Target;
use crate::translator::{parse_content_parts, parse_data_url, ChatRequest, ContentPart, Message};

/// Marker for adapter-side capability rejections (tools/vision/JSON/stream).
/// `execute_with_failover`/`demote_status_for` treat these as client-side
/// skips: fail over without demoting the entry, since the request — not the
/// upstream — is at fault.
pub const ADAPTER_CAPABILITY_SKIP: &str = "adapter capability skip";

/// Upstream URL: `{base}/v1/responses` (the base may already end in `/v1`).
pub fn build_url(target: &Target) -> String {
    format!("{}/v1/responses", normalize_base(&target.provider.base_url))
}

/// Bearer auth plus any operator-configured extra headers.
pub fn build_headers(target: &Target) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", target.provider.auth_token),
    );
    for (k, v) in &target.provider.extra_headers {
        headers.insert(k.clone(), v.clone());
    }
    headers
}

/// Translate the canonical request into an ordered Responses input list.
/// Tool definitions, tool history, media, structured output, and roleplay are
/// mapped to native Responses values; no user turn is flattened into text.
pub fn translate_request(chat_req: &ChatRequest, model_id: &str) -> Result<serde_json::Value> {
    let extra = &chat_req.extra;
    let reserved = extra.get("agos_responses");
    let mut instructions = Vec::<String>::new();
    let mut input = Vec::<serde_json::Value>::new();

    for message in &chat_req.messages {
        if matches!(message.role.as_str(), "system" | "developer") {
            let text = plain_text(message)?;
            if !text.trim().is_empty() {
                instructions.push(text);
            }
            continue;
        }
        if message.role == "tool" {
            input.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": message.extra.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or_default(),
                "output": message.content,
            }));
            continue;
        }
        let calls = message.extra.get("tool_calls").and_then(|v| v.as_array());
        if !message.content.is_null() {
            input.push(serde_json::json!({
                "type": "message",
                "role": message.role,
                "content": responses_content(message)?,
            }));
        }
        if let Some(calls) = calls {
            for call in calls {
                input.push(responses_function_call(call)?);
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model_id,
        "input": input,
        "stream": false,
    });
    let obj = body.as_object_mut().expect("JSON object");
    if !instructions.is_empty() {
        obj.insert("instructions".into(), instructions.join("\n\n").into());
    }
    if let Some(tools) = tools_for_responses(reserved, extra)? {
        obj.insert("tools".into(), tools);
    }
    for key in [
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "top_logprobs",
        "truncation",
        "user",
        "service_tier",
    ] {
        if let Some(value) = extra.get(key).filter(|v| !v.is_null()) {
            obj.insert(key.into(), value.clone());
        }
    }
    for key in ["max_completion_tokens", "max_tokens"] {
        if let Some(value) = extra.get(key).filter(|v| !v.is_null()) {
            obj.insert("max_output_tokens".into(), value.clone());
            break;
        }
    }
    if let Some(format) = extra.get("response_format").filter(|v| !v.is_null()) {
        obj.insert(
            "text".into(),
            serde_json::json!({ "format": responses_text_format(format) }),
        );
    }
    // Responses-native fields collected from a Responses client are restored
    // only when the selected destination actually speaks Responses.
    if let Some(fields) = reserved.and_then(|v| v.as_object()) {
        for (key, value) in fields {
            if matches!(key.as_str(), "tools" | "max_output_tokens" | "text") {
                continue;
            }
            obj.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    Ok(body)
}

fn plain_text(message: &Message) -> Result<String> {
    let parts = parse_content_parts(&message.content)
        .map_err(|e| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: invalid content: {e}"))?;
    if parts
        .iter()
        .any(|part| !matches!(part, ContentPart::Text(_)))
    {
        bail!("{ADAPTER_CAPABILITY_SKIP}: Responses instructions/system content must be text-only");
    }
    Ok(parts
        .into_iter()
        .filter_map(|part| match part {
            ContentPart::Text(t) => Some(t),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn responses_content(message: &Message) -> Result<serde_json::Value> {
    match parse_content_parts(&message.content)
        .map_err(|e| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: invalid content: {e}"))?
    {
        parts if parts.len() == 1 && matches!(parts[0], ContentPart::Text(_)) => Ok(
            serde_json::Value::String(match parts.into_iter().next().unwrap() {
                ContentPart::Text(t) => t,
                _ => unreachable!(),
            }),
        ),
        parts => {
            let output: Vec<serde_json::Value> = parts
                .into_iter()
                .map(responses_content_part)
                .collect::<Result<_>>()?;
            Ok(serde_json::Value::Array(output))
        }
    }
}

fn responses_content_part(part: ContentPart) -> Result<serde_json::Value> {
    match part {
        ContentPart::Text(text) => Ok(serde_json::json!({ "type": "input_text", "text": text })),
        ContentPart::Image { url } => {
            Ok(serde_json::json!({ "type": "input_image", "image_url": url }))
        }
        ContentPart::Audio { url } => {
            let data = parse_data_url(&url)
                .filter(|d| d.mime.starts_with("audio/"))
                .ok_or_else(|| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: Responses audio input requires inline base64 audio"))?;
            let format = data.mime.trim_start_matches("audio/");
            Ok(
                serde_json::json!({ "type": "input_audio", "input_audio": { "data": data.data, "format": format } }),
            )
        }
        ContentPart::Video { .. } => {
            bail!("{ADAPTER_CAPABILITY_SKIP}: the Responses API has no standard video content part")
        }
        ContentPart::File { url } => {
            let value = if let Some(data) = parse_data_url(&url) {
                serde_json::json!({ "type": "input_file", "file_data": data.data })
            } else {
                serde_json::json!({ "type": "input_file", "file_url": url })
            };
            Ok(value)
        }
    }
}

fn responses_function_call(call: &serde_json::Value) -> Result<serde_json::Value> {
    let name = call
        .pointer("/function/name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if name.is_empty() {
        bail!("tool call is missing function.name");
    }
    let call_id = call.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    Ok(serde_json::json!({
        "type": "function_call",
        "id": call_id,
        "call_id": call_id,
        "name": name,
        "arguments": call.pointer("/function/arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
        "status": "completed",
    }))
}

fn tools_for_responses(
    reserved: Option<&serde_json::Value>,
    extra: &serde_json::Value,
) -> Result<Option<serde_json::Value>> {
    if let Some(tools) = reserved.and_then(|v| v.get("tools")) {
        return Ok(Some(tools.clone()));
    }
    let Some(tools) = extra.get("tools").and_then(|v| v.as_array()) else {
        return Ok(None);
    };
    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        match tool.get("type").and_then(|v| v.as_str()) {
            Some("function") => {
                let function = tool
                    .get("function")
                    .ok_or_else(|| anyhow::anyhow!("function tool is missing `function`"))?;
                converted.push(serde_json::json!({
                    "type": "function",
                    "name": function.get("name").cloned().unwrap_or_default(),
                    "description": function.get("description").cloned().unwrap_or_default(),
                    "parameters": function.get("parameters").cloned().unwrap_or(serde_json::json!({ "type": "object", "properties": {} })),
                    "strict": function.get("strict").cloned().unwrap_or(serde_json::Value::Bool(false)),
                }));
            }
            Some(other) => bail!("tool type {other:?} has no Responses function mapping"),
            None => bail!("tool is missing a string `type`"),
        }
    }
    Ok(Some(serde_json::Value::Array(converted)))
}

fn responses_text_format(format: &serde_json::Value) -> serde_json::Value {
    match format.get("type").and_then(|v| v.as_str()) {
        Some("json_schema") => {
            let schema = format.get("json_schema").cloned().unwrap_or_default();
            serde_json::json!({
                "type": "json_schema",
                "name": schema.get("name").cloned().unwrap_or_default(),
                "description": schema.get("description").cloned(),
                "schema": schema.get("schema").cloned().unwrap_or_default(),
                "strict": schema.get("strict").cloned().unwrap_or(serde_json::Value::Bool(false)),
            })
        }
        _ => serde_json::json!({ "type": "json_object" }),
    }
}

/// Walk a Responses reply and reshape it into a chat-completion response.
///
/// Only `type == "message"` output items contribute text, taken from their
/// `content[]` entries typed `output_text`/`text`. Token usage is reported
/// only when the upstream actually sent counts (`input_tokens` /
/// `output_tokens`) — nothing is fabricated. A reply with no assistant text
/// is an error so the router fails over instead of returning an empty answer.
pub fn translate_response(resp: &serde_json::Value, model_id: &str) -> Result<serde_json::Value> {
    let output = resp
        .get("output")
        .and_then(|o| o.as_array())
        .context("responses body missing `output` array")?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for item in output {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for part in parts {
                        let kind = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if matches!(kind, "output_text" | "text" | "refusal") {
                            if let Some(value) = part.get("text").and_then(|t| t.as_str()) {
                                text.push_str(value);
                            }
                        }
                    }
                }
            }
            Some("function_call" | "custom_tool_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let arguments =
                    if item.get("type").and_then(|v| v.as_str()) == Some("custom_tool_call") {
                        item.get("input")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string()
                    } else {
                        item.get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string()
                    };
                tool_calls.push(serde_json::json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": item.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                        "arguments": arguments,
                    }
                }));
            }
            _ => {}
        }
    }
    if text.trim().is_empty() && tool_calls.is_empty() {
        bail!("responses output carried no assistant text or tool calls");
    }

    let id = resp
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("responses")
        .to_string();
    let created = resp
        .get("created_at")
        .and_then(|c| c.as_i64())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let model = resp
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(model_id)
        .to_string();

    let usage = resp
        .get("usage")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut message = serde_json::json!({ "role": "assistant", "content": text });
    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::Value::Array(tool_calls.clone());
    }
    let finish_reason = if tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    Ok(serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Provider, RouteEntry};
    use std::collections::BTreeMap;

    fn target(base: &str, model: &str) -> Target {
        let provider = Provider {
            id: 1,
            profile_id: "p".into(),
            name: "example".into(),
            description: None,
            base_url: base.into(),
            auth_token: "sk-test".into(),
            kind: crate::domain::ProviderKind::OpenAIResponses,
            extra_headers: BTreeMap::new(),
            masking_server_id: None,
            masking_server: None,
            shared: false,
        };
        let entry = RouteEntry {
            id: 1,
            route_id: 1,
            provider_id: Some(1),
            target_route_id: None,
            model_id: model.into(),
            priority: 1,
            weight: 1.0,
            price_per_1m: 0.0,
            status: crate::domain::ModelStatus::Healthy,
            capabilities: Default::default(),
            cooldown_until: 0,
        };
        Target {
            provider,
            entry,
            identity: None,
            prompt_cache: Default::default(),
        }
    }

    fn chat(extra: serde_json::Value) -> ChatRequest {
        let mut raw = serde_json::json!({
            "model": "whatever",
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": "hi" }
            ],
            "stream": false,
        });
        // Sampling params ride on the top level; ChatRequest keeps them via
        // serde `flatten` into `extra`.
        for (k, v) in extra.as_object().expect("object") {
            raw[k] = v.clone();
        }
        serde_json::from_value(raw).expect("parse chat request")
    }

    #[test]
    fn url_appends_v1_responses_and_strips_trailing_v1() {
        assert_eq!(
            build_url(&target("https://api.example.test/", "m/x")),
            "https://api.example.test/v1/responses"
        );
        assert_eq!(
            build_url(&target("https://api.example.test/v1/", "m/x")),
            "https://api.example.test/v1/responses"
        );
    }

    #[test]
    fn request_joins_transcript_and_maps_params() {
        let body = translate_request(
            &chat(serde_json::json!({
                "temperature": 0.5,
                "max_tokens": 128,
            })),
            "responses-model",
        )
        .expect("translate");
        assert_eq!(body["model"], "responses-model");
        assert_eq!(body["instructions"], "be terse");
        assert_eq!(
            body["input"],
            serde_json::json!([{
                "type": "message", "role": "user", "content": "hi"
            }])
        );
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_output_tokens"], 128);
    }

    #[test]
    fn request_maps_tools_to_responses_function_tools() {
        let body = translate_request(
            &chat(serde_json::json!({
                "tools": [{ "type": "function", "function": { "name": "shell", "parameters": { "type": "object" } } }],
                "tool_choice": "required",
            })),
            "responses-model",
        )
        .expect("translate");
        assert_eq!(body["tools"][0]["name"], "shell");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn request_preserves_tool_choice() {
        let body = translate_request(
            &chat(serde_json::json!({ "tool_choice": "auto" })),
            "responses-model",
        )
        .expect("translate");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn request_maps_response_format_to_responses_text_format() {
        let body = translate_request(
            &chat(serde_json::json!({
                "response_format": { "type": "json_object" },
            })),
            "responses-model",
        )
        .expect("translate");
        assert_eq!(body["text"]["format"]["type"], "json_object");
    }

    #[test]
    fn request_maps_images_to_responses_input_image() {
        let mut req = chat(serde_json::json!({}));
        req.messages[1].content = serde_json::json!([
            { "type": "text", "text": "what is this" },
            { "type": "image_url", "image_url": { "url": "https://x/cat.png" } }
        ]);
        let body = translate_request(&req, "responses-model").expect("translate");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    }

    #[test]
    fn request_prefers_max_completion_tokens_over_max_tokens() {
        let body = translate_request(
            &chat(serde_json::json!({
                "max_tokens": 8,
                "max_completion_tokens": 64,
            })),
            "responses-model",
        )
        .expect("translate");
        assert_eq!(body["max_output_tokens"], 64);
    }

    #[test]
    fn response_extracts_assistant_text_from_output_items() {
        let resp = serde_json::json!({
            "id": "resp_1",
            "created_at": 1_700_000_000i64,
            "model": "responses-model",
            "output": [
                { "type": "reasoning", "summary": [] },
                {
                    "type": "message",
                    "content": [
                        { "type": "output_text", "text": "hello " },
                        { "type": "output_text", "text": "world" }
                    ]
                }
            ]
        });
        let out = translate_response(&resp, "responses-model").expect("translate");
        assert_eq!(out["object"], "chat.completion");
        assert_eq!(out["choices"][0]["message"]["content"], "hello world");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["created"], 1_700_000_000i64);
        // No usage upstream -> omitted rather than fabricated.
        assert!(out["usage"].is_null());
    }

    #[test]
    fn response_maps_usage_counts_only_when_present() {
        let mut resp = serde_json::json!({
            "id": "resp_2",
            "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "hi" }] }],
            "usage": { "input_tokens": 11, "output_tokens": 7 }
        });
        let out = translate_response(&resp, "m").expect("translate");
        assert_eq!(out["usage"]["input_tokens"], 11);
        assert_eq!(out["usage"]["output_tokens"], 7);

        resp["usage"] = serde_json::json!({});
        let out = translate_response(&resp, "m").expect("translate");
        assert_eq!(out["usage"], serde_json::json!({}));
    }

    #[test]
    fn reasoning_only_output_fails_clean() {
        let resp = serde_json::json!({
            "id": "resp_3",
            "output": [
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking" }] },
                { "type": "message", "content": [{ "type": "output_text", "text": "   " }] }
            ]
        });
        let err = translate_response(&resp, "m").expect_err("must fail");
        assert!(err.to_string().contains("no assistant text"));
    }

    #[test]
    fn malformed_body_is_an_error() {
        let resp = serde_json::json!({ "id": "resp_4" });
        assert!(translate_response(&resp, "m").is_err());
    }
}
