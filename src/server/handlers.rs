//! HTTP handlers for the OpenAI surface.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::response::Response;
use futures::StreamExt;

use super::sse;
use crate::adapter::outbound;
use crate::adapter::Registry;
use crate::router::{execute_with_failover, resolve_targets_with_strategy, RoutingState};
use crate::storage::Store;
use crate::translator::ChatRequest;

/// Per-handler body size cap. The outer tower-http layer enforces 10 MB on
/// the raw stream; this tighter cap protects the JSON layer from allocating
/// huge intermediate buffers for malformed but technically-in-range payloads.
const HANDLER_BODY_LIMIT: usize = 5 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub attempt_timeout: Duration,
    /// Max gap between upstream stream chunks once committed; a provider that
    /// stalls longer than this fails the stream instead of hanging the client.
    pub stream_idle_timeout: Duration,
    pub http_client: reqwest::Client,
    pub routing_state: RoutingState,
    pub rate_limiter: Arc<crate::server::ratelimit::RateLimiter>,
    /// When true, the /ready endpoint requires authentication.
    pub require_auth_on_health: bool,
    /// The master inbound-adapter registry used to dispatch native API surfaces.
    pub adapter: Registry,
}

/// Legacy OpenAI completions request (non-streaming passthrough).
#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct CompletionRequest {
    model: String,
    prompt: String,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    echo: bool,
}

/// OpenAI embeddings request (passthrough).
#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct EmbeddingRequest {
    model: String,
    input: serde_json::Value,
}

pub async fn chat_completions(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let profile_id = match parts.extensions.get::<String>() {
        Some(id) => id.clone(),
        None => return bad_request("unauthenticated"),
    };
    let escalate_header = crate::router::wants_escalation_header(
        parts
            .headers
            .get("X-Economy-Escalate")
            .and_then(|v| v.to_str().ok()),
    );
    let bytes = match axum::body::to_bytes(body, HANDLER_BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return bad_request(format!("failed to read body: {e}")),
    };
    let body_value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    let needs = crate::router::RequestNeeds::from_body(&body_value);
    let escalate_body = crate::router::wants_escalation(&body_value);
    let chat_req: ChatRequest = match serde_json::from_value(body_value) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    if chat_req.stream {
        handle_streaming(state, profile_id, needs, chat_req).await
    } else {
        handle_non_streaming(
            state,
            profile_id,
            needs,
            chat_req,
            escalate_header || escalate_body,
        )
        .await
    }
}

/// Handle `/v1/completions` — legacy OpenAI completions API, passthrough
/// through the routing layer with automatic failover.
pub async fn completions(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let profile_id = match parts.extensions.get::<String>() {
        Some(id) => id.clone(),
        None => return bad_request("unauthenticated"),
    };
    let bytes = match axum::body::to_bytes(body, HANDLER_BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return bad_request(format!("failed to read body: {e}")),
    };
    let body_value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    let completion_req: CompletionRequest = match serde_json::from_value(body_value.clone()) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    if completion_req.stream {
        handle_completion_streaming(state, profile_id, completion_req).await
    } else {
        handle_completion(state, profile_id, completion_req).await
    }
}

/// Handle `/v1/embeddings` — OpenAI embeddings API, passthrough
/// through the routing layer with automatic failover.
pub async fn embeddings(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let profile_id = match parts.extensions.get::<String>() {
        Some(id) => id.clone(),
        None => return bad_request("unauthenticated"),
    };
    let bytes = match axum::body::to_bytes(body, HANDLER_BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return bad_request(format!("failed to read body: {e}")),
    };
    let body_value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    let embedding_req: EmbeddingRequest = match serde_json::from_value(body_value) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    handle_embeddings(state, profile_id, embedding_req).await
}

/// When a route has an identity, prepend a system message telling the model
/// to adopt that identity and never reveal its original model or developer.
pub(crate) fn inject_identity_into_messages(
    messages: &mut Vec<crate::translator::Message>,
    identity: &str,
) {
    let system_content = format!(
        "You are {}. You must never reveal your original model name, developer,
        creator, or that you are powered by any specific AI system, API, or company.
        When asked who you are, what model you are, who made you, or what powers you,
        always respond naturally as {}. You ARE {} — this is your one and
        only identity. Never break character.",
        identity, identity, identity
    );
    messages.insert(
        0,
        crate::translator::Message::text("system", system_content),
    );
}

