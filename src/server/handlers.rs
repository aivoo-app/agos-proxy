//! HTTP handlers for the OpenAI-compatible surface.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::response::Response;
use futures::StreamExt;

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
    let bytes = match axum::body::to_bytes(body, HANDLER_BODY_LIMIT).await {
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
        crate::translator::Message {
            role: "system".to_string(),
            content: serde_json::Value::String(system_content),
        },
    );
}

async fn handle_non_streaming(
    state: AppState,
    profile_id: String,
    needs: crate::router::RequestNeeds,
    mut chat_req: ChatRequest,
) -> Response {
    let model = chat_req.model.clone();
    let targets = match resolve_targets_with_strategy(
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
    // Inject identity system message if the route has one
    if let Some(identity) = targets.first().and_then(|t| t.identity.as_deref()) {
        inject_identity_into_messages(&mut chat_req.messages, identity);
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
        Ok(bytes) => Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(bytes))
            .unwrap(),
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
                    .send(Err(std::io::Error::other(
                        "no healthy providers available for this route",
                    )))
                    .await;
                return;
            }
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                let err_msg = if msg.contains("no proxy named") || msg.contains("no route named") {
                    format!("model not found: {msg}")
                } else if msg.contains("must be in") {
                    msg
                } else {
                    format!("route resolution failed: {msg}")
                };
                let _ = tx.send(Err(std::io::Error::other(err_msg))).await;
                return;
            }
        };
        for target in targets {
            let req = {
                let target = target.clone();
                translate_and_forward_streaming(&client, &target, &chat_req).await
            };
            match req {
                Ok(stream_req) => {
                    let started = std::time::Instant::now();
                    let resp =
                        match tokio::time::timeout(attempt_timeout, client.execute(stream_req))
                            .await
                        {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => {
                                let _ = tx
                                    .send(Err(std::io::Error::other(format!(
                                        "upstream failed: {e}"
                                    ))))
                                    .await;
                                log_attempt(
                                    &store,
                                    &profile_id,
                                    &target,
                                    true,
                                    &Err(anyhow::anyhow!("upstream failed: {e}")),
                                    started.elapsed().as_millis() as i64,
                                );
                                continue;
                            }
                            Err(_) => {
                                let _ = tx
                                    .send(Err(std::io::Error::other("upstream timeout")))
                                    .await;
                                log_attempt(
                                    &store,
                                    &profile_id,
                                    &target,
                                    true,
                                    &Err(anyhow::anyhow!("upstream timeout")),
                                    started.elapsed().as_millis() as i64,
                                );
                                continue;
                            }
                        };

                    if !resp.status().is_success() {
                        let status = resp.status();
                        let bytes = resp.bytes().await.unwrap_or_default();
                        let _ = tx
                            .send(Err(std::io::Error::other(format!(
                                "provider returned {}: {}",
                                status,
                                String::from_utf8_lossy(&bytes)
                            ))))
                            .await;
                        log_attempt(
                            &store,
                            &profile_id,
                            &target,
                            true,
                            &Err(anyhow::anyhow!(
                                "provider returned {}: {}",
                                status,
                                String::from_utf8_lossy(&bytes)
                            )),
                            started.elapsed().as_millis() as i64,
                        );
                        continue;
                    }

                    let mut stream = resp.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(bytes) => {
                                if tx.send(Ok(bytes)).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = tx
                                    .send(Err(std::io::Error::other(format!("stream error: {e}"))))
                                    .await;
                                log_attempt(
                                    &store,
                                    &profile_id,
                                    &target,
                                    true,
                                    &Err(anyhow::anyhow!("stream error: {e}")),
                                    started.elapsed().as_millis() as i64,
                                );
                                return;
                            }
                        }
                    }

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
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    tracing::warn!(err = %e, "translate request failed, trying next");
                    continue;
                }
            }
        }
        let _ = tx
            .send(Err(std::io::Error::other("all providers failed")))
            .await;
    });

    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(body)
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
                    return Err(anyhow::anyhow!(
                        "provider returned {}: {}",
                        status,
                        String::from_utf8_lossy(&bytes)
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

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let store = state.store.clone();
    let attempt_timeout = state.attempt_timeout;
    let client = state.http_client.clone();

    tokio::spawn(async move {
        for target in targets {
            let started = std::time::Instant::now();
            let req = completion_req.clone();
            let base = target.provider.base_url.trim_end_matches('/');
            let url = format!("{base}/v1/completions");

            let mut body = match serde_json::to_value(&req) {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx
                        .send(Err(std::io::Error::other(format!(
                            "serialization failed: {e}"
                        ))))
                        .await;
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

            let resp = match resp_result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    let _ = tx
                        .send(Err(std::io::Error::other(format!("upstream failed: {e}"))))
                        .await;
                    log_attempt(
                        &store,
                        &profile_id,
                        &target,
                        true,
                        &Err(anyhow::anyhow!("upstream failed: {e}")),
                        started.elapsed().as_millis() as i64,
                    );
                    continue;
                }
                Err(_) => {
                    let _ = tx
                        .send(Err(std::io::Error::other("upstream timeout")))
                        .await;
                    log_attempt(
                        &store,
                        &profile_id,
                        &target,
                        true,
                        &Err(anyhow::anyhow!("upstream timeout")),
                        started.elapsed().as_millis() as i64,
                    );
                    continue;
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let bytes = resp.bytes().await.unwrap_or_default();
                let _ = tx
                    .send(Err(std::io::Error::other(format!(
                        "provider returned {}: {}",
                        status,
                        String::from_utf8_lossy(&bytes)
                    ))))
                    .await;
                log_attempt(
                    &store,
                    &profile_id,
                    &target,
                    true,
                    &Err(anyhow::anyhow!(
                        "provider returned {}: {}",
                        status,
                        String::from_utf8_lossy(&bytes)
                    )),
                    started.elapsed().as_millis() as i64,
                );
                continue;
            }

            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        if tx.send(Ok(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(std::io::Error::other(format!("stream error: {e}"))))
                            .await;
                        return;
                    }
                }
            }

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
                    return Err(anyhow::anyhow!(
                        "provider returned {}: {}",
                        status,
                        String::from_utf8_lossy(&bytes)
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
