//! Outbound adapter for the OpenAI *Responses* API (`POST {base}/v1/responses`).
//!
//! Some models (notably free tiers) only serve the Responses
//! endpoint. Chat-completions traffic is translated on the way out: system
//! messages go to `instructions`, user/assistant turns go to `input`, sampling
//! parameters are passed through, and the reply's `output[]` items are walked
//! to recover the assistant text, which is reshaped into a standard
//! chat-completion response.
//!
//! v1 scope (see the build plan): streaming requests are served from the
//! non-streamed answer; tool calls, structured outputs, and vision requests
//! are rejected with an error so the router can fail over to a capable entry.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::normalize_base;
use crate::router::Target;
use crate::translator::{content_has_image, content_text, ChatRequest, Message};

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

/// Translate a chat-completions request into a Responses request.
///
/// System messages are extracted into the dedicated `instructions` field;
/// user/assistant turns are joined into the `input` string. `temperature`,
/// `top_p` and `max_tokens`/`max_completion_tokens` are forwarded
/// (`max_*` becomes `max_output_tokens`).
///
/// Returns an error if the request uses features not supported by this adapter
/// (tools, tool_choice, response_format, images), so the router can fail over
/// to a capable entry instead of silently dropping them.
pub fn translate_request(chat_req: &ChatRequest, model_id: &str) -> Result<serde_json::Value> {
    let mut instructions = String::new();
    let mut input = String::new();

    for message in &chat_req.messages {
        let text = message_text(message);
        if message.role == "system" {
            if !instructions.is_empty() {
                instructions.push('\n');
            }
            instructions.push_str(&text);
        } else {
            if !input.is_empty() {
                input.push('\n');
            }
            input.push_str(&message.role);
            input.push_str(": ");
            input.push_str(&text);
        }
    }

    // Streaming is served from the non-streamed answer by the handler;
    // we just ignore the stream flag and make a non-streaming request.
    let _ = chat_req.stream;
    let extra = &chat_req.extra;
    if extra.get("tools").is_some_and(|v| !v.is_null()) {
        bail!("{ADAPTER_CAPABILITY_SKIP}: tools/function calling not supported by openai_responses adapter; use a tools-capable upstream");
    }
    if extra.get("tool_choice").is_some_and(|v| !v.is_null()) {
        bail!("{ADAPTER_CAPABILITY_SKIP}: tool_choice not supported by openai_responses adapter; use a tools-capable upstream");
    }
    if extra.get("response_format").is_some_and(|v| !v.is_null()) {
        bail!("{ADAPTER_CAPABILITY_SKIP}: response_format (JSON mode) not supported by openai_responses adapter; use a json-mode-capable upstream");
    }
    // Check for images in any message content.
    for message in &chat_req.messages {
        if content_has_image(&message.content) {
            bail!("{ADAPTER_CAPABILITY_SKIP}: image (vision) input not supported by openai_responses adapter; use a vision-capable upstream");
        }
    }

    let mut body = serde_json::json!({
        "model": model_id,
        "input": input,
    });
    let obj = body.as_object_mut().expect("json object");
    if !instructions.is_empty() {
        obj.insert(
            "instructions".to_string(),
            serde_json::Value::String(instructions),
        );
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = extra.get(key) {
            obj.insert(key.to_string(), v.clone());
        }
    }
    // The newer `max_completion_tokens` wins when both are present.
    for key in ["max_completion_tokens", "max_tokens"] {
        if let Some(v) = extra.get(key).filter(|v| !v.is_null()) {
            obj.insert("max_output_tokens".to_string(), v.clone());
            break;
        }
    }
    Ok(body)
}

/// Extract the plain-text content of a message: a JSON string as-is, or the
/// concatenated `text` entries of an OpenAI parts array.
fn message_text(message: &Message) -> String {
    content_text(&message.content)
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
    for item in output {
        if item.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let Some(parts) = item.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for part in parts {
            let kind = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if kind == "output_text" || kind == "text" {
                if let Some(s) = part.get("text").and_then(|t| t.as_str()) {
                    text.push_str(s);
                }
            }
        }
    }
    if text.trim().is_empty() {
        bail!("responses output carried no assistant text");
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
    Ok(serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
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
            provider_id: 1,
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
        assert_eq!(body["input"], "user: hi");
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_output_tokens"], 128);
    }

    #[test]
    fn request_rejects_tools() {
        let err = translate_request(
            &chat(serde_json::json!({
                "tools": [{ "type": "function" }],
            })),
            "responses-model",
        )
        .expect_err("tools should be rejected");
        assert!(err.to_string().contains("tools"));
    }

    #[test]
    fn request_rejects_tool_choice() {
        let err = translate_request(
            &chat(serde_json::json!({
                "tool_choice": "auto",
            })),
            "responses-model",
        )
        .expect_err("tool_choice should be rejected");
        assert!(err.to_string().contains("tool_choice"));
    }

    #[test]
    fn request_rejects_response_format() {
        let err = translate_request(
            &chat(serde_json::json!({
                "response_format": { "type": "json_object" },
            })),
            "responses-model",
        )
        .expect_err("response_format should be rejected");
        assert!(err.to_string().contains("response_format"));
    }

    #[test]
    fn request_rejects_images() {
        let mut req = chat(serde_json::json!({}));
        // Add an image to the user message
        req.messages[1].content = serde_json::json!([
            { "type": "text", "text": "what is this" },
            { "type": "image_url", "image_url": { "url": "https://x/cat.png" } }
        ]);
        let err =
            translate_request(&req, "responses-model").expect_err("images should be rejected");
        assert!(err.to_string().contains("vision") || err.to_string().contains("image"));
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