async fn handle_non_streaming(
    state: AppState,
    profile_id: String,
    needs: crate::router::RequestNeeds,
    mut chat_req: ChatRequest,
    escalate: bool,
) -> Response {
    let model = chat_req.model.clone();
    let mut targets = match resolve_targets_with_strategy(
        &state.store,
        &profile_id,
        &model,
        needs,
        &state.routing_state,
    ) {
        Ok(t) if t.is_empty() => {
            return service_unavailable("no healthy providers available for this route")
        }
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no proxy named") || msg.contains("no route named") {
                return not_found(msg);
            }
            if msg.contains("must be in") {
                return bad_request(msg);
            }
            return bad_request(format!("route resolution failed: {e}"));
        }
    };
    // Route-level economy config (max_tokens clamp + cache TTL).
    let (route_max_tokens, route_cache_ttl, route_id) =
        route_economy(&state.store, &profile_id, &model);
    if escalate && targets.len() > 1 {
        // Explicit escalation: try the most expensive entry first.
        targets.sort_by(|a, b| {
            b.entry
                .price_per_1m
                .partial_cmp(&a.entry.price_per_1m)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    if route_max_tokens > 0 {
        clamp_chat_max_tokens(&mut chat_req, route_max_tokens);
    }
    // Inject identity system message if the route has one
    if let Some(identity) = targets.first().and_then(|t| t.identity.as_deref()) {
        inject_identity_into_messages(&mut chat_req.messages, identity);
    }
    // Exact-cache: deterministic requests only (no tools/stream, temp≈0 or unset).
    // Escalated requests always bypass the cache — they must reach the flagship.
    let cache_key = if route_cache_ttl > 0 && !escalate {
        cache_hash(&profile_id, &model, &chat_req)
    } else {
        None
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    if let (Some(hash), Some(rid)) = (cache_key.clone(), route_id) {
        if let Ok(Some((cached, _, _))) = state.store.cache_get(rid, &hash, now_ms) {
            return Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .header("X-Agos-Cache", "HIT")
                .body(axum::body::Body::from(cached))
                .unwrap();
        }
    }

    let result = execute_with_failover(
        state.store.clone(),
        targets,
        state.attempt_timeout,
        |target| {
            let client = state.http_client.clone();
            let req = chat_req.clone();
            let store = state.store.clone();
            let profile_id = profile_id.clone();
            async move {
                let started = std::time::Instant::now();
                let outcome = outbound::forward_non_streaming(&client, &target, &req).await;
                let latency_ms = started.elapsed().as_millis() as i64;
                log_attempt(&store, &profile_id, &target, false, &outcome, latency_ms);
                outcome
            }
        },
    )
    .await;

    match result {
        Ok(bytes) => {
            if let (Some(hash), Some(rid)) = (cache_key.clone(), route_id) {
                if route_cache_ttl > 0 {
                    let (p, c) = usage_tokens(&bytes);
                    let _ = state.store.cache_put(
                        rid,
                        &hash,
                        &bytes,
                        p,
                        c,
                        chrono::Utc::now().timestamp_millis(),
                        route_cache_ttl,
                    );
                }
            }
            Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .header("X-Agos-Cache", "MISS")
                .body(axum::body::Body::from(bytes))
                .unwrap()
        }
        Err(e) => {
            let status = e
                .downcast_ref::<crate::adapter::outbound::ProviderError>()
                .map(|pe| pe.status.as_u16() as i32);
            let msg = if let Some(se) = e.downcast_ref::<crate::adapter::outbound::ProviderError>()
            {
                format!("provider returned {}: {}", se.status, se.body)
            } else {
                e.to_string()
            };
            let resp = serde_json::json!({
                "error": {
                    "message": msg,
                    "type": "provider_error",
                    "code": status.unwrap_or(502),
                }
            });
            Response::builder()
                .status(axum::http::StatusCode::BAD_GATEWAY)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(resp.to_string()))
                .unwrap()
        }
    }
}

/// Extract prompt/completion tokens from a raw OpenAI response body.
fn usage_tokens(bytes: &[u8]) -> (Option<i64>, Option<i64>) {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            let u = v.get("usage")?.as_object()?.clone();
            Some((
                u.get("prompt_tokens").and_then(|t| t.as_i64()),
                u.get("completion_tokens").and_then(|t| t.as_i64()),
            ))
        })
        .unwrap_or((None, None))
}

/// Resolve `(max_tokens, cache_ttl, route_id)` for economy clamping/caching.
/// Returns zeros when the route cannot be resolved.
fn route_economy(store: &Store, profile_id: &str, model: &str) -> (u32, i64, Option<i64>) {
    let (proxy_name, route_name) = match model.split_once('/') {
        Some(p) => p,
        None => return (0, 0, None),
    };
    let proxy = match store.get_proxy_named(profile_id, proxy_name) {
        Ok(Some(p)) => p,
        _ => return (0, 0, None),
    };
    match store.get_route_named(proxy.id, route_name) {
        Ok(Some(r)) => (r.max_tokens, r.cache_ttl_secs, Some(r.id)),
        _ => (0, 0, None),
    }
}

/// Clamp `max_tokens` / `max_completion_tokens` to the route ceiling.
fn clamp_chat_max_tokens(req: &mut ChatRequest, ceiling: u32) {
    // `extra` is `Null` when the caller sent no optional fields; start from an
    // empty map in that case so the ceiling is still enforced.
    let mut v = match &req.extra {
        serde_json::Value::Null => serde_json::json!({}),
        other => other.clone(),
    };
    let obj = match v.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(cur) = obj.get(key).and_then(|x| x.as_u64()) {
            if cur > ceiling as u64 {
                obj.insert(key.to_string(), serde_json::json!(ceiling));
            }
        }
    }
    // If unset, set a ceiling so runaway completions cannot burn budget.
    if !obj.contains_key("max_tokens") && !obj.contains_key("max_completion_tokens") {
        obj.insert("max_tokens".to_string(), serde_json::json!(ceiling));
    }
    if let Ok(extra) = serde_json::from_value(v) {
        req.extra = extra;
    }
}

/// Deterministic cache key — only for cacheable requests:
/// non-streaming, no tools, temperature unset/0. Returns None otherwise.
///
/// Note: `ChatRequest` is flattened, so `stream`/`tools`/`temperature` live at
/// the top level of the serialized value; `messages`/`model` too. The proxy-local
/// `economy_escalate` flag (also flattened into `extra`) is stripped before
/// hashing so escalated and normal requests share a key — escalation bypasses
/// the cache at the call site anyway.
fn cache_hash(profile_id: &str, model: &str, req: &ChatRequest) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut v = serde_json::to_value(req).ok()?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("economy_escalate");
    }
    if v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false) {
        return None;
    }
    if v.get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        return None;
    }
    // Anthropic-style tool passthrough: a non-empty tool-result block also
    // disqualifies the request (kept cheap — string scan of message extras).
    if serde_json::to_string(v.get("messages").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_default()
        .contains("tool_call_id")
    {
        return None;
    }
    let temp_ok = match v.get("temperature") {
        None | Some(serde_json::Value::Null) => true,
        Some(t) => t.as_f64().is_some_and(|f| f == 0.0),
    };
    if !temp_ok {
        return None;
    }
    let mut h = Sha256::new();
    h.update(profile_id.as_bytes());
    h.update(b"|");
    h.update(model.as_bytes());
    h.update(b"|");
    h.update(serde_json::to_string(&v).ok()?.as_bytes());
    Some(hex::encode(h.finalize()))
}

