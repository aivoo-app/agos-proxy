//! Google (Google) native request/response translation.
//!
//! Translates between the OpenAI chat-completions format and
//! Google's `generateContent` API. Reference:
//! https://ai.google.dev/api/generate-content

use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, Context as _, Result};

use super::normalize_base;
use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{
    infer_media_mime, parse_content_parts, parse_data_url, ChatRequest, ContentPart,
};
pub const ADAPTER_CAPABILITY_SKIP: &str = "adapter capability skip";

/// Build the upstream URL for a Google request. The API key goes in the query
/// string, so the target's auth token is appended there.
pub fn build_url(target: &Target, stream: bool) -> String {
    let base = normalize_base(&target.provider.base_url);
    let model = target.entry.model_id.replace('/', "-");
    if stream {
        format!(
            "{base}/v1beta/models/{model}:streamGenerateContent?alt=sse&key={}",
            target.provider.auth_token
        )
    } else {
        format!(
            "{base}/v1beta/models/{model}:generateContent?key={}",
            target.provider.auth_token
        )
    }
}

/// Build the header map for a Google request.
pub fn build_headers(target: &Target) -> BTreeMap<String, String> {
    let mut headers = target.provider.extra_headers.clone();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers
}

/// Map an OpenAI role onto Google's role vocabulary.
fn google_role(role: &str) -> &'static str {
    match role {
        "assistant" => "model",
        _ => "user",
    }
}

/// Rebuild a Google `parts` array from the canonical message content.
///
/// Text-only content yields a single text part, so a text-only request
/// serializes byte-identically to the pre-vision behavior. Content carrying
/// image parts yields ordered parts: text parts keep their `text` field and
/// image parts become `fileData` (`http(s)://` references) or `inlineData`
/// (`data:` URLs).
fn google_parts(content: &serde_json::Value) -> Result<Vec<serde_json::Value>> {
    let parts = parse_content_parts(content)
        .map_err(|e| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: invalid content: {e}"))?;
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(match part {
            ContentPart::Text(text) => serde_json::json!({ "text": text }),
            ContentPart::Image { url } => {
                media_part(&ContentPart::Image { url: url.clone() }, &url)
            }
            ContentPart::Audio { url } => {
                media_part(&ContentPart::Audio { url: url.clone() }, &url)
            }
            ContentPart::Video { url } => {
                media_part(&ContentPart::Video { url: url.clone() }, &url)
            }
            ContentPart::File { url } => media_part(&ContentPart::File { url: url.clone() }, &url),
        });
    }
    if out.is_empty() {
        out.push(serde_json::json!({ "text": "" }));
    }
    Ok(out)
}

fn media_part(kind: &ContentPart, url: &str) -> serde_json::Value {
    if let Some(data) = parse_data_url(url) {
        return serde_json::json!({ "inlineData": { "mimeType": data.mime, "data": data.data } });
    }
    serde_json::json!({ "fileData": { "mimeType": infer_media_mime(kind, url), "fileUri": url } })
}

