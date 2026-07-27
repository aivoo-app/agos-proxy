//! Request/response translation between OpenAI-compatible format and the
//! provider's native format.
//!
//! Callers always speak the OpenAI-compatible API; each provider kind gets a
//! translator that reshapes the request on the way out and the response on the
//! way back. OpenAI-compatible providers pass through untouched.

pub mod anthropic;
pub mod google;

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use reqwest::Client;

use crate::domain::ProviderKind;
use crate::router::Target;

/// Strip a trailing `/v1` segment (and any trailing slashes) from a provider
/// base URL. The URL builders append their own versioned path
/// (`/v1/chat/completions`, `/v1/messages`, `/v1beta/models/...`), so a base
/// URL copied from provider docs that already ends in `/v1` (e.g.
/// `https://openrouter.ai/api/v1`) would otherwise produce a doubled segment
/// like `/v1/v1/chat/completions` and 404 on every request and health probe.
pub fn normalize_base(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

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
    /// The message content, kept as raw JSON so the OpenAI-compatible
    /// passthrough stays lossless. Plain text is a JSON string; multimodal
    /// requests use the OpenAI parts array (text / image_url entries).
    pub content: serde_json::Value,
}

/// Extract the plain-text portion of a message content value. Multimodal
/// (array) content contributes its `text` parts; image parts are dropped.
pub fn content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Build the upstream request for a target: URL, headers, and a body already
/// shaped for the provider. `stream` selects the streaming endpoint variant.
pub fn build_upstream_request(
    target: &Target,
    chat_req: &ChatRequest,
    stream: bool,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)> {
    match target.provider.kind {
        ProviderKind::OpenAICompatible => {
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
        ProviderKind::Anthropic => {
            let url = anthropic::build_url(target);
            let headers = anthropic::build_headers(target);
            let body = anthropic::translate_request(chat_req, &target.entry.model_id);
            Ok((url, headers, body))
        }
        ProviderKind::Google => {
            let url = google::build_url(target, stream);
            let headers = google::build_headers(target);
            let body = google::translate_request(chat_req);
            Ok((url, headers, body))
        }
        ProviderKind::Custom => {
            // Custom providers are treated as OpenAI-compatible — the caller
            // supplies their own base URL and the request passes through as-is.
            let base = normalize_base(&target.provider.base_url);
            let url = format!("{base}/v1/chat/completions");
            let mut headers = target.provider.extra_headers.clone();
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", target.provider.auth_token),
            );
            headers.insert("Content-Type".to_string(), "application/json".to_string());
            let mut body = serde_json::to_value(chat_req)?;
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(target.entry.model_id.clone()),
                );
            }
            Ok((url, headers, body))
        }
    }
}

/// Reshape a successful upstream response into OpenAI-compatible JSON bytes.
/// OpenAI-compatible responses pass through as-is.
pub fn translate_response(target: &Target, bytes: &[u8]) -> Result<Vec<u8>> {
    match target.provider.kind {
        ProviderKind::OpenAICompatible => Ok(bytes.to_vec()),
        ProviderKind::Anthropic => {
            let resp: serde_json::Value =
                serde_json::from_slice(bytes).context("parsing anthropic response")?;
            let out = anthropic::translate_response(&resp)?;
            serde_json::to_vec(&out).context("serializing translated response")
        }
        ProviderKind::Google => {
            let resp: serde_json::Value =
                serde_json::from_slice(bytes).context("parsing gemini response")?;
            let out = google::translate_response(&resp, &target.entry.model_id)?;
            serde_json::to_vec(&out).context("serializing translated response")
        }
        ProviderKind::Custom => Ok(bytes.to_vec()),
    }
}

/// Forward a non-streaming chat-completions request, returning OpenAI-shaped
/// response bytes regardless of the provider's native format.
pub async fn forward_non_streaming(
    client: &Client,
    target: &Target,
    chat_req: &ChatRequest,
) -> Result<Vec<u8>> {
    let (url, headers, body) = build_upstream_request(target, chat_req, false)?;
    let mut req = client.post(&url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let resp = req
        .json(&body)
        .send()
        .await
        .context("sending request to provider")?;
    let status = resp.status();
    let bytes = resp.bytes().await.context("reading provider response")?;
    if !status.is_success() {
        return Err(ProviderError {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .context("provider returned an error response");
    }
    translate_response(target, &bytes)
}

/// An error response straight from the upstream provider, preserving its HTTP
/// status so usage logging can record what actually happened.
#[derive(Debug, thiserror::Error)]
#[error("provider returned {}: {}", status, body)]
pub struct ProviderError {
    pub status: reqwest::StatusCode,
    pub body: String,
}

/// Check whether a provider kind is supported by the current translator set.
pub fn is_supported(_kind: ProviderKind) -> bool {
    // Custom providers are treated as OpenAI-compatible passthrough,
    // so they are supported.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ModelStatus, Provider, RouteEntry};

    #[test]
    fn normalize_base_strips_trailing_v1() {
        assert_eq!(normalize_base("https://openrouter.ai/api/v1"), "https://openrouter.ai/api");
        assert_eq!(normalize_base("https://api.openai.com/v1/"), "https://api.openai.com");
        assert_eq!(normalize_base("https://api.deepseek.com"), "https://api.deepseek.com");
        assert_eq!(normalize_base("http://localhost:11434/v1"), "http://localhost:11434");
        // /v1beta (Google) must not be touched.
        assert_eq!(
            normalize_base("https://generativelanguage.googleapis.com/v1beta"),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

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
        let (url, headers, body) = build_upstream_request(&target, &chat_req, false).unwrap();
        assert_eq!(url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(headers.get("Authorization").unwrap(), "Bearer sk-secret");
        assert_eq!(headers.get("X-Custom").unwrap(), "yes");
        assert_eq!(body["model"], "deepseek-v4-flash");
    }
}

/// An OpenAI-compatible completions request. Kept flexible with `#[serde(flatten)]`
/// so unknown provider-specific fields pass through untouched.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub prompt: serde_json::Value,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub logprobs: Option<u32>,
    #[serde(default)]
    pub echo: Option<bool>,
    #[serde(default)]
    pub stop: Option<serde_json::Value>,
    #[serde(default)]
    pub best_of: Option<u32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub logit_bias: Option<serde_json::Value>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// A single choice in a completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionChoice {
    pub text: String,
    pub index: u32,
    #[serde(default)]
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: String,
}

/// Usage stats returned with a completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// An OpenAI-compatible completions response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: CompletionUsage,
}

/// An OpenAI-compatible embeddings request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: serde_json::Value,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// A single embedding vector in an embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingData {
    pub object: String,
    pub index: u32,
    pub embedding: Vec<f32>,
}

/// Usage stats returned with an embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

/// An OpenAI-compatible embeddings response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
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
/// raw response bytes. For OpenAI-compatible providers the response is
/// already in OpenAI format, so no translation is needed.
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
        return Err(ProviderError {
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
        return Err(ProviderError {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .context("provider returned an error response");
    }
    Ok(bytes.to_vec())
}