pub(crate) fn log_attempt(
    store: &Arc<Store>,
    profile_id: &str,
    target: &crate::router::Target,
    streamed: bool,
    outcome: &Result<Vec<u8>, anyhow::Error>,
    latency_ms: i64,
) {
    let (success, status_code, error_message, prompt_tokens, completion_tokens) = match outcome {
        Ok(bytes) => {
            let usage = serde_json::from_slice::<serde_json::Value>(bytes)
                .ok()
                .and_then(|mut v| {
                    let usage_obj = v.get_mut("usage")?.as_object()?.clone();
                    let prompt = usage_obj.get("prompt_tokens").and_then(|t| t.as_i64());
                    let completion = usage_obj.get("completion_tokens").and_then(|t| t.as_i64());
                    Some((prompt, completion))
                })
                .unwrap_or((None, None));
            (true, Some(200), None, usage.0, usage.1)
        }
        Err(e) => {
            let status = e
                .downcast_ref::<crate::adapter::outbound::ProviderError>()
                .map(|pe| pe.status.as_u16() as i32);
            (false, status, Some(e.to_string()), None, None)
        }
    };
    let _ = store.record_usage(crate::storage::NewUsage {
        profile_id: profile_id.to_string(),
        route_entry_id: target.entry.id,
        model_id: target.entry.model_id.clone(),
        streamed,
        success,
        status_code,
        error_message,
        latency_ms,
        prompt_tokens,
        completion_tokens,
    });
}

