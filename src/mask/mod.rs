//! Egress masking: relay upstream requests through a separate network identity.
//!
//! A *mask* is an HTTP hop that AGOS sends requests through, so a provider's
//! traffic does not leave from the IP of the host running AGOS. Binding one mask
//! per provider is what lets several API keys of the same upstream present
//! distinct network identities.
//!
//! # Backend contract
//!
//! A mask is deliberately dumb and platform-neutral — it can be a Cloudflare
//! Worker, an AWS Lambda function URL, a Cloud Run service, an nginx box on a
//! VPS, or a commercial proxy. All it has to do is:
//!
//! 1. Reject requests whose [`MASK_HEADER`] does not match its shared secret.
//! 2. Forward the body to the URL in [`MASK_TARGET_HEADER`], untouched, and
//!    stream the reply back without buffering.
//! 3. Strip `X-forward-*`, `cf-*`, `x-forwarded-*` and `host` from the forwarded
//!    request, and disable automatic redirect following.
//! 4. Answer [`MASK_PROBE_HEADER`] with its own egress identity as
//!    `{"ip": ..., "asn": ..., "country": ...}` so `mask audit` can verify it.
//!
//! # What a mask cannot do
//!
//! Masking only defeats limits keyed on network identity (IP, /24 or ASN). If an
//! upstream meters quota per account instead — same email, payment method or
//! device — no egress hop changes that.
//!
//! Note also that an HTTP hop necessarily sees the upstream credentials AGOS
//! forwards, unlike a CONNECT proxy, which only ever sees a hostname.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::domain::MaskingServer;

/// Request header carrying the mask's shared secret.
pub const MASK_HEADER: &str = "X-forward-mask";
/// Request header carrying the absolute upstream URL to forward to.
pub const MASK_TARGET_HEADER: &str = "X-forward-target";
/// Request header asking a mask to report its own egress identity.
pub const MASK_PROBE_HEADER: &str = "X-forward-probe";
/// Response header value a mask sets when *it* refused or failed the request.
pub const MASK_ERR: &str = "err";

/// Sentinel embedded in an error message when a request was skipped because the
/// body is larger than the mask can carry.
///
/// Like the adapter's capability sentinel, this tells the router to try a
/// different target **without** demoting the entry: the provider is healthy, the
/// hop just cannot carry this payload.
pub const MASK_SKIP: &str = "AGOS_MASK_SKIP";

/// Sentinel embedded in an error message when the hop itself failed, so the
/// provider is never blamed for it.
pub const MASK_FAILED: &str = "AGOS_MASK_FAILED";

/// A mask's own view of the network identity it egresses from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressIdentity {
    pub ip: String,
    #[serde(default)]
    pub asn: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

/// Route an already-built request through `mask`.
///
/// Rewrites `url` to the hop and injects [`MASK_HEADER`] plus
/// [`MASK_TARGET_HEADER`] (the original upstream URL) into `headers`. Callers
/// then send to the rewritten `url` as usual, so a masked and an unmasked
/// request take exactly the same send path.
pub fn apply_request(
    mask: Option<&MaskingServer>,
    url: &mut String,
    headers: &mut BTreeMap<String, String>,
    body_len: usize,
) -> Result<()> {
    let Some(mask) = mask else {
        return Ok(());
    };
    if mask.max_body_bytes > 0 && body_len as i64 > mask.max_body_bytes {
        bail!(
            "{MASK_SKIP} request body of {body_len} bytes exceeds the {:?} mask limit of {} bytes",
            mask.name,
            mask.max_body_bytes
        );
    }
    headers.insert(MASK_HEADER.to_string(), mask.secret.clone());
    headers.insert(MASK_TARGET_HEADER.to_string(), url.clone());
    *url = mask.endpoint_url.clone();
    Ok(())
}

/// [`apply_request`] for a JSON body, measuring it only when the mask declares a
/// size ceiling (a base64 image easily exceeds a serverless hop's request cap).
pub fn apply_json(
    mask: Option<&MaskingServer>,
    url: &mut String,
    headers: &mut BTreeMap<String, String>,
    body: &serde_json::Value,
) -> Result<()> {
    let body_len = match mask {
        Some(m) if m.max_body_bytes > 0 => serde_json::to_vec(body)
            .map(|bytes| bytes.len())
            .unwrap_or(0),
        _ => 0,
    };
    apply_request(mask, url, headers, body_len)
}

/// Whether a response says the *hop* failed rather than the upstream.
pub fn is_mask_rejection(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(MASK_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case(MASK_ERR))
        .unwrap_or(false)
}

/// Build the error for a response the mask itself refused.
pub fn rejection_error(mask_name: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{MASK_FAILED} egress mask {mask_name:?} returned {status}: {}",
        body.trim()
    )
}

