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
pub mod prompt_cache;
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
    /// Upstream-advised wait before retrying this entry, in seconds.
    ///
    /// Captured because a rate-limited key must be taken out of rotation for
    /// the period the upstream asked for: `Degraded` alone is not enough, since
    /// degraded entries stay selectable.
    pub retry_after: Option<i64>,
}

impl ProviderError {
    /// Build the error for a non-success upstream response, capturing any retry
    /// hint the upstream supplied.
    pub fn from_response(
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
        body: String,
    ) -> Self {
        Self {
            status,
            body,
            retry_after: retry_after_secs(headers),
        }
    }
}

/// Parse an upstream retry hint from response headers.
///
/// Understands `Retry-After` in delta-seconds form plus the duration-ish
/// `x-ratelimit-reset-*` values some gateways emit (`30s`, `2m`, `1h30m`). An
/// HTTP-date `Retry-After` is deliberately not parsed: it falls back to the
/// configured default cooldown rather than guessing a wall-clock offset.
pub fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    const HINTS: [&str; 3] = [
        "retry-after",
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
    ];
    HINTS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_delay_secs)
    })
}

/// Parse `120`, `120s`, `2m`, `1.5s` or `1h30m` into whole seconds.
fn parse_delay_secs(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(secs) = value.parse::<i64>() {
        return Some(secs.max(0));
    }
    // Duration form: one or more `<number><unit>` segments, e.g. `1h30m`.
    let mut total = 0f64;
    let mut number = String::new();
    let mut saw_unit = false;
    for ch in value.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
            continue;
        }
        let multiplier = match ch.to_ascii_lowercase() {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            _ => return None,
        };
        total += number.parse::<f64>().ok()? * multiplier;
        number.clear();
        saw_unit = true;
    }
    if saw_unit && number.is_empty() {
        Some(total.round() as i64)
    } else {
        None
    }
}

/// Check whether a provider kind is supported by the current adapter set.
pub fn is_supported(_kind: ProviderKind) -> bool {
    // Custom providers are treated as OpenAI passthrough,
    // so they are supported.
    true
}

/// Build the upstream request for a target: URL, headers, and a body already
/// shaped for the provider. `stream` selects the streaming endpoint variant.
///
/// The returned body has the route's prompt-cache policy applied last, so the
/// translators stay pure and every provider dialect is handled in one place.
pub fn build_upstream_request(
    target: &Target,
    chat_req: &ChatRequest,
    stream: bool,
) -> Result<(String, BTreeMap<String, String>, serde_json::Value)> {
    let (url, headers, mut body) = build_native_request(target, chat_req, stream)?;
    let markers = prompt_cache::apply(target.provider.kind, target.prompt_cache, &mut body);
    if markers > 0 {
        tracing::debug!(
            provider = %target.provider.name,
            model = %target.entry.model_id,
            markers,
            "applied prompt-cache markers"
        );
    }
    Ok((url, headers, body))
}

/// Provider-native request building, without the prompt-cache pass.
fn build_native_request(
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
            let body = responses::translate_request(chat_req, &target.entry.model_id)?;
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
    let (mut url, mut headers, body) = build_upstream_request(target, chat_req, false)?;
    // Leave through the provider's mask when one is bound (directly, or via the
    // profile default). A hop that cannot carry the body fails here, before any
    // bytes are spent upstream.
    crate::mask::apply_json(
        target.provider.masking_server.as_ref(),
        &mut url,
        &mut headers,
        &body,
    )?;
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
    // Read the mask verdict and any retry hint before the body is consumed.
    let mask_rejected = crate::mask::is_mask_rejection(&resp);
    let retry_after = retry_after_secs(resp.headers());
    let bytes = resp.bytes().await.context("reading provider response")?;
    if !status.is_success() {
        if mask_rejected {
            if let Some(mask) = target.provider.masking_server.as_ref() {
                return Err(crate::mask::rejection_error(
                    &mask.name,
                    status,
                    &String::from_utf8_lossy(&bytes),
                ));
            }
        }
        return Err(ProviderError {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            retry_after,
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

    #[test]
    fn retry_hints_are_parsed_from_the_forms_gateways_actually_send() {
        assert_eq!(parse_delay_secs("120"), Some(120));
        assert_eq!(parse_delay_secs("120s"), Some(120));
        assert_eq!(parse_delay_secs("2m"), Some(120));
        assert_eq!(parse_delay_secs("1h30m"), Some(5400));
        assert_eq!(parse_delay_secs("1m30s"), Some(90));
        // An HTTP-date `Retry-After` is not guessed at: the configured default
        // cool-down is used instead of inventing a wall-clock offset.
        assert_eq!(parse_delay_secs("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_delay_secs(""), None);
        assert_eq!(parse_delay_secs("soon"), None);
    }

    #[test]
    fn retry_hints_prefer_retry_after_over_gateway_reset_headers() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_secs(&headers), None);

        headers.insert("x-ratelimit-reset-requests", "30s".parse().unwrap());
        assert_eq!(retry_after_secs(&headers), Some(30));

        headers.insert("retry-after", "5".parse().unwrap());
        assert_eq!(retry_after_secs(&headers), Some(5));
    }
}