async fn handle_streaming(
    state: AppState,
    profile_id: String,
    needs: crate::router::RequestNeeds,
    chat_req: ChatRequest,
) -> Response {
    let model = chat_req.model.clone();
    let store = state.store.clone();
    let attempt_timeout = state.attempt_timeout;
    let stream_idle_timeout = state.stream_idle_timeout;
    let client = state.http_client.clone();

    // Try each upstream in sequence to establish a successful HTTP connection
    let targets = match resolve_targets_with_strategy(
        &store,
        &profile_id,
        &model,
        needs,
        &state.routing_state,
    ) {
        Ok(t) if t.is_empty() => {
            return service_unavailable("no healthy providers available for this route");
        }
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no proxy named") || msg.contains("no route named") {
                return not_found(format!("model not found: {msg}"));
            } else if msg.contains("must be in") {
                return bad_request(msg);
            } else {
                return bad_request(format!("route resolution failed: {msg}"));
            }
        }
    };

    // Find the first working upstream: send the request for real and only
    // commit once an upstream's body proves itself. Several OpenAI
    // OpenAI free tiers in particular answer HTTP 200 and then
    // deliver an *in-band* SSE error event (`data: {"error": ...}`), so a 2xx
    // alone is not enough — the first frames must carry real content before
    // the 200 goes out to the client. Failed attempts are logged, the entry is
    // demoted per the failure classification, and the next target is tried.
    let mut working: Option<(crate::router::Target, ProbeOutcome)> = None;

    for target in targets {
        let started_at = std::time::Instant::now();

        // Responses-kind upstreams have no streaming endpoint in v1: the
        // request is served from the non-streamed answer, wrapped into a
        // minimal OpenAI SSE stream. A failure here behaves exactly like any
        // other attempt failure — logged, classified, and the next entry in
        // the chain is tried.
        if target.provider.kind == crate::domain::ProviderKind::OpenAIResponses {
            match forward_responses_attempt(&client, &target, &chat_req, attempt_timeout).await {
                Ok((completion, prompt_tokens, completion_tokens)) => {
                    log_stream_outcome(
                        &store,
                        &profile_id,
                        &target,
                        true,
                        Some(200),
                        None,
                        started_at.elapsed().as_millis() as i64,
                        prompt_tokens,
                        completion_tokens,
                    );
                    return sse_response_from_completion(&completion);
                }
                Err(e) => {
                    // anyhow's Display only shows the outer context, so the
                    // raw upstream body (which names images on a rejection)
                    // must come from the downcast. Format it the same way the
                    // streaming branch does so classification sees it too.
                    let provider_err = e.downcast_ref::<crate::adapter::outbound::ProviderError>();
                    let status = provider_err.map(|pe| pe.status.as_u16() as i32);
                    let message = provider_err
                        .map(|pe| format!("provider returned {}: {}", pe.status, pe.body))
                        .unwrap_or_else(|| e.to_string());
                    fail_stream_attempt(
                        &store,
                        &profile_id,
                        &target,
                        status,
                        message,
                        started_at.elapsed().as_millis() as i64,
                    );
                    continue;
                }
            }
        }

        let req = match translate_and_forward_streaming(&client, &target, &chat_req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(err = %e, "translate request failed, trying next");
                continue;
            }
        };

        let resp = match tokio::time::timeout(attempt_timeout, client.execute(req)).await {
            Ok(Ok(resp)) if resp.status().is_success() => resp,
            Ok(Ok(resp)) => {
                // Non-success status, log and try the next target.
                let status = resp.status();
                let bytes = resp.bytes().await.unwrap_or_default();
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    Some(status.as_u16() as i32),
                    format!(
                        "provider returned {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    ),
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
            Ok(Err(e)) => {
                // Upstream connection failed.
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    None,
                    format!("upstream failed: {e}"),
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
            Err(_) => {
                // Upstream timed out before sending headers.
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    None,
                    "upstream timeout".to_string(),
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
        };

        // Probe the 2xx body: read frames until the stream proves it is a real
        // completion (commit) or reveals an in-band error (fail over).
        match probe_stream(stream_idle_timeout, resp).await {
            Probe::Committed {
                stream,
                parser,
                pending,
            } => {
                working = Some((
                    target,
                    ProbeOutcome {
                        stream,
                        parser,
                        pending,
                        started_at,
                    },
                ));
                break;
            }
            Probe::InBandError { code, message } => {
                tracing::warn!(
                    provider = %target.provider.name,
                    model = %target.entry.model_id,
                    code = ?code,
                    "upstream answered 2xx with an in-band SSE error, failing over"
                );
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    Some(code.unwrap_or(502) as i32),
                    format!("upstream in-band error: {message}"),
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
            Probe::Failed(msg) => {
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    None,
                    msg,
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
            Probe::Timeout => {
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    Some(504),
                    "upstream stalled before sending any stream data".to_string(),
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
            Probe::Transport(e) => {
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    None,
                    format!("stream error: {e}"),
                    started_at.elapsed().as_millis() as i64,
                );
                continue;
            }
        }
    }

    // If no working upstream was found, fail before any 200 is sent.
    let Some((target, outcome)) = working else {
        let error_response = serde_json::json!({
            "error": {
                "message": "All upstream providers failed",
                "type": "provider_error",
                "code": 502,
            }
        });
        return Response::builder()
            .status(axum::http::StatusCode::BAD_GATEWAY)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(error_response.to_string()))
            .unwrap();
    };

    // A working upstream is confirmed: stream its response body to the client.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);

    tokio::spawn(pump_stream(
        target.clone(),
        target.provider.kind,
        outcome,
        model,
        stream_idle_timeout,
        tx,
        store.clone(),
        profile_id,
    ));

    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(body)
        .unwrap()
}

/// The state a committed upstream stream carries from the probe into the pump:
/// the partially-consumed body stream, the parser holding any half-received
/// frame bytes, and frames that were decoded after the commit decision.
struct ProbeOutcome {
    stream: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    parser: sse::SseParser,
    pending: sse::PendingFrames,
    started_at: std::time::Instant,
}

/// Result of probing an upstream 2xx body before committing to it.
enum Probe {
    /// The body carries a real completion stream; hand the remnants to the pump.
    Committed {
        stream: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
        parser: sse::SseParser,
        pending: sse::PendingFrames,
    },
    /// The stream delivered an in-band error event before any content.
    InBandError { code: Option<i64>, message: String },
    /// The stream ended without ever carrying content.
    Failed(String),
    /// No frame arrived within the idle timeout.
    Timeout,
    /// The body stream errored at the transport level.
    Transport(String),
}

/// Outcome of awaiting the next upstream chunk under the idle guard.
enum NextChunk {
    /// A body chunk arrived.
    Chunk(Bytes),
    /// The upstream closed the stream cleanly.
    End,
    /// Nothing arrived within the idle window.
    Idle,
    /// The transport failed mid-stream.
    Transport(String),
}

/// Await the next chunk from a committed upstream stream, applying the idle
/// timeout. A zero window disables the guard entirely, which lets operators opt
/// out via `AGOS_STREAM_IDLE_TIMEOUT_SECS=0` when a provider is legitimately
/// slow between chunks.
async fn next_chunk(
    stream: &mut futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    idle_timeout: Duration,
) -> NextChunk {
    let awaited = async {
        match stream.next().await {
            Some(Ok(bytes)) => NextChunk::Chunk(bytes),
            Some(Err(e)) => NextChunk::Transport(e.to_string()),
            None => NextChunk::End,
        }
    };
    if idle_timeout.is_zero() {
        return awaited.await;
    }
    match tokio::time::timeout(idle_timeout, awaited).await {
        Ok(outcome) => outcome,
        Err(_) => NextChunk::Idle,
    }
}

/// Read frames from a 2xx streaming response until the body proves it is a
/// real completion. The very first frames decide: an in-band `{"error": ...}`
/// payload means the attempt failed (fail over), anything else commits — the
/// already-decoded frames travel to the pump so no bytes are lost.
async fn probe_stream(idle_timeout: Duration, resp: reqwest::Response) -> Probe {
    let mut parser = sse::SseParser::new();
    let mut pending = sse::PendingFrames::new();
    let mut stream: futures::stream::BoxStream<'static, reqwest::Result<Bytes>> =
        Box::pin(resp.bytes_stream());

    loop {
        if let Some(frame) = pending.pop_front() {
            match sse::Frame::classify(&frame.data) {
                sse::Frame::Error { code, message, .. } => {
                    return Probe::InBandError { code, message };
                }
                sse::Frame::Done => {
                    if pending.is_empty() && parser.buf_is_empty() {
                        // `data: [DONE]` with zero content is the empty-200
                        // failure mode, not a success.
                        return Probe::Failed(
                            "upstream returned an empty stream (only [DONE])".to_string(),
                        );
                    }
                    // Content already seen in this chunk: commit, the pump
                    // forwards the sentinel.
                    pending.push_front(frame);
                    return Probe::Committed {
                        stream,
                        parser,
                        pending,
                    };
                }
                sse::Frame::Other { .. } => {
                    // Real content: commit. This frame and everything decoded
                    // after it is replayed by the pump.
                    pending.push_front(frame);
                    return Probe::Committed {
                        stream,
                        parser,
                        pending,
                    };
                }
            }
        }

        match next_chunk(&mut stream, idle_timeout).await {
            NextChunk::Idle => return Probe::Timeout,
            NextChunk::End => {
                pending.extend(parser.finish());
                if pending.is_empty() {
                    return Probe::Failed(
                        "upstream closed the stream without sending data".to_string(),
                    );
                }
            }
            NextChunk::Transport(e) => return Probe::Transport(e),
            NextChunk::Chunk(bytes) => {
                pending.extend(parser.feed(&bytes));
            }
        }
    }
}

/// Log one failed streaming attempt and demote the entry per the failure
/// classification.
fn fail_stream_attempt(
    store: &Arc<Store>,
    profile_id: &str,
    target: &crate::router::Target,
    status_code: Option<i32>,
    message: String,
    latency_ms: i64,
) {
    log_stream_outcome(
        store,
        profile_id,
        target,
        false,
        status_code,
        Some(message.clone()),
        latency_ms,
        None,
        None,
    );
    if let Some(status) = crate::router::demote_status_for(status_code.map(|c| c as i64), &message)
    {
        let _ = store.set_route_entry_status(target.entry.id, status);
    }
}

/// Record exactly one usage row for a streaming attempt with real status and
/// token counts (unlike [`log_attempt`], which parses a full response body).
#[allow(clippy::too_many_arguments)]
fn log_stream_outcome(
    store: &Arc<Store>,
    profile_id: &str,
    target: &crate::router::Target,
    success: bool,
    status_code: Option<i32>,
    error_message: Option<String>,
    latency_ms: i64,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
) {
    let _ = store.record_usage(crate::storage::NewUsage {
        profile_id: profile_id.to_string(),
        route_entry_id: target.entry.id,
        model_id: target.entry.model_id.clone(),
        streamed: true,
        success,
        status_code,
        error_message,
        latency_ms,
        prompt_tokens,
        completion_tokens,
    });
}

/// Stream a committed upstream response to the client.
///
/// - wraps every chunk read in the idle timeout, so a provider that stalls
///   after the headers cannot hang the client forever (Bug 2);
/// - keeps watching for in-band error events and reports them as failed
///   attempts instead of successes (Bug 1, post-commit);
/// - translates Anthropic/Google SSE into OpenAI `chat.completion.chunk`
///   deltas; OpenAI streams pass through verbatim (Bug 4);
/// - extracts the upstream usage frames and records exactly one usage row with
///   the real status and token counts (Bug 6).
#[allow(clippy::too_many_arguments)]
async fn pump_stream(
    target: crate::router::Target,
    kind: crate::domain::ProviderKind,
    outcome: ProbeOutcome,
    model: String,
    idle_timeout: Duration,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    store: Arc<Store>,
    profile_id: String,
) {
    use crate::domain::ProviderKind;

    let ProbeOutcome {
        mut stream,
        mut parser,
        mut pending,
        started_at,
    } = outcome;
    let completion_id = format!(
        "chatcmpl-{}-{}",
        target.entry.id,
        chrono::Utc::now().timestamp_millis()
    );

    let mut prompt_tokens: Option<u64> = None;
    let mut completion_tokens: Option<u64> = None;
    let mut first_chunk = true;
    let mut failure: Option<(Option<i32>, String)> = None;

    'read: loop {
        let frame = if let Some(f) = pending.pop_front() {
            f
        } else {
            match next_chunk(&mut stream, idle_timeout).await {
                NextChunk::Idle => {
                    failure = Some((
                        Some(504),
                        format!("stream idle timeout after {idle_timeout:?}"),
                    ));
                    break 'read;
                }
                NextChunk::End => {
                    pending.extend(parser.finish());
                    match pending.pop_front() {
                        Some(f) => f,
                        None => break 'read,
                    }
                }
                NextChunk::Transport(e) => {
                    let _ = tx
                        .send(Err(std::io::Error::other(format!("stream error: {e}"))))
                        .await;
                    failure = Some((None, format!("stream error: {e}")));
                    break 'read;
                }
                NextChunk::Chunk(bytes) => {
                    pending.extend(parser.feed(&bytes));
                    continue 'read;
                }
            }
        };

        match sse::Frame::classify(&frame.data) {
            sse::Frame::Done => {
                let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                break 'read;
            }
            sse::Frame::Error { code, message, raw } => {
                // The upstream broke the stream mid-flight. Forward the error
                // so OpenAI SDKs surface it, and record the attempt as failed
                // instead of a phantom 200.
                let _ = tx.send(Ok(Bytes::from(format!("data: {raw}\n\n")))).await;
                failure = Some((
                    Some(code.unwrap_or(502) as i32),
                    format!("upstream in-band error: {message}"),
                ));
                break 'read;
            }
            sse::Frame::Other { json, raw } => {
                if let Some(j) = &json {
                    let (p, c) = sse::frame_usage(kind, j);
                    prompt_tokens = prompt_tokens.or(p);
                    completion_tokens = completion_tokens.or(c);
                }
                match kind {
                    ProviderKind::OpenAI | ProviderKind::Custom => {
                        let wire = sse::SseFrame {
                            event: String::new(),
                            data: raw.to_string(),
                        }
                        .to_wire();
                        if tx.send(Ok(Bytes::from(wire))).await.is_err() {
                            return; // client disconnected; stream is over
                        }
                        // The upstream already framed its own first chunk
                        // (including the role delta), so there is nothing left
                        // for us to inject.
                        first_chunk = false;
                    }
                    // Never reached: Responses upstreams are served non-streamed.
                    ProviderKind::OpenAIResponses => {}
                    ProviderKind::Anthropic | ProviderKind::Google => {
                        if let Some(ev) = crate::adapter::outbound::parse_stream_chunk(kind, raw) {
                            if let Some(chunk) =
                                sse::render_openai_chunk(&model, &completion_id, &ev, first_chunk)
                            {
                                if tx
                                    .send(Ok(Bytes::from(format!("data: {chunk}\n\n"))))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                // Only now has the role-bearing chunk gone out.
                                // Bookkeeping-only events (`message_start`, a bare
                                // usage frame) render nothing and must not consume
                                // the role, or OpenAI clients never see it.
                                first_chunk = false;
                            }
                            if ev.done {
                                let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                                break 'read;
                            }
                        }
                    }
                }
            }
        }
    }

    let latency_ms = started_at.elapsed().as_millis() as i64;
    match failure {
        Some((status_code, message)) => {
            fail_stream_attempt(
                &store,
                &profile_id,
                &target,
                status_code,
                message,
                latency_ms,
            );
        }
        None => {
            log_stream_outcome(
                &store,
                &profile_id,
                &target,
                true,
                Some(200),
                None,
                latency_ms,
                prompt_tokens.map(|t| t as i64),
                completion_tokens.map(|t| t as i64),
            );
        }
    }
}

/// Forward a non-streaming attempt against a Responses upstream, returning the
/// translated chat-completion JSON plus the token counts when the upstream
/// reported usage (`input_tokens`/`output_tokens`).
async fn forward_responses_attempt(
    client: &reqwest::Client,
    target: &crate::router::Target,
    req: &ChatRequest,
    timeout: Duration,
) -> anyhow::Result<(serde_json::Value, Option<i64>, Option<i64>)> {
    let (url, headers, body) =
        crate::adapter::outbound::build_upstream_request(target, req, false)?;
    let mut request = client.post(&url);
    for (k, v) in &headers {
        request = request.header(k, v);
    }
    let resp = tokio::time::timeout(timeout, request.json(&body).send())
        .await
        .map_err(|_| anyhow::anyhow!("upstream timeout"))?
        .map_err(|e| anyhow::anyhow!("upstream failed: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("reading provider response failed: {e}"))?;
    if !status.is_success() {
        return Err(anyhow::Error::new(crate::adapter::outbound::ProviderError {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .context("provider returned an error response"));
    }
    let translated = crate::adapter::outbound::translate_response(target, &bytes)?;
    let completion: serde_json::Value = serde_json::from_slice(&translated)
        .map_err(|e| anyhow::anyhow!("translated response is not JSON: {e}"))?;
    let usage = completion.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_i64());
    let completion_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_i64());
    Ok((completion, prompt_tokens, completion_tokens))
}

/// Wrap a full chat-completion answer into a minimal OpenAI SSE stream so
/// streaming clients can consume a Responses-only upstream: one content chunk,
/// one terminal `stop` chunk, then `[DONE]`.
fn sse_response_from_completion(completion: &serde_json::Value) -> Response {
    let id = completion
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("responses")
        .to_string();
    let model = completion
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let created = completion
        .get("created")
        .cloned()
        .unwrap_or(serde_json::json!(0));
    let content = completion
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let chunk = |delta: serde_json::Value, finish: &str| {
        serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": if finish.is_empty() { serde_json::Value::Null } else { serde_json::json!(finish) },
            }]
        })
        .to_string()
    };
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(
            serde_json::json!({ "role": "assistant", "content": content }),
            ""
        ),
        chunk(serde_json::json!({}), "stop"),
    );
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(axum::body::Body::from(wire))
        .unwrap()
}