/// Translate an OpenAI chat request into a Google generateContent body.
pub fn translate_request(chat_req: &ChatRequest) -> Result<serde_json::Value> {
    let mut system_parts: Vec<serde_json::Value> = Vec::new();
    let mut contents = Vec::new();
    let mut tool_names = HashMap::<String, String>::new();
    for msg in &chat_req.messages {
        if let Some(calls) = msg.extra.get("tool_calls").and_then(|v| v.as_array()) {
            for call in calls {
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(|v| v.as_str()),
                    call.pointer("/function/name").and_then(|v| v.as_str()),
                ) {
                    tool_names.insert(id.to_string(), name.to_string());
                }
            }
        }
    }

    for msg in &chat_req.messages {
        if matches!(msg.role.as_str(), "system" | "developer") {
            let parts = parse_content_parts(&msg.content)
                .map_err(|e| anyhow::anyhow!("{ADAPTER_CAPABILITY_SKIP}: invalid content: {e}"))?;
            if parts
                .iter()
                .any(|part| !matches!(part, ContentPart::Text(_)))
            {
                bail!("{ADAPTER_CAPABILITY_SKIP}: Google systemInstruction is text-only");
            }
            system_parts.push(serde_json::json!({ "text": parts.into_iter().filter_map(|part| match part { ContentPart::Text(t) => Some(t), _ => None }).collect::<Vec<_>>().join("\n") }));
            continue;
        }
        if msg.role == "tool" {
            let call_id = msg
                .extra
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let name = msg
                .extra
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| tool_names.get(call_id).map(String::as_str))
                .unwrap_or("tool");
            let output = if msg.content.is_object() {
                msg.content.clone()
            } else {
                serde_json::json!({ "output": msg.content })
            };
            contents.push(serde_json::json!({ "role": "user", "parts": [{ "functionResponse": { "name": name, "response": output } }] }));
            continue;
        }
        let mut parts = google_parts(&msg.content)?;
        if let Some(calls) = msg.extra.get("tool_calls").and_then(|v| v.as_array()) {
            if parts.len() == 1 && parts[0].get("text").and_then(|v| v.as_str()) == Some("") {
                parts.clear();
            }
            for call in calls {
                parts.push(serde_json::json!({ "functionCall": {
                    "name": call.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "args": serde_json::from_str::<serde_json::Value>(call.pointer("/function/arguments").and_then(|v| v.as_str()).unwrap_or("{}")).unwrap_or(serde_json::json!({})),
                }}));
            }
        }
        contents.push(serde_json::json!({ "role": google_role(&msg.role), "parts": parts }));
    }

    let mut body = serde_json::json!({ "contents": contents });
    if !system_parts.is_empty() {
        body["systemInstruction"] = serde_json::json!({ "parts": system_parts });
    }
    let mut gen_cfg = serde_json::Map::new();
    if let Some(obj) = chat_req.extra.as_object() {
        for (from, to) in [
            ("temperature", "temperature"),
            ("top_p", "topP"),
            ("top_k", "topK"),
        ] {
            if let Some(v) = obj.get(from) {
                gen_cfg.insert(to.into(), v.clone());
            }
        }
        if let Some(v) = obj.get("stop") {
            gen_cfg.insert("stopSequences".into(), v.clone());
        }
        for key in ["max_tokens", "max_completion_tokens"] {
            if let Some(n) = obj.get(key).and_then(|v| v.as_u64()) {
                gen_cfg.insert("maxOutputTokens".into(), serde_json::Value::from(n));
                break;
            }
        }
        if let Some(format) = obj.get("response_format").filter(|v| !v.is_null()) {
            gen_cfg.insert("responseMimeType".into(), "application/json".into());
            if let Some(schema) = format.pointer("/json_schema/schema") {
                gen_cfg.insert("responseSchema".into(), schema.clone());
            }
        }
    }
    if !gen_cfg.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(gen_cfg);
    }
    if let Some(tools) = chat_req
        .extra
        .get("tools")
        .and_then(|v| v.as_array())
        .filter(|v| !v.is_empty())
    {
        let declarations: Vec<_> = tools
            .iter()
            .map(|tool| {
                let f = tool.get("function").unwrap_or(tool);
                serde_json::json!({ "name": f.get("name").cloned().unwrap_or_default(), "description": f.get("description").cloned().unwrap_or_default(), "parameters": f.get("parameters").cloned().unwrap_or(serde_json::json!({ "type": "object", "properties": {} })) })
            })
            .collect();
        body["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);
        if let Some(choice) = chat_req.extra.get("tool_choice") {
            body["toolConfig"] =
                serde_json::json!({ "functionCallingConfig": google_tool_choice(choice) });
        }
    }
    Ok(body)
}

fn google_tool_choice(choice: &serde_json::Value) -> serde_json::Value {
    let mode = match choice.as_str().unwrap_or("auto") {
        "none" => "NONE",
        "required" => "ANY",
        _ => "AUTO",
    };
    let mut config = serde_json::json!({ "mode": mode });
    if let Some(name) = choice.pointer("/function/name") {
        config["allowedFunctionNames"] = serde_json::json!([name]);
    }
    config
}

/// Translate a Google generateContent response back into OpenAI format.
pub fn translate_response(resp: &serde_json::Value, model_id: &str) -> Result<serde_json::Value> {
    let candidates = resp
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .context("google response missing candidates")?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(parts) = candidates
        .pointer("/content/parts")
        .and_then(|p| p.as_array())
    {
        for part in parts {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
            }
            if let Some(call) = part.get("functionCall") {
                tool_calls.push(serde_json::json!({
                    "id": call.get("id").cloned().unwrap_or_default(),
                    "type": "function",
                    "function": { "name": call.get("name").cloned().unwrap_or_default(), "arguments": call.get("args").map(|v| if v.is_string() { v.clone() } else { serde_json::Value::String(v.to_string()) }).unwrap_or_else(|| serde_json::Value::String("{}".into())) }
                }));
            }
        }
    }
    if text.trim().is_empty() && tool_calls.is_empty() {
        bail!("google response carried no assistant text or function calls");
    }

    let finish_reason = match candidates.get("finishReason").and_then(|v| v.as_str()) {
        Some("MAX_TOKENS") => "length",
        Some("SAFETY") | Some("RECITATION") => "content_filter",
        _ if !tool_calls.is_empty() => "tool_calls",
        _ => "stop",
    };

    let usage = resp.get("usageMetadata");
    let prompt_tokens = usage
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    Ok(serde_json::json!({
        "id": resp.get("responseId").cloned().unwrap_or_else(|| serde_json::Value::String("google_agos".into())),
        "object": "chat.completion",
        "model": serde_json::Value::String(model_id.to_string()),
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text,
                "tool_calls": tool_calls,
            },
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    }))
}

