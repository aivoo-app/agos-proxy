//! OpenAI-compatible outbound adapter.
//!
//! OpenAI-compatible providers (including `custom` kinds, which supply their
//! own base URL) pass requests through untouched: the canonical
//! [`ChatRequest`] *is* the OpenAI wire format, so the outbound side only
//! stamps the route's model id, auth headers, and strips pipeline-internal
//! bookkeeping keys the upstream must not see. This module also owns the
//! OpenAI-shaped decoders ([`parse_canonical`], [`parse_stream_chunk`]) used
//! to reduce any upstream response into canonical form — including
//! `tool_calls`, which are preserved so an agentic caller can act on them.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use reqwest::Client;

use super::normalize_base;
use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{
    CanonicalResponse, ChatRequest, CompletionRequest, EmbeddingRequest, StreamEvent, ToolCall,
    ToolCallDelta,
};

/// Body keys the pipeline adds for its own bookkeeping and must never forward
/// upstream. `agos_responses` holds the Responses-API-only request fields the
/// Codex surface collected (see [`crate::adapter::inbound::responses`]); a
/// chat-completions provider would reject them as unknown parameters, so they
/// are stripped here. `economy_escalate` is the proxy-local escalation flag.
const INTERNAL_BODY_KEYS: [&str; 2] = ["agos_responses", "economy_escalate"];

/// Remove pipeline-internal keys from an outbound body.
fn strip_internal_keys(body: &mut serde_json::Value) {
    if let Some(obj) = body.as_object_mut() {
        for key in INTERNAL_BODY_KEYS {
            obj.remove(key);
        }
    }
}

/// Build the upstream request for an OpenAI-compatible chat target.
pub fn build_upstream_request(
    target: &Target,
    chat_req: &ChatRequest,
    stream: bool,
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
    strip_internal_keys(&mut body);
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(target.entry.model_id.clone()),
        );
        // Ask streaming upstreams for the usage frame so streamed traffic gets
        // real token counts in the usage log. Providers that don't support the
        // field fail the attempt and fail over, same as any other 4xx.
        if stream && !obj.contains_key("stream_options") {
            obj.insert(
                "stream_options".to_string(),
                serde_json::json!({ "include_usage": true }),
            );
        }
    }
    Ok((url, headers, body))
}

/// Coerce a `function.arguments` value into the raw JSON *string* the wire
/// format specifies. OpenAI sends a string; some compatible providers send an
/// already-parsed object, which is re-serialized so downstream code can always
/// treat the value as a string.
fn arguments_to_string(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => "{}".to_string(),
        Some(other) => other.to_string(),
    }
}

/// Extract `choices[0].message.tool_calls` into canonical form.
///
/// A call that carries a single `id` (the chat-completions encoding) sets both
/// [`ToolCall::id`] and [`ToolCall::call_id`] to that id, because that is the
/// value the client echoes back as `tool_call_id`.
fn parse_tool_calls(v: &serde_json::Value) -> Vec<ToolCall> {
    let Some(calls) = v
        .pointer("/choices/0/message/tool_calls")
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let name = call
                .pointer("/function/name")
                .and_then(|n| n.as_str())
                .filter(|n| !n.is_empty())?;
            let id = call
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string();
            Some(ToolCall {
                id: id.clone(),
                call_id: id,
                name: name.to_string(),
                arguments: arguments_to_string(call.pointer("/function/arguments")),
            })
        })
        .collect()
}