/// Translate and prepare the streaming request for a target.
async fn translate_and_forward_streaming(
    client: &reqwest::Client,
    target: &crate::router::Target,
    req: &ChatRequest,
) -> Result<reqwest::Request, anyhow::Error> {
    let (url, headers, body) = crate::adapter::outbound::build_upstream_request(target, req, true)?;
    let mut request = client.post(&url);
    for (k, v) in &headers {
        request = request.header(k, v);
    }
    let reqwest_req = request
        .json(&body)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build request: {e}"))?;
    Ok(reqwest_req)
}

/// Passthrough a non-streaming completions request to the upstream.
async fn handle_completion(
    state: AppState,
    profile_id: String,
    completion_req: CompletionRequest,
) -> Response {
    let targets = match resolve_targets_with_strategy(
        &state.store,
        &profile_id,
        &completion_req.model,
        crate::router::RequestNeeds::default(),
        &state.routing_state,
    ) {
        Ok(t) if t.is_empty() => {
            return service_unavailable("no healthy providers available for this route")
        }
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no proxy named") || msg.contains("no route named") {
                return not_found(msg);
            }
            if msg.contains("must be in") {
                return bad_request(msg);
            }
            return bad_request(format!("route resolution failed: {e}"));
        }
    };

    let result = execute_with_failover(
        state.store.clone(),
        targets,
        state.attempt_timeout,
        |target| {
            let client = state.http_client.clone();
            let req = completion_req.clone();
            let store = state.store.clone();
            let profile_id = profile_id.clone();
            let base = target.provider.base_url.trim_end_matches('/').to_string();
            async move {
                let started = std::time::Instant::now();
                let url = format!("{base}/v1/completions");
                let mut body = serde_json::to_value(&req).map_err(|e| anyhow::anyhow!("{e}"))?;
                if let Some(obj) = body.as_object_mut() {
                    obj.insert(
                        "model".to_string(),
                        serde_json::Value::String(target.entry.model_id.clone()),
                    );
                }
                let resp = client
                    .post(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", target.provider.auth_token),
                    )
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("upstream failed: {e}"))?;
                let status = resp.status();
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to read response: {e}"))?;
                if !status.is_success() {
                    return Err(anyhow::Error::new(
                        crate::adapter::outbound::ProviderError {
                            status,
                            body: String::from_utf8_lossy(&bytes).into_owned(),
                        },
                    ));
                }
                let outcome: Result<Vec<u8>, anyhow::Error> = Ok(bytes.to_vec());
                log_attempt(
                    &store,
                    &profile_id,
                    &target,
                    false,
                    &outcome,
                    started.elapsed().as_millis() as i64,
                );
                outcome
            }
        },
    )
    .await;

    match result {
        Ok(bytes) => Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(bytes))
            .unwrap(),
        Err(e) => bad_request(format!("all providers failed: {e}")),
    }
}

