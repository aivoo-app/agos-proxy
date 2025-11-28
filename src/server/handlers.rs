//! HTTP handlers for the OpenAI-compatible surface.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::response::Response;

use crate::router::{execute_with_failover, resolve_targets_with_strategy, RoutingState};
use crate::storage::Store;
use crate::translator::{self, ChatRequest};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub attempt_timeout: Duration,
    pub http_client: reqwest::Client,
    pub routing_state: RoutingState,
}

pub async fn chat_completions(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let profile_id = match parts.extensions.get::<String>() {
        Some(id) => id.clone(),
        None => return bad_request("unauthenticated"),
    };
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => return bad_request(format!("failed to read body: {e}")),
    };
    let body_value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    let needs = crate::router::RequestNeeds::from_body(&body_value);
    let chat_req: ChatRequest = match serde_json::from_value(body_value) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    if chat_req.stream {
        handle_streaming(state, profile_id, needs, chat_req).await
    } else {
        handle_non_streaming(state, profile_id, needs, chat_req).await
    }
}

async fn handle_non_streaming(
    state: AppState,
    profile_id: String,
    needs: crate::router::RequestNeeds,
    chat_req: ChatRequest,
) -> Response {
    let targets = match resolve_targets_with_strategy(
        &state.store,
        &profile_id,
        &chat_req.model,
        needs,
        &state.routing_state,
    ) {
        Ok(t) if t.is_empty() => return bad_request("no healthy targets for this route"),
        Ok(t) => t,
        Err(e) => return bad_request(format!("route resolution failed: {e}")),
    };
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
                let outcome = translator::forward_non_streaming(&client, &target, &req).await;
                let latency_ms = started.elapsed().as_millis() as i64;
                log_attempt(&store, &profile_id, &target, false, &outcome, latency_ms);
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
        Err(e) => {
            let body = serde_json::json!({ "error": { "message": format!("all providers failed: {e}"), "type": "provider_error" } });
            axum::response::Response::builder()
                .status(axum::http::StatusCode::BAD_GATEWAY)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        }
    }
}

/// Write one usage-log row for a completed attempt. Failures to log are
/// swallowed — telemetry must never break request handling.
fn log_attempt(
    store: &Store,
    profile_id: &str,
    target: &crate::router::Target,
    streamed: bool,
    outcome: &anyhow::Result<Vec<u8>>,
    latency_ms: i64,
) {
    let (success, status_code, error_message, prompt_tokens, completion_tokens) = match outcome {
        Ok(bytes) => {
            // Best-effort token extraction from the OpenAI-shaped response.
            let (pt, ct) = serde_json::from_slice::<serde_json::Value>(bytes)
                .ok()
                .and_then(|v| {
                    let u = v.get("usage")?;
                    Some((
                        u.get("prompt_tokens").and_then(|t| t.as_i64()),
                        u.get("completion_tokens").and_then(|t| t.as_i64()),
                    ))
                })
                .unwrap_or((None, None));
            (true, Some(200), None, pt, ct)
        }
        Err(e) => {
            let status = e
                .downcast_ref::<crate::translator::ProviderError>()
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
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let store = state.store.clone();
    let attempt_timeout = state.attempt_timeout;
    let client = state.http_client.clone();

    tokio::spawn(async move {
        let targets = match resolve_targets_with_strategy(
            &store,
            &profile_id,
            &model,
            needs,
            &state.routing_state,
        ) {
            Ok(t) if t.is_empty() => {
                let _ = tx
                    .send(Err(std::io::Error::other("no healthy targets")))
                    .await;
                return;
            }
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                return;
            }
        };
        for target in targets {
            let req = {
                let mut r = chat_req.clone();
                r.stream = true;
                r
            };
            let (url, headers, body) = match translator::build_upstream_request(&target, &req, true)
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "build upstream failed");
                    continue;
                }
            };
            let mut rb = client.post(&url);
            for (k, v) in &headers {
                rb = rb.header(k, v);
            }
            let started = std::time::Instant::now();
            let resp = match tokio::time::timeout(attempt_timeout, rb.json(&body).send()).await {
                Ok(Ok(r)) if r.status().is_success() => r,
                Ok(Ok(r)) => {
                    let status = r.status();
                    let bytes = r.bytes().await.unwrap_or_default();
                    log_attempt(
                        &store,
                        &profile_id,
                        &target,
                        true,
                        &Err::<Vec<u8>, _>(anyhow::anyhow!(
                            "provider returned {}: {}",
                            status,
                            String::from_utf8_lossy(&bytes)
                        )),
                        started.elapsed().as_millis() as i64,
                    );
                    tracing::warn!(status = %status, "upstream error");
                    continue;
                }
                Ok(Err(e)) => {
                    log_attempt(
                        &store,
                        &profile_id,
                        &target,
                        true,
                        &Err::<Vec<u8>, _>(anyhow::anyhow!("upstream failed: {e}")),
                        started.elapsed().as_millis() as i64,
                    );
                    tracing::warn!(error = %e, "upstream failed");
                    continue;
                }
                Err(_) => {
                    tracing::warn!("upstream timed out");
                    let _ = store.set_route_entry_status(
                        target.entry.id,
                        crate::domain::ModelStatus::Unhealthy,
                    );
                    log_attempt(
                        &store,
                        &profile_id,
                        &target,
                        true,
                        &Err::<Vec<u8>, _>(anyhow::anyhow!("upstream timed out")),
                        started.elapsed().as_millis() as i64,
                    );
                    continue;
                }
            };
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
                match chunk {
                    Ok(bytes) => {
                        if tx.send(Ok(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                        return;
                    }
                }
            }
            // Stream completed successfully. Token counts for streamed responses
            // only appear when the caller asks for stream_options.include_usage,
            // so we record the latency and leave counts empty.
            log_attempt(
                &store,
                &profile_id,
                &target,
                true,
                &Ok(Vec::new()),
                started.elapsed().as_millis() as i64,
            );
            return;
        }
        let _ = tx
            .send(Err(std::io::Error::other("all providers failed")))
            .await;
    });

    // Return the raw byte stream with SSE content type for true passthrough.
    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(body)
        .unwrap()
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
    for proxy in proxies {
        if let Ok(routes) = state.store.list_routes(proxy.id) {
            for route in routes {
                models.push(serde_json::json!({
                    "id": format!("{}/{}", proxy.name, route.name),
                    "object": "model",
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

fn bad_request(msg: impl Into<String>) -> Response {
    let body =
        serde_json::json!({ "error": { "message": msg.into(), "type": "invalid_request_error" } });
    axum::response::Response::builder()
        .status(axum::http::StatusCode::BAD_REQUEST)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}