/// Decode one Google `streamGenerateContent` SSE `data:` payload into a
/// [`StreamEvent`]. Each payload is a `GenerateContentResponse`; non-candidate
/// bookkeeping responses yield `None`.
pub fn parse_stream_chunk(data: &str) -> Option<crate::translator::StreamEvent> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let parts = v
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array());
    let delta = parts
        .iter()
        .flat_map(|parts| parts.iter())
        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
        .collect::<String>();
    let tool_call_deltas = parts
        .into_iter()
        .flat_map(|parts| parts.iter())
        .enumerate()
        .filter_map(|(index, part)| {
            let call = part.get("functionCall")?;
            Some(crate::translator::ToolCallDelta {
                index: index as u32,
                id: call.get("id").and_then(|v| v.as_str()).map(str::to_string),
                call_id: call.get("id").and_then(|v| v.as_str()).map(str::to_string),
                name: call
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                arguments_delta: call.get("args").map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap_or_default().to_string()
                    } else {
                        v.to_string()
                    }
                }),
            })
        })
        .collect::<Vec<_>>();
    let finish = v
        .pointer("/candidates/0/finishReason")
        .and_then(|f| f.as_str())
        .map(|r| match r {
            "MAX_TOKENS" => "length".to_string(),
            "STOP" | "STOP_SEQUENCE" => "stop".to_string(),
            other => other.to_lowercase(),
        });
    let prompt = v
        .pointer("/usageMetadata/promptTokenCount")
        .and_then(|t| t.as_u64());
    let completion = v
        .pointer("/usageMetadata/candidatesTokenCount")
        .and_then(|t| t.as_u64());
    // Drop pure-metric or empty bookkeeping chunks: only forward events that
    // carry text, a finish reason, or usage worth surfacing.
    if delta.is_empty()
        && tool_call_deltas.is_empty()
        && finish.is_none()
        && prompt.is_none()
        && completion.is_none()
    {
        return None;
    }
    let done = finish.is_some();
    Some(crate::translator::StreamEvent {
        delta,
        finish_reason: finish,
        prompt_tokens: prompt,
        completion_tokens: completion,
        tool_call_deltas,
        done,
    })
}