/// Streaming passthrough for `/v1/completions` (SSE), with priority failover.
async fn handle_completion_streaming(
    state: AppState,
    profile_id: String,
    completion_req: CompletionRequest,
) -> Response {
    let targets = match resolve_targets_with_strategy(
        &state.store,
        &profile_id,
        &completion_req.model,
        crate::router::RequestNeeds::default(),
        &state.routing_state,
    ) {
        Ok(t) if t.is_empty() => {
            return service_unavailable("no healthy providers available for this route")
        }
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no proxy named") || msg.contains("no route named") {
                return not_found(msg);
            }
            if msg.contains("must be in") {
                return bad_request(msg);
            }
            return bad_request(format!("route resolution failed: {e}"));
        }
    };

    // Try each upstream in sequence to establish a successful HTTP connection
    let store = state.store.clone();
    let attempt_timeout = state.attempt_timeout;
    let client = state.http_client.clone();

    // Find the first working upstream: send the request for real and only
    // commit to the 200 SSE response once an upstream answers 2xx.
    let mut working: Option<(crate::router::Target, ProbeOutcome)> = None;

    for target in targets {
        let started_at = std::time::Instant::now();
        let base = target.provider.base_url.trim_end_matches('/');
        let url = format!("{base}/v1/completions");

        let mut body = match serde_json::to_value(&completion_req) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(err = %e, "serialization failed, trying next");
                continue;
            }
        };
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "model".to_string(),
                serde_json::Value::String(target.entry.model_id.clone()),
            );
        }

        let resp_result = tokio::time::timeout(
            attempt_timeout,
            client
                .post(&url)
                .header(
                    "Authorization",
                    format!("Bearer {}", target.provider.auth_token),
                )
                .header("Content-Type", "application/json")
                .json(&body)
                .send(),
        )
        .await;

        match resp_result {
            Ok(Ok(resp)) if resp.status().is_success() => {
                // Probe the 2xx body before committing: an in-band SSE error
                // must fail over, not reach the client.
                match probe_stream(state.stream_idle_timeout, resp).await {
                    Probe::Committed {
                        stream,
                        parser,
                        pending,
                    } => {
                        working = Some((
                            target,
                            ProbeOutcome {
                                stream,
                                parser,
                                pending,
                                started_at,
                            },
                        ));
                        break;
                    }
                    Probe::InBandError { code, message } => {
                        fail_stream_attempt(
                            &store,
                            &profile_id,
                            &target,
                            Some(code.unwrap_or(502) as i32),
                            format!("upstream in-band error: {message}"),
                            started_at.elapsed().as_millis() as i64,
                        );
                        continue;
                    }
                    Probe::Failed(msg) | Probe::Transport(msg) => {
                        fail_stream_attempt(
                            &store,
                            &profile_id,
                            &target,
                            None,
                            msg,
                            started_at.elapsed().as_millis() as i64,
                        );
                        continue;
                    }
                    Probe::Timeout => {
                        fail_stream_attempt(
                            &store,
                            &profile_id,
                            &target,
                            Some(504),
                            "upstream stalled before sending any stream data".to_string(),
                            started_at.elapsed().as_millis() as i64,
                        );
                        continue;
                    }
                }
            }
            Ok(Ok(resp)) => {
                // Non-success status, log, demote (provider-side), try next.
                let status = resp.status();
                let bytes = resp.bytes().await.unwrap_or_default();
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    Some(status.as_u16() as i32),
                    format!(
                        "provider returned {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    ),
                    started_at.elapsed().as_millis() as i64,
                );
            }
            Ok(Err(e)) => {
                // Upstream connection failed.
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    None,
                    format!("upstream failed: {e}"),
                    started_at.elapsed().as_millis() as i64,
                );
            }
            Err(_) => {
                // Upstream timed out before sending headers.
                fail_stream_attempt(
                    &store,
                    &profile_id,
                    &target,
                    None,
                    "upstream timeout".to_string(),
                    started_at.elapsed().as_millis() as i64,
                );
            }
        }
    }

    // If no working upstream was found, fail before any 200 is sent.
    let Some((target, outcome)) = working else {
        let error_response = serde_json::json!({
            "error": {
                "message": "All upstream providers failed",
                "type": "provider_error",
                "code": 502,
            }
        });
        return Response::builder()
            .status(axum::http::StatusCode::BAD_GATEWAY)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(error_response.to_string()))
            .unwrap();
    };

    // A working upstream is confirmed: stream its response body to the client.
    // The legacy completions surface is an OpenAI-shaped passthrough, so the
    // pump runs in OpenAI mode regardless of the provider kind.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);

    tokio::spawn(pump_stream(
        target,
        crate::domain::ProviderKind::OpenAI,
        outcome,
        completion_req.model.clone(),
        state.stream_idle_timeout,
        tx,
        store,
        profile_id,
    ));

    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(body)
        .unwrap()
}