/// Ask a mask to report its own egress identity.
///
/// This is how `agos-proxy mask test` and `mask audit` establish that several
/// masks really are several identities. Serverless backends share a platform IP
/// pool, so five deployments of the same platform can report five addresses
/// inside one ASN — which is exactly what `audit` warns about.
pub async fn probe(
    client: &Client,
    mask: &MaskingServer,
    timeout: Duration,
) -> Result<EgressIdentity> {
    let mut req = client.get(&mask.endpoint_url);
    req = req.header(MASK_HEADER, &mask.secret);
    req = req.header(MASK_PROBE_HEADER, "ip");
    let resp = match tokio::time::timeout(timeout, req.send()).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => return Err(e).context("reaching the mask"),
        Err(_) => bail!("mask {:?} probe timed out after {timeout:?}", mask.name),
    };
    let status = resp.status();
    let bytes = resp.bytes().await.context("reading the mask probe reply")?;
    if !status.is_success() {
        bail!(
            "mask {:?} probe returned {status}: {}",
            mask.name,
            String::from_utf8_lossy(&bytes).trim()
        );
    }
    serde_json::from_slice::<EgressIdentity>(&bytes)
        .with_context(|| format!("mask {:?} probe reply is not an egress identity", mask.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPSTREAM: &str = "https://upstream.example/v1/chat/completions";

    fn mask(max_body_bytes: i64) -> MaskingServer {
        MaskingServer {
            id: 1,
            profile_id: "p".into(),
            name: "edge-1".into(),
            kind: "vps".into(),
            endpoint_url: "https://edge-1.example/forward".into(),
            secret: "s3cret".into(),
            max_body_bytes,
            expected_egress_ip: None,
            last_verified_ip: None,
            last_verified_asn: None,
            last_verified_country: None,
            last_verified_at: None,
        }
    }

    fn headers() -> BTreeMap<String, String> {
        let mut h = BTreeMap::new();
        h.insert("Authorization".into(), "Bearer up-key".into());
        h
    }

    #[test]
    fn no_mask_leaves_the_request_untouched() {
        let mut url = UPSTREAM.to_string();
        let mut h = headers();
        apply_request(None, &mut url, &mut h, 10).unwrap();
        assert_eq!(url, UPSTREAM);
        assert!(!h.contains_key(MASK_HEADER));
        assert!(!h.contains_key(MASK_TARGET_HEADER));
    }

    #[test]
    fn a_mask_rewrites_the_url_and_names_the_original_target() {
        let mut url = UPSTREAM.to_string();
        let mut h = headers();
        apply_request(Some(&mask(0)), &mut url, &mut h, 10).unwrap();
        assert_eq!(url, "https://edge-1.example/forward");
        assert_eq!(h.get(MASK_TARGET_HEADER).unwrap(), UPSTREAM);
        assert_eq!(h.get(MASK_HEADER).unwrap(), "s3cret");
        // Provider credentials still travel: the hop forwards them untouched.
        assert_eq!(h.get("Authorization").unwrap(), "Bearer up-key");
    }

    #[test]
    fn an_oversized_body_skips_the_mask_without_blaming_the_provider() {
        let mut url = UPSTREAM.to_string();
        let mut h = headers();
        let err = apply_json(
            Some(&mask(16)),
            &mut url,
            &mut h,
            &serde_json::json!({"a": "x".repeat(64)}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(MASK_SKIP), "unexpected error: {err}");
        // Nothing was rewritten, so the caller still holds a usable URL.
        assert_eq!(url, UPSTREAM);
    }

    #[test]
    fn size_limits_only_apply_when_declared() {
        let body = serde_json::json!({"a": "x".repeat(10_000)});

        let mut url = UPSTREAM.to_string();
        let mut h = headers();
        apply_json(
            Some(&mask(4096)),
            &mut url,
            &mut h,
            &serde_json::json!({"a": "x"}),
        )
        .expect("a small body passes the ceiling");
        assert_eq!(url, "https://edge-1.example/forward");

        // 0 means "no ceiling", so even a large body is carried.
        let mut url = UPSTREAM.to_string();
        let mut h = headers();
        apply_json(Some(&mask(0)), &mut url, &mut h, &body).expect("no ceiling is declared");
        assert_eq!(url, "https://edge-1.example/forward");
    }

    #[test]
    fn the_shared_secret_is_never_serialized() {
        // `MaskingServer.secret` is `#[serde(skip)]`, so a mask can never leak its
        // secret into an API response or an exported profile.
        let json = serde_json::to_string(&mask(0)).unwrap();
        assert!(
            !json.contains("s3cret"),
            "secret leaked into serialized form: {json}"
        );
    }
}
