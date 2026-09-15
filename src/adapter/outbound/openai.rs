//! OpenAI-compatible outbound adapter.
//!
//! OpenAI-compatible providers (including `custom` kinds, which supply their
//! own base URL) pass requests through untouched: the canonical
//! [`ChatRequest`] *is* the OpenAI wire format, so the outbound side only
//! stamps the route's model id and auth headers. This module also owns the
//! OpenAI-shaped decoders ([`parse_canonical`], [`parse_stream_chunk`]) used
//! to reduce any upstream response into canonical form.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use reqwest::Client;

use super::normalize_base;
use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{
    CanonicalResponse, ChatRequest, CompletionRequest, EmbeddingRequest, StreamEvent,
};

/// Build the upstream request for an OpenAI-compatible chat target.
pub fn build_upstream_request(
    target: &Target,
    chat_req: &ChatRequest,
    _stream: bool,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)> {
    let base = normalize_base(&target.provider.base_url);
    let url = format!("{base}/v1/chat/completions");

    let mut headers = target.provider.extra_headers.clone();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", target.provider.auth_token),
    );
    headers.insert("Content-Type".to_string(), "application/json".to_string());

    // The body uses the model ID from the route entry, not the caller's
    // model string.
    let mut body = serde_json::to_value(chat_req)?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(target.entry.model_id.clone()),
        );
    }
    Ok((url, headers, body))
}

/// Parse an OpenAI-shaped chat completion response into the provider-
/// independent [`CanonicalResponse`] so a native inbound adapter can re-render
/// it in its own format.
pub fn parse_canonical(bytes: &[u8]) -> Result<CanonicalResponse> {
    let v: serde_json::Value = serde_json::from_slice(bytes).context("parsing response")?;
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let finish_reason = v
        .pointer("/choices/0/finish_reason")
        .and_then(|t| t.as_str())
        .unwrap_or("stop")
        .to_string();
    let usage = v.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    Ok(CanonicalResponse {
        id: v
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("chatcmpl-agos")
            .to_string(),
        model: v
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string(),
        text,
        finish_reason,
        prompt_tokens,
        completion_tokens,
        tool_calls: Vec::new(),
    })
}

/// Decode a single OpenAI-style SSE `data:` payload into a [`StreamEvent`].
/// The `[DONE]` sentinel maps to a terminal event; keep-alive/irrelevant lines
/// return `None`.
pub fn parse_stream_chunk(data: &str) -> Option<StreamEvent> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == ": keep-alive" {
        return None;
    }
    if trimmed == "[DONE]" {
        return Some(StreamEvent {
            delta: String::new(),
            finish_reason: Some("stop".to_string()),
            prompt_tokens: None,
            completion_tokens: None,
            done: true,
            ..Default::default()
        });
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let delta = v
        .pointer("/choices/0/delta/content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|f| f.as_str())
        .map(str::to_string);
    let prompt = v
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64());
    let completion = v
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64());
    let done = finish.is_some();
    Some(StreamEvent {
        delta,
        finish_reason: finish,
        prompt_tokens: prompt,
        completion_tokens: completion,
        done,
        ..Default::default()
    })
}

/// Build the upstream request for a completions target. OpenAI-compatible
/// providers get a straight passthrough with the route entry's model_id;
/// other provider kinds are rejected because they do not expose an
/// OpenAI-compatible completions endpoint.
pub fn build_completion_upstream_request(
    target: &Target,
    req: &CompletionRequest,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)> {
    match target.provider.kind {
        ProviderKind::OpenAICompatible | ProviderKind::Custom => {
            let base = target.provider.base_url.trim_end_matches('/');
            let url = format!("{base}/v1/completions");
            let mut headers = target.provider.extra_headers.clone();
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", target.provider.auth_token),
            );
            headers.insert("Content-Type".to_string(), "application/json".to_string());
            let mut body = serde_json::to_value(req)?;
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(target.entry.model_id.clone()),
                );
            }
            Ok((url, headers, body))
        }
        kind => Err(anyhow::anyhow!(
            "completions not supported for provider kind {:?}",
            kind
        )),
    }
}

