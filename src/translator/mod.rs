//! Request/response translation between OpenAI-compatible format and the
//! provider's native format.
//!
//! The MVP supports OpenAI-compatible providers only, so the translator is
//! largely a passthrough that injects provider-specific headers and rewrites
//! the base URL. Anthropic and Google translators land in V1.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use reqwest::Client;

use crate::domain::ProviderKind;
use crate::router::Target;

/// An OpenAI-compatible chat completions request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Build the upstream request for a target, returning the full URL and headers.
pub fn build_upstream_request(
    target: &Target,
    chat_req: &ChatRequest,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)>
{
    let base = target.provider.base_url.trim_end_matches('/');
    let url = format!("{base}/v1/chat/completions");

    let mut headers = target.provider.extra_headers.clone();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", target.provider.auth_token),
    );
    headers.insert("Content-Type".to_string(), "application/json".to_string());

    // The body uses the model ID from the route entry, not the caller's model string.
    let mut body = serde_json::to_value(chat_req)?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(target.entry.model_id.clone()),
        );
    }

    Ok((url, headers, body))
}

/// Forward a non-streaming chat-completions request, returning the raw bytes.
pub async fn forward_non_streaming(
    client: &Client,
    target: &Target,
    chat_req: &ChatRequest,
) -> Result<Vec<u8>> {
    let (url, headers, body) = build_upstream_request(target, chat_req)?;
    let mut req = client.post(&url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let resp = req.json(&body).send().await.context("sending request to provider")?;
    let status = resp.status();
    let bytes = resp.bytes().await.context("reading provider response")?;
    if !status.is_success() {
        anyhow::bail!(
            "provider returned {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(bytes.to_vec())
}

/// Check whether a provider kind is supported by the current translator set.
pub fn is_supported(kind: ProviderKind) -> bool {
    matches!(kind, ProviderKind::OpenAICompatible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ModelStatus, Provider, RouteEntry};

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
        }
    }

    #[test]
    fn upstream_request_uses_route_model_and_auth_header() {
        let target = dummy_target();
        let chat_req = ChatRequest {
            model: "prog/my-route".into(),
            messages: vec![Message {
                role: "user".into(),
                content: "hi".into(),
            }],
            stream: false,
            extra: serde_json::Value::Null,
        };
        let (url, headers, body) = build_upstream_request(&target, &chat_req).unwrap();
        assert_eq!(url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(headers.get("Authorization").unwrap(), "Bearer sk-secret");
        assert_eq!(headers.get("X-Custom").unwrap(), "yes");
        assert_eq!(body["model"], "deepseek-v4-flash");
    }
}