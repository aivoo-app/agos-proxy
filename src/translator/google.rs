//! Google (Gemini) native request/response translation.
//!
//! Translates between the OpenAI-compatible chat-completions format and
//! Google's `generateContent` API. Reference:
//! https://ai.google.dev/api/generate-content

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::ChatRequest;

/// Build the upstream URL for a Gemini request. The API key goes in the query
/// string, so the target's auth token is appended there.
pub fn build_url(target: &Target, stream: bool) -> String {
    let base = target.provider.base_url.trim_end_matches('/');
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

/// Build the header map for a Gemini request.
pub fn build_headers(target: &Target) -> BTreeMap<String, String> {
    let mut headers = target.provider.extra_headers.clone();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers
}

/// Map an OpenAI role onto Gemini's role vocabulary.
fn gemini_role(role: &str) -> &'static str {
    match role {
        "assistant" => "model",
        _ => "user",
    }
}

/// Translate an OpenAI-compatible chat request into a Gemini generateContent body.
pub fn translate_request(chat_req: &ChatRequest) -> serde_json::Value {
    let mut system_parts: Vec<serde_json::Value> = Vec::new();
    let mut contents = Vec::new();

    for msg in &chat_req.messages {
        if msg.role == "system" {
            system_parts.push(serde_json::json!({ "text": msg.content }));
            continue;
        }
        contents.push(serde_json::json!({
            "role": gemini_role(&msg.role),
            "parts": [{ "text": msg.content }],
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

/// Translate a Gemini generateContent response back into OpenAI-compatible format.
pub fn translate_response(resp: &serde_json::Value, model_id: &str) -> Result<serde_json::Value> {
    let candidates = resp
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .context("gemini response missing candidates")?;

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
        "id": resp.get("responseId").cloned().unwrap_or_else(|| serde_json::Value::String("gemini_agos".into())),
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
                name: "gemini".into(),
                description: None,
                base_url: "https://generativelanguage.googleapis.com".into(),
                auth_token: "g-key".into(),
                kind: ProviderKind::Google,
                extra_headers: BTreeMap::new(),
            },
            entry: RouteEntry {
                id: 1,
                route_id: 1,
                provider_id: 1,
                model_id: "gemini-2.0-flash".into(),
                priority: 1,
                weight: 1.0,
                status: ModelStatus::Healthy,
                capabilities: Default::default(),
            },
        }
    }

    #[test]
    fn url_includes_model_and_key() {
        let t = dummy_target();
        assert_eq!(
            build_url(&t, false),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=g-key"
        );
        assert!(build_url(&t, true).contains(":streamGenerateContent?alt=sse"));
    }

    #[test]
    fn request_maps_system_and_roles() {
        let chat = ChatRequest {
            model: "prog/route".into(),
            messages: vec![
                Message {
                    role: "system".into(),
                    content: "be terse".into(),
                },
                Message {
                    role: "user".into(),
                    content: "hi".into(),
                },
                Message {
                    role: "assistant".into(),
                    content: "hello".into(),
                },
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
    fn response_maps_back_to_openai_shape() {
        let gemini = serde_json::json!({
            "responseId": "abc",
            "candidates": [{
                "content": { "parts": [{ "text": "hello " }, { "text": "world" }] },
                "finishReason": "STOP",
            }],
            "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 7 },
        });
        let out = translate_response(&gemini, "gemini-2.0-flash").unwrap();
        assert_eq!(out["choices"][0]["message"]["content"], "hello world");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["total_tokens"], 12);
        assert_eq!(out["model"], "gemini-2.0-flash");
    }
}