/// Build the upstream request for an embeddings target. Same passthrough
/// approach as completions.
pub fn build_embedding_upstream_request(
    target: &Target,
    req: &EmbeddingRequest,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)> {
    match target.provider.kind {
        ProviderKind::OpenAICompatible | ProviderKind::Custom => {
            let base = target.provider.base_url.trim_end_matches('/');
            let url = format!("{base}/v1/embeddings");
            let mut headers = target.provider.extra_headers.clone();
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", target.provider.auth_token),
            );
            headers.insert("Content-Type".to_string(), "application/json".to_string());
            let mut body = serde_json::to_value(req)?;
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(target.entry.model_id.clone()),
                );
            }
            Ok((url, headers, body))
        }
        kind => Err(anyhow::anyhow!(
            "embeddings not supported for provider kind {:?}",
            kind
        )),
    }
}

/// Forward a non-streaming completions request to a target and return the
/// raw response bytes.
pub async fn forward_completion(
    client: &Client,
    target: &Target,
    req: &CompletionRequest,
) -> Result<Vec<u8>> {
    let (url, headers, body) = build_completion_upstream_request(target, req)?;
    let mut request = client.post(&url);
    for (k, v) in &headers {
        request = request.header(k, v);
    }
    let resp = request
        .json(&body)
        .send()
        .await
        .context("sending completion request to provider")?;
    let status = resp.status();
    let bytes = resp.bytes().await.context("reading provider response")?;
    if !status.is_success() {
        return Err(super::ProviderError {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .context("provider returned an error response");
    }
    Ok(bytes.to_vec())
}

/// Forward a non-streaming embeddings request to a target and return the
/// raw response bytes.
pub async fn forward_embedding(
    client: &Client,
    target: &Target,
    req: &EmbeddingRequest,
) -> Result<Vec<u8>> {
    let (url, headers, body) = build_embedding_upstream_request(target, req)?;
    let mut request = client.post(&url);
    for (k, v) in &headers {
        request = request.header(k, v);
    }
    let resp = request
        .json(&body)
        .send()
        .await
        .context("sending embedding request to provider")?;
    let status = resp.status();
    let bytes = resp.bytes().await.context("reading provider response")?;
    if !status.is_success() {
        return Err(super::ProviderError {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .context("provider returned an error response");
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ModelStatus, Provider, RouteEntry};
    use crate::translator::Message;

    fn dummy_target() -> Target {
        let mut extra = BTreeMap::new();
        extra.insert("X-Custom".into(), "yes".into());
        Target {
            provider: Provider {
                id: 1,
                profile_id: "p1".into(),
                name: "deepseek".into(),
                description: None,
                base_url: "https://api.deepseek.com".into(),
                auth_token: "sk-secret".into(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: extra,
            },
            entry: RouteEntry {
                id: 1,
                route_id: 1,
                provider_id: 1,
                model_id: "deepseek-v4-flash".into(),
                priority: 1,
                weight: 1.0,
                status: ModelStatus::Healthy,
                capabilities: Default::default(),
            },
            identity: None,
        }
    }

    #[test]
    fn upstream_request_uses_route_model_and_auth_header() {
        let target = dummy_target();
        let chat_req = ChatRequest {
            model: "prog/my-route".into(),
            messages: vec![Message::text("user", "hi")],
            stream: false,
            extra: serde_json::Value::Null,
        };
        let (url, headers, body) = build_upstream_request(&target, &chat_req, false).unwrap();
        assert_eq!(url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(headers.get("Authorization").unwrap(), "Bearer sk-secret");
        assert_eq!(headers.get("X-Custom").unwrap(), "yes");
        assert_eq!(body["model"], "deepseek-v4-flash");
    }

    #[test]
    fn canonical_parse_and_stream_decode() {
        let bytes = serde_json::json!({
            "id": "x", "model": "m",
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 5},
        })
        .to_string()
        .into_bytes();
        let canon = parse_canonical(&bytes).unwrap();
        assert_eq!(canon.text, "hi");
        assert_eq!(canon.total_tokens(), 8);

        let ev = parse_stream_chunk(r#"{"choices":[{"delta":{"content":"a"}}]}"#).unwrap();
        assert_eq!(ev.delta, "a");
        let done = parse_stream_chunk("[DONE]").unwrap();
        assert!(done.done);
        assert!(parse_stream_chunk(": keep-alive").is_none());
    }
}