/// Extract `choices[0].delta.tool_calls` fragments. Providers stream one call's
/// id and name in the first fragment and its arguments across many, so the
/// caller accumulates these by [`ToolCallDelta::index`].
fn parse_stream_tool_calls(v: &serde_json::Value) -> Vec<ToolCallDelta> {
    let Some(calls) = v
        .pointer("/choices/0/delta/tool_calls")
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    calls
        .iter()
        .enumerate()
        .map(|(position, call)| {
            let index = call
                .get("index")
                .and_then(|i| i.as_u64())
                .map(|i| i as u32)
                .unwrap_or(position as u32);
            let id = call
                .get("id")
                .and_then(|i| i.as_str())
                .filter(|i| !i.is_empty())
                .map(str::to_string);
            ToolCallDelta {
                index,
                call_id: id.clone(),
                id,
                name: call
                    .pointer("/function/name")
                    .and_then(|n| n.as_str())
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
                arguments_delta: call
                    .pointer("/function/arguments")
                    .and_then(|a| a.as_str())
                    .map(str::to_string),
            }
        })
        .collect()
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
        tool_calls: parse_tool_calls(&v),
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
        tool_call_deltas: parse_stream_tool_calls(&v),
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
                name: "example".into(),
                description: None,
                base_url: "https://api.example.com".into(),
                auth_token: "sk-secret".into(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: extra,
            },
            entry: RouteEntry {
                id: 1,
                route_id: 1,
                provider_id: 1,
                model_id: "example-model".into(),
                priority: 1,
                weight: 1.0,
                status: ModelStatus::Healthy,
                capabilities: Default::default(),
                price_per_1m: 0.4,
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
        assert_eq!(url, "https://api.example.com/v1/chat/completions");
        assert_eq!(headers.get("Authorization").unwrap(), "Bearer sk-secret");
        assert_eq!(headers.get("X-Custom").unwrap(), "yes");
        assert_eq!(body["model"], "example-model");
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

    #[test]
    fn tool_calls_are_decoded_from_a_non_streaming_response() {
        let bytes = serde_json::json!({
            "id": "x", "model": "m",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" }
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": { "name": "apply_patch", "arguments": "{}" }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 5 }
        })
        .to_string()
        .into_bytes();

        let canon = parse_canonical(&bytes).unwrap();
        assert_eq!(canon.finish_reason, "tool_calls");
        assert_eq!(canon.text, "");
        assert_eq!(canon.tool_calls.len(), 2);
        // Chat-completions carries one id, used for both the item and the
        // correlation id so the client's `tool_call_id` matches.
        assert_eq!(canon.tool_calls[0].id, "call_1");
        assert_eq!(canon.tool_calls[0].call_id, "call_1");
        assert_eq!(canon.tool_calls[0].name, "shell");
        assert_eq!(canon.tool_calls[0].arguments, r#"{"cmd":"ls"}"#);
        assert_eq!(canon.tool_calls[1].name, "apply_patch");
    }

    #[test]
    fn tool_call_arguments_object_is_coerced_to_a_string() {
        // Some compatible providers send arguments already parsed; the wire
        // format says string, so it is re-serialized rather than dropped.
        let bytes = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "c", "function": { "name": "n", "arguments": { "a": 1 } }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string()
        .into_bytes();
        let canon = parse_canonical(&bytes).unwrap();
        assert_eq!(canon.tool_calls[0].arguments, r#"{"a":1}"#);
    }

    #[test]
    fn malformed_and_partial_tool_calls_are_skipped() {
        // A call with no function name is unusable; the others still decode.
        let bytes = serde_json::json!({
            "choices": [{
                "message": { "tool_calls": [
                    { "id": "a", "function": { "arguments": "{}" } },
                    { "function": { "name": "ok" } }
                ] },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string()
        .into_bytes();
        let canon = parse_canonical(&bytes).unwrap();
        assert_eq!(canon.tool_calls.len(), 1);
        assert_eq!(canon.tool_calls[0].name, "ok");
        assert_eq!(canon.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn text_only_response_has_no_tool_calls() {
        let bytes = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}]
        })
        .to_string()
        .into_bytes();
        assert!(parse_canonical(&bytes).unwrap().tool_calls.is_empty());
    }

    #[test]
    fn streaming_tool_call_fragments_are_decoded() {
        // The first fragment opens the call; later ones append arguments.
        let first = parse_stream_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"shell","arguments":""}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(first.tool_call_deltas.len(), 1);
        assert_eq!(first.tool_call_deltas[0].index, 0);
        assert_eq!(first.tool_call_deltas[0].id.as_deref(), Some("call_1"));
        assert_eq!(first.tool_call_deltas[0].name.as_deref(), Some("shell"));
        assert_eq!(
            first.tool_call_deltas[0].arguments_delta.as_deref(),
            Some("")
        );

        let second = parse_stream_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cmd\":"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(second.tool_call_deltas[0].name, None);
        assert_eq!(
            second.tool_call_deltas[0].arguments_delta.as_deref(),
            Some(r#"{"cmd":"#)
        );
        // Argument fragments do not terminate the stream on their own.
        assert!(!second.done);
    }

    #[test]
    fn internal_pipeline_keys_are_stripped_from_the_upstream_body() {
        // The Codex surface stashes Responses-only fields in the body under
        // `agos_responses`; a chat-completions provider must never see them.
        // `economy_escalate` is equally proxy-local (explicit flagship request).
        let target = dummy_target();
        let chat_req = ChatRequest {
            model: "prog/codex".into(),
            messages: vec![Message::text("user", "hi")],
            stream: true,
            extra: serde_json::json!({
                "tools": [{ "type": "function" }],
                "agos_responses": { "store": false, "include": ["reasoning.encrypted_content"] },
                "economy_escalate": true
            }),
        };
        let (_, _, body) = build_upstream_request(&target, &chat_req, true).unwrap();
        assert!(body.get("agos_responses").is_none());
        assert!(body.get("economy_escalate").is_none());
        assert!(body.get("tools").is_some());
        assert_eq!(body["model"], "example-model");
    }
}