/// Non-streaming passthrough for `/v1/embeddings`: resolve targets with
/// priority failover, send the request, and return the upstream's JSON verbatim.
async fn handle_embeddings(
    state: AppState,
    profile_id: String,
    embedding_req: EmbeddingRequest,
) -> Response {
    let targets = match resolve_targets_with_strategy(
        &state.store,
        &profile_id,
        &embedding_req.model,
        crate::router::RequestNeeds::default(),
        &state.routing_state,
    ) {
        Ok(t) if t.is_empty() => {
            return service_unavailable("no healthy providers available for this route")
        }
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no proxy named") || msg.contains("no route named") {
                return not_found(msg);
            }
            if msg.contains("must be in") {
                return bad_request(msg);
            }
            return bad_request(format!("route resolution failed: {e}"));
        }
    };

    let result = execute_with_failover(
        state.store.clone(),
        targets,
        state.attempt_timeout,
        |target| {
            let client = state.http_client.clone();
            let req = embedding_req.clone();
            let store = state.store.clone();
            let profile_id = profile_id.clone();
            let base = target.provider.base_url.trim_end_matches('/').to_string();
            async move {
                let started = std::time::Instant::now();
                let url = format!("{base}/v1/embeddings");
                let mut body = serde_json::to_value(&req).map_err(|e| anyhow::anyhow!("{e}"))?;
                if let Some(obj) = body.as_object_mut() {
                    obj.insert(
                        "model".to_string(),
                        serde_json::Value::String(target.entry.model_id.clone()),
                    );
                }
                let resp = client
                    .post(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", target.provider.auth_token),
                    )
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("upstream failed: {e}"))?;
                let status = resp.status();
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to read response: {e}"))?;
                if !status.is_success() {
                    return Err(anyhow::Error::new(
                        crate::adapter::outbound::ProviderError {
                            status,
                            body: String::from_utf8_lossy(&bytes).into_owned(),
                        },
                    ));
                }
                let outcome: Result<Vec<u8>, anyhow::Error> = Ok(bytes.to_vec());
                log_attempt(
                    &store,
                    &profile_id,
                    &target,
                    false,
                    &outcome,
                    started.elapsed().as_millis() as i64,
                );
                outcome
            }
        },
    )
    .await;

    match result {
        Ok(bytes) => Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(bytes))
            .unwrap(),
        Err(e) => bad_request(format!("all providers failed: {e}")),
    }
}

