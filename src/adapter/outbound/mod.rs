//! Outbound adapters: one module per upstream provider kind.
//!
//! Each child module knows how to speak its provider's native wire format —
//! build the URL/headers/body, translate the canonical request out, and decode
//! native responses or SSE chunks back into canonical form. This module is the
//! dispatcher that picks the right adapter for a [`Target`] based on its
//! [`ProviderKind`], so callers never match on provider kinds themselves.
//!
//! - [`openai`]: OpenAI passthrough (also covers `custom` kinds)
//! - [`anthropic`]: Anthropic `/v1/messages`
//! - [`google`]: Google `generateContent`

pub mod anthropic;
pub mod google;
pub mod openai;
pub mod responses;

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use reqwest::Client;

use crate::domain::ProviderKind;
use crate::router::Target;
use crate::translator::{ChatRequest, StreamEvent};

/// Strip a trailing `/v1` segment (and any trailing slashes) from a provider
/// base URL. The URL builders append their own versioned path
/// (`/v1/chat/completions`, `/v1/messages`, `/v1beta/models/...`), so a base
/// URL copied from provider docs that already ends in `/v1` (e.g.
/// `https://api.example.com/v1`) would otherwise produce a doubled segment
/// like `/v1/v1/chat/completions` and 404 on every request and health probe.
pub fn normalize_base(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

/// An error response straight from the upstream provider, preserving its HTTP
/// status so usage logging can record what actually happened.
#[derive(Debug, thiserror::Error)]
#[error("provider returned {}: {}", status, body)]
pub struct ProviderError {
    pub status: reqwest::StatusCode,
    pub body: String,
}

/// Check whether a provider kind is supported by the current adapter set.
pub fn is_supported(_kind: ProviderKind) -> bool {
    // Custom providers are treated as OpenAI passthrough,
    // so they are supported.
    true
}

/// Build the upstream request for a target: URL, headers, and a body already
/// shaped for the provider. `stream` selects the streaming endpoint variant.
pub fn build_upstream_request(
    target: &Target,
    chat_req: &ChatRequest,
    stream: bool,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)> {
    match target.provider.kind {
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
        // The Responses endpoint is only served non-streamed in v1; the
        // streaming handler wraps the full answer into an SSE response.
        ProviderKind::OpenAIResponses => {
            let url = responses::build_url(target);
            let headers = responses::build_headers(target);
            let body = responses::translate_request(chat_req, &target.entry.model_id);
            Ok((url, headers, body))
        }
        // OpenAI and custom providers share the passthrough adapter.
        ProviderKind::OpenAI | ProviderKind::Custom => {
            openai::build_upstream_request(target, chat_req, stream)
        }
    }
}

/// Reshape a successful upstream response into OpenAI JSON bytes.
/// OpenAI responses pass through as-is.
pub fn translate_response(target: &Target, bytes: &[u8]) -> Result<Vec<u8>> {
    match target.provider.kind {
        ProviderKind::OpenAI => Ok(bytes.to_vec()),
        ProviderKind::Anthropic => {
            let resp: serde_json::Value =
                serde_json::from_slice(bytes).context("parsing anthropic response")?;
            let out = anthropic::translate_response(&resp)?;
            serde_json::to_vec(&out).context("serializing translated response")
        }
        ProviderKind::Google => {
            let resp: serde_json::Value =
                serde_json::from_slice(bytes).context("parsing google response")?;
            let out = google::translate_response(&resp, &target.entry.model_id)?;
            serde_json::to_vec(&out).context("serializing translated response")
        }
        ProviderKind::OpenAIResponses => {
            let resp: serde_json::Value =
                serde_json::from_slice(bytes).context("parsing responses body")?;
            let out = responses::translate_response(&resp, &target.entry.model_id)?;
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

/// Decode one upstream SSE `data:` payload into a canonical [`StreamEvent`],
/// dispatching on the provider kind.
pub fn parse_stream_chunk(kind: ProviderKind, payload: &str) -> Option<StreamEvent> {
    match kind {
        ProviderKind::Anthropic => anthropic::parse_stream_chunk(payload),
        ProviderKind::Google => google::parse_stream_chunk(payload),
        // Never reached: Responses upstreams are served non-streamed, but the
        // openai decoder is the safest passthrough if a body ever lands here.
        ProviderKind::OpenAI | ProviderKind::OpenAIResponses | ProviderKind::Custom => {
            openai::parse_stream_chunk(payload)
        }
    }
}

pub use openai::parse_canonical;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_strips_trailing_v1() {
        assert_eq!(
            normalize_base("https://api.example.com/v1"),
            "https://api.example.com"
        );
        assert_eq!(
            normalize_base("https://api.openai.com/v1/"),
            "https://api.openai.com"
        );
        assert_eq!(
            normalize_base("https://api.example.org"),
            "https://api.example.org"
        );
        assert_eq!(
            normalize_base("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
        // /v1beta (Google) must not be touched.
        assert_eq!(
            normalize_base("https://generativelanguage.googleapis.com/v1beta"),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }
}
