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

impl GoogleAdapter {
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
            let text = Self::parts_text(sys);
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
                let text = Self::parts_text(c.get("parts").unwrap_or(&serde_json::Value::Null));
                messages.push(Message::text(role, text));
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