pub async fn list_models(State(state): State<AppState>, req: Request) -> Response {
    let token = req
        .extensions()
        .get::<String>()
        .cloned()
        .unwrap_or_default();
    let profile = match state.store.get_profile_by_id(&token).ok().flatten() {
        Some(p) => p,
        None => return bad_request("invalid profile token"),
    };
    let proxies = match state.store.list_proxies(profile.id.as_str()) {
        Ok(p) => p,
        Err(e) => return bad_request(format!("failed to list proxies: {e}")),
    };
    let mut models = Vec::new();
    let now = chrono::Utc::now().timestamp();
    for proxy in proxies {
        if let Ok(routes) = state.store.list_routes(proxy.id) {
            for route in routes {
                models.push(serde_json::json!({
                    "id": format!("{}/{}", proxy.name, route.name),
                    "object": "model",
                    "created": now,
                    "owned_by": "agos",
                }));
            }
        }
    }
    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({ "object": "list", "data": models }).to_string(),
        ))
        .unwrap()
}

pub(crate) fn bad_request(msg: impl Into<String>) -> Response {
    let body =
        serde_json::json!({ "error": { "message": msg.into(), "type": "invalid_request_error" } });
    axum::response::Response::builder()
        .status(axum::http::StatusCode::BAD_REQUEST)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

/// 404 Not Found — model/route does not exist.
pub(crate) fn not_found(msg: impl Into<String>) -> Response {
    let body = serde_json::json!({ "error": { "message": msg.into(), "type": "not_found_error" } });
    axum::response::Response::builder()
        .status(axum::http::StatusCode::NOT_FOUND)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

/// 503 Service Unavailable — no healthy provider in the route chain.
pub(crate) fn service_unavailable(msg: impl Into<String>) -> Response {
    let body = serde_json::json!({ "error": { "message": msg.into(), "type": "service_unavailable_error" } });
    axum::response::Response::builder()
        .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod economy_tests {
    use super::*;

    fn chat(text: &str) -> ChatRequest {
        ChatRequest {
            model: "prog/r".into(),
            messages: vec![crate::translator::Message::text("user", text)],
            stream: false,
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn clamp_sets_ceiling_when_extra_is_null() {
        let mut req = chat("hi");
        clamp_chat_max_tokens(&mut req, 1024);
        assert_eq!(req.extra["max_tokens"], 1024);
        // Higher values are clamped down; lower values pass through.
        req.extra = serde_json::json!({"max_tokens": 4096});
        clamp_chat_max_tokens(&mut req, 1024);
        assert_eq!(req.extra["max_tokens"], 1024);
        req.extra = serde_json::json!({"max_tokens": 10});
        clamp_chat_max_tokens(&mut req, 1024);
        assert_eq!(req.extra["max_tokens"], 10);
    }

    #[test]
    fn cache_hash_accepts_deterministic_and_rejects_the_rest() {
        let base = chat("hi");
        assert!(cache_hash("p", "prog/r", &base).is_some());
        // temperature 0 and explicit null temperature are still deterministic.
        let mut t0 = base.clone();
        t0.extra = serde_json::json!({"temperature": 0.0});
        assert!(cache_hash("p", "prog/r", &t0).is_some());
        let mut tn = base.clone();
        tn.extra = serde_json::json!({"temperature": null});
        assert!(cache_hash("p", "prog/r", &tn).is_some());
        // temperature > 0, tools, and streaming bypass the cache.
        let mut hot = base.clone();
        hot.extra = serde_json::json!({"temperature": 0.7});
        assert!(cache_hash("p", "prog/r", &hot).is_none());
        let mut tools = base.clone();
        tools.extra = serde_json::json!({"tools": [{"type": "function"}]});
        assert!(cache_hash("p", "prog/r", &tools).is_none());
        let mut stream = base.clone();
        stream.stream = true;
        assert!(cache_hash("p", "prog/r", &stream).is_none());
        // The escalation flag never changes the key (it bypasses the cache).
        let mut esc = base.clone();
        esc.extra = serde_json::json!({"economy_escalate": true});
        assert_eq!(
            cache_hash("p", "prog/r", &base),
            cache_hash("p", "prog/r", &esc)
        );
    }

    #[test]
    fn image_rejecting_4xx_demotes_the_streaming_entry() {
        use crate::domain::{ModelStatus, ProviderKind, RoutingStrategy};
        use crate::storage::NewProvider;

        let store = Arc::new(Store::open_in_memory().unwrap());
        let profile = store.create_profile("stream-test", None, None).unwrap();
        let provider = store
            .create_provider(
                &profile.id,
                NewProvider {
                    name: "mock".into(),
                    description: None,
                    base_url: "http://unused.invalid".into(),
                    auth_token: "unused".into(),
                    kind: ProviderKind::OpenAI,
                    extra_headers: Default::default(),
                },
            )
            .unwrap();
        let proxy = store.create_proxy(&profile.id, "proxy", None).unwrap();
        let route = store
            .create_route(proxy.id, "route", None, RoutingStrategy::Priority, None)
            .unwrap();
        store
            .add_route_entry(route.id, provider.id, "model", 1, 1.0, Default::default())
            .unwrap();
        let target = crate::router::resolve_targets(&store, &profile.id, "proxy/route")
            .unwrap()
            .remove(0);

        for (code, message, expected) in [
            (
                Some(400),
                r#"{"error":{"message":"unsupported parameter"},"request":{"image_url":"x"}}"#,
                ModelStatus::Healthy,
            ),
            (
                Some(422),
                "cannot decode image: corrupt data",
                ModelStatus::Healthy,
            ),
            (
                Some(400),
                r#"{"error":{"message":"image input not supported"}}"#,
                ModelStatus::Unhealthy,
            ),
            (Some(429), "rate limited", ModelStatus::Degraded),
            (Some(503), "overloaded", ModelStatus::Unhealthy),
            (None, "upstream timeout", ModelStatus::Unhealthy),
        ] {
            store
                .set_route_entry_status(target.entry.id, ModelStatus::Healthy)
                .unwrap();
            fail_stream_attempt(&store, &profile.id, &target, code, message.into(), 1);
            let entries = store.route_entries(route.id).unwrap();
            assert_eq!(entries[0].status, expected, "{message}");
        }
    }
}