pub fn is_supported(_kind: ProviderKind) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ModelStatus, Provider, RouteEntry};
    use crate::translator::Message;

    fn dummy_target() -> Target {
        Target {
            provider: Provider {
                id: 1,
                profile_id: "p1".into(),
                name: "google".into(),
                description: None,
                base_url: "https://generativelanguage.googleapis.com".into(),
                auth_token: "g-key".into(),
                kind: ProviderKind::Google,
                extra_headers: BTreeMap::new(),
                masking_server_id: None,
                masking_server: None,
                shared: false,
            },
            entry: RouteEntry {
                id: 1,
                route_id: 1,
                provider_id: Some(1),
                target_route_id: None,
                model_id: "google-2.0-flash".into(),
                priority: 1,
                weight: 1.0,
                status: ModelStatus::Healthy,
                capabilities: Default::default(),
                price_per_1m: 0.4,
                cooldown_until: 0,
            },
            identity: None,
            prompt_cache: Default::default(),
        }
    }

    #[test]
    fn url_includes_model_and_key() {
        let t = dummy_target();
        assert_eq!(
            build_url(&t, false),
            "https://generativelanguage.googleapis.com/v1beta/models/google-2.0-flash:generateContent?key=g-key"
        );
        assert!(build_url(&t, true).contains(":streamGenerateContent?alt=sse"));
    }

    #[test]
    fn request_maps_system_and_roles() {
        let chat = ChatRequest {
            model: "prog/route".into(),
            messages: vec![
                Message::text("system", "be terse"),
                Message::text("user", "hi"),
                Message::text("assistant", "hello"),
            ],
            stream: false,
            extra: serde_json::json!({ "temperature": 0.5, "max_tokens": 128 }),
            request_id: None,
        };
        let body = translate_request(&chat).expect("translate");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be terse");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 128);
    }

    #[test]
    fn text_only_request_keeps_a_single_text_part() {
        let chat = ChatRequest {
            model: "prog/route".into(),
            messages: vec![Message::text("user", "hi")],
            stream: false,
            extra: serde_json::Value::Null,
            request_id: None,
        };
        let body = translate_request(&chat).expect("translate");
        assert_eq!(
            body["contents"][0]["parts"],
            serde_json::json!([{ "text": "hi" }])
        );
    }

    #[test]
    fn image_url_parts_map_to_file_data() {
        let chat = ChatRequest {
            model: "prog/route".into(),
            messages: vec![Message::new(
                "user",
                serde_json::json!([
                    {"type": "text", "text": "what is this"},
                    {"type": "image_url", "image_url": {"url": "https://x/cat.png"}},
                ]),
            )],
            stream: false,
            extra: serde_json::Value::Null,
            request_id: None,
        };
        let body = translate_request(&chat).expect("translate");
        let parts = &body["contents"][0]["parts"];
        assert_eq!(parts[0]["text"], "what is this");
        assert_eq!(parts[1]["fileData"]["fileUri"], "https://x/cat.png");
        assert_eq!(parts[1]["fileData"]["mimeType"], "image/png");
        assert!(parts[1].get("inlineData").is_none());
    }

    #[test]
    fn data_url_image_maps_to_inline_data() {
        let chat = ChatRequest {
            model: "prog/route".into(),
            messages: vec![Message::new(
                "user",
                serde_json::json!([
                    {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,QUJD"}},
                    {"type": "text", "text": "describe"},
                ]),
            )],
            stream: false,
            extra: serde_json::Value::Null,
            request_id: None,
        };
        let body = translate_request(&chat).expect("translate");
        let parts = &body["contents"][0]["parts"];
        assert_eq!(parts[0]["inlineData"]["mimeType"], "image/jpeg");
        assert_eq!(parts[0]["inlineData"]["data"], "QUJD");
        assert_eq!(parts[1]["text"], "describe");
    }

    #[test]
    fn response_maps_back_to_openai_shape() {
        let google = serde_json::json!({
            "responseId": "abc",
            "candidates": [{
                "content": { "parts": [{ "text": "hello " }, { "text": "world" }] },
                "finishReason": "STOP",
            }],
            "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 7 },
        });
        let out = translate_response(&google, "google-2.0-flash").unwrap();
        assert_eq!(out["choices"][0]["message"]["content"], "hello world");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["total_tokens"], 12);
        assert_eq!(out["model"], "google-2.0-flash");
    }
    #[test]
    fn tool_definitions_and_history_map_to_native_google_parts() {
        let mut assistant = Message::new("assistant", serde_json::Value::Null);
        assistant.extra = serde_json::json!({ "tool_calls": [{
            "id": "call_1", "type": "function",
            "function": { "name": "shell", "arguments": r#"{"cmd":"ls"}"# }
        }] });
        let mut tool_result = Message::text("tool", "a.txt");
        tool_result.extra = serde_json::json!({ "tool_call_id": "call_1", "name": "shell" });
        let req = ChatRequest {
            model: "prog/route".into(),
            messages: vec![Message::text("user", "run it"), assistant, tool_result],
            stream: false,
            extra: serde_json::json!({ "tools": [{ "type": "function", "function": { "name": "shell", "parameters": { "type": "object" } } }] }),
            request_id: None,
        };
        let body = translate_request(&req).expect("translate");
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "shell");
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["name"],
            "shell"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "shell"
        );
    }
}

#[cfg(test)]
mod stream_tests {
    use super::parse_stream_chunk;

    #[test]
    fn decodes_text_chunks_and_finish() {
        let ev = parse_stream_chunk(
            r#"{"candidates":[{"content":{"parts":[{"text":"Hel"},{"text":"lo"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":6}}"#,
        )
        .unwrap();
        assert_eq!(ev.delta, "Hello");
        assert_eq!(ev.finish_reason.as_deref(), Some("stop"));
        assert_eq!(ev.prompt_tokens, Some(4));
        assert_eq!(ev.completion_tokens, Some(6));
        assert!(ev.done);
    }

    #[test]
    fn decodes_function_call_parts() {
        let ev = parse_stream_chunk(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"shell","args":{"cmd":"ls"}}}]}}]}"#,
        ).unwrap();
        assert_eq!(ev.tool_call_deltas[0].name.as_deref(), Some("shell"));
        assert_eq!(
            ev.tool_call_deltas[0].arguments_delta.as_deref(),
            Some("{\"cmd\":\"ls\"}")
        );
    }

    #[test]
    fn maps_max_tokens_to_length() {
        let ev = parse_stream_chunk(
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"MAX_TOKENS"}]}"#,
        )
        .unwrap();
        assert_eq!(ev.finish_reason.as_deref(), Some("length"));
    }

    #[test]
    fn ignores_non_candidates() {
        assert!(parse_stream_chunk(r#"{}"#).is_none());
        assert!(parse_stream_chunk("keep-alive").is_none());
    }
}
