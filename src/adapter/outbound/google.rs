//! Google (Google) native request/response translation.
//!
//! Translates between the OpenAI chat-completions format and
//! Google's `generateContent` API. Reference:
//! https://ai.google.dev/api/generate-content

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use super::normalize_base;
use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{
    content_parts, content_text, infer_image_mime, parse_data_url, ChatRequest, ContentPart,
};

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
fn google_parts(content: &serde_json::Value) -> Vec<serde_json::Value> {
    let has_image = content_parts(content)
        .iter()
        .any(|p| matches!(p, ContentPart::Image { .. }));
    if !has_image {
        return vec![serde_json::json!({ "text": content_text(content) })];
    }

    content_parts(content)
        .into_iter()
        .map(|p| match p {
            ContentPart::Text(text) => serde_json::json!({ "text": text }),
            ContentPart::Image { url } => image_part(&url),
        })
        .collect()
}

/// Map one canonical image onto a Google content part. A `data:` URL is split
/// into its MIME type and base64 payload; anything else is passed as a
/// `fileData` URI (which requires a publicly resolvable URL) with the MIME
/// type inferred from the file extension.
fn image_part(url: &str) -> serde_json::Value {
    if let Some(data) = parse_data_url(url) {
        return serde_json::json!({
            "inlineData": { "mimeType": data.mime, "data": data.data },
        });
    }
    serde_json::json!({
        "fileData": { "mimeType": infer_image_mime(url), "fileUri": url },
    })
}

/// Translate an OpenAI chat request into a Google generateContent body.
pub fn translate_request(chat_req: &ChatRequest) -> serde_json::Value {
    let mut system_parts: Vec<serde_json::Value> = Vec::new();
    let mut contents = Vec::new();

    for msg in &chat_req.messages {
        if msg.role == "system" {
            // systemInstruction is text-only; image parts there are dropped
            // (see [`google_parts`] for the message-level mapping).
            system_parts.push(serde_json::json!({ "text": content_text(&msg.content) }));
            continue;
        }
        contents.push(serde_json::json!({
            "role": google_role(&msg.role),
            "parts": google_parts(&msg.content),
        }));
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
                gen_cfg.insert(to.to_string(), v.clone());
            }
        }
        if let Some(v) = obj.get("stop") {
            gen_cfg.insert("stopSequences".to_string(), v.clone());
        }
        if let Some(n) = obj.get("max_tokens").and_then(|v| v.as_u64()) {
            gen_cfg.insert("maxOutputTokens".to_string(), serde_json::Value::from(n));
        }
    }
    if !gen_cfg.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(gen_cfg);
    }

    body
}

/// Translate a Google generateContent response back into OpenAI format.
pub fn translate_response(resp: &serde_json::Value, model_id: &str) -> Result<serde_json::Value> {
    let candidates = resp
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .context("google response missing candidates")?;

    let mut text = String::new();
    if let Some(parts) = candidates
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
    {
        for part in parts {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
            }
        }
    }

    let finish_reason = match candidates.get("finishReason").and_then(|v| v.as_str()) {
        Some("MAX_TOKENS") => "length",
        Some("SAFETY") | Some("RECITATION") => "content_filter",
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
            "message": { "role": "assistant", "content": text },
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
    let delta = v
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
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
    if delta.is_empty() && finish.is_none() && prompt.is_none() && completion.is_none() {
        return None;
    }
    let done = finish.is_some();
    Some(crate::translator::StreamEvent {
        delta,
        finish_reason: finish,
        prompt_tokens: prompt,
        completion_tokens: completion,
        done,
        ..Default::default()
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
        };
        let body = translate_request(&chat);
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
        };
        let body = translate_request(&chat);
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
        };
        let body = translate_request(&chat);
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
        };
        let body = translate_request(&chat);
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
