//! Native multi-adapter inbound surfaces (Anthropic, Gemini, the namespaced
//! OpenAI chat route, and the Responses API for the Codex CLI).
//!
//! Every handler here runs the same canonical pipeline:
//!
//! ```text
//! native request -> ChatRequest -> resolve -> failover -> canonical result
//!   -> render native response (JSON or SSE)
//! ```
//!
//! Streaming re-encodes each upstream SSE chunk into the inbound surface's own
//! SSE framing, so a Claude or Gemini outbound stream is served to an OpenAI,
//! Anthropic, Gemini, or Codex client correctly.

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::response::Response;
use futures::StreamExt;

use crate::adapter::outbound;
use crate::adapter::{ApiKind, StreamRenderer};
use crate::router::Target;
use crate::router::{execute_with_failover, resolve_targets_with_strategy, RequestNeeds};
use crate::server::handlers::{
    bad_request, inject_identity_into_messages, log_attempt, not_found, service_unavailable,
    AppState,
};
use crate::translator::{ChatRequest, StreamEvent};
use anyhow;

/// Per-handler body size cap, mirroring the OpenAI handler.
const BODY_LIMIT: usize = 5 * 1024 * 1024;

/// OpenAI-compatible chat surface mounted under `/openai/v1/chat/completions`.
pub async fn openai_chat(State(state): State<AppState>, req: Request) -> Response {
    native_chat(state, ApiKind::OpenAI, req).await
}

/// Anthropic messages surface: `/anthropic/v1/messages`.
pub async fn anthropic_messages(State(state): State<AppState>, req: Request) -> Response {
    native_chat(state, ApiKind::Anthropic, req).await
}

/// Gemini generateContent surface: `/google/v1beta/models/{model}:generateContent`
/// and `:streamGenerateContent` (both are wildcard-routed to this handler).
pub async fn google_generate(State(state): State<AppState>, req: Request) -> Response {
    native_chat(state, ApiKind::Google, req).await
}

/// Codex Responses surface: `POST /codex/v1/responses`.
///
/// The Codex CLI only speaks the OpenAI Responses API (its
/// `wire_api = "chat"` mode was removed upstream), so this is the endpoint a
/// Codex client points its provider `base_url` at.
pub async fn codex_responses(State(state): State<AppState>, req: Request) -> Response {
    native_chat(state, ApiKind::Responses, req).await
}

async fn native_chat(state: AppState, kind: ApiKind, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let profile_id = match parts.extensions.get::<String>() {
        Some(id) => id.clone(),
        None => return bad_request("unauthenticated"),
    };
    let bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return bad_request(format!("failed to read body: {e}")),
    };
    let mut body_value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };

    // The Gemini SDK carries the model in the URL path, so the handler injects
    // it into the body before the adapter parses the request.
    if kind == ApiKind::Google {
        if let Some(m) = model_from_gemini_path(parts.uri.path()) {
            if let Some(obj) = body_value.as_object_mut() {
                obj.insert("model".to_string(), serde_json::Value::String(m));
            }
        }
    }

    let chat_req = match state.adapter.parse_request(kind, &body_value) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid request body: {e}")),
    };
    // Derive capability needs from the canonical request (after native parsing),
    // so Anthropic/Gemini text arrays are not mistaken for vision content.
    let canonical_value = serde_json::to_value(&chat_req).unwrap_or(serde_json::Value::Null);
    let needs = RequestNeeds::from_body(&canonical_value);
    if chat_req.stream {
        chat_stream(state, kind, profile_id, needs, chat_req).await
    } else {
        chat_non_stream(state, kind, profile_id, needs, chat_req).await
    }
}

/// Extract the model name from a Gemini `models/{name}:{method}` path, decoding
/// `%2F` so a `proxy/route` model can pass through the single path segment.
fn model_from_gemini_path(path: &str) -> Option<String> {
    let idx = path.find("models/")?;
    let rest = &path[idx + "models/".len()..];
    let model = rest.split(':').next()?.to_string();
    if model.is_empty() {
        None
    } else {
        Some(model.replace("%2F", "/").replace("%2f", "/"))
    }
}

async fn chat_non_stream(
    state: AppState,
    kind: ApiKind,
    profile_id: String,
    needs: RequestNeeds,
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
        Ok(bytes) => {
            // The outbound translator already produced OpenAI-shaped bytes; turn
            // those into the canonical form, then render them in the surface's
            // own native shape. If the payload is not parseable, fall back to
            // sending the OpenAI-shaped bytes through unchanged.
            match outbound::parse_canonical(&bytes) {
                Ok(canon) => {
                    let native = state.adapter.render_response(kind, &canon);
                    Response::builder()
                        .status(200)
                        .header("Content-Type", "application/json")
                        .body(axum::body::Body::from(native.to_string()))
                        .unwrap()
                }
                Err(_) => Response::builder()
                    .status(200)
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(bytes))
                    .unwrap(),
            }
        }
        Err(e) => bad_request(format!("all providers failed: {e}")),
    }
}

async fn chat_stream(
    state: AppState,
    kind: ApiKind,
    profile_id: String,
    needs: RequestNeeds,
    chat_req: ChatRequest,
) -> Response {
    let model = chat_req.model.clone();
    let store = state.store.clone();
    let attempt_timeout = state.attempt_timeout;
    let client = state.http_client.clone();
    let routing_state = state.routing_state.clone();

    // Try each upstream in sequence to establish a successful HTTP connection
    let targets =
        match resolve_targets_with_strategy(&store, &profile_id, &model, needs, &routing_state) {
            Ok(t) if t.is_empty() => {
                return service_unavailable("no healthy providers available for this route");
            }
            Ok(t) => t,
            Err(e) => {
                return bad_request(format!("route resolution failed: {e}"));
            }
        };

    // Find the first working upstream: send the request for real and only
    // commit to the 200 SSE response once an upstream answers 2xx.
    let mut working: Option<(Target, reqwest::Response, std::time::Instant)> = None;

    for target in targets {
        let started_at = std::time::Instant::now();
        // One renderer per attempt: the Responses surface accumulates text
        // and tool arguments, and that state must not leak into a retry.
        let _renderer = state.adapter.stream_renderer(kind);
        let (url, headers, body) = match outbound::build_upstream_request(&target, &chat_req, true)
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(err = %e, "build upstream request failed, trying next");
                continue;
            }
        };
        let mut request = client.post(&url);
        for (k, v) in &headers {
            request = request.header(k, v);
        }

        match tokio::time::timeout(attempt_timeout, request.json(&body).send()).await {
            Ok(Ok(resp)) if resp.status().is_success() => {
                // Found a working upstream; keep the response and stream it.
                working = Some((target, resp, started_at));
                break;
            }
            Ok(Ok(resp)) => {
                // Non-success status, log and try the next target.
                let status = resp.status();
                let _bytes = resp.bytes().await.unwrap_or_default();
                let msg = format!("provider returned {status}");
                log_attempt(
                    &store,
                    &profile_id,
                    &target,
                    true,
                    &Err(anyhow::anyhow!("{msg}")),
                    started_at.elapsed().as_millis() as i64,
                );
            }
            Ok(Err(e)) => {
                // Upstream connection failed.
                log_attempt(
                    &store,
                    &profile_id,
                    &target,
                    true,
                    &Err(anyhow::anyhow!("upstream failed: {e}")),
                    started_at.elapsed().as_millis() as i64,
                );
            }
            Err(_) => {
                // Upstream timed out.
                log_attempt(
                    &store,
                    &profile_id,
                    &target,
                    true,
                    &Err(anyhow::anyhow!("upstream timeout")),
                    started_at.elapsed().as_millis() as i64,
                );
            }
        }
    }

    // If no working upstream was found, fail before any 200 is sent.
    let Some((target, resp, started)) = working else {
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

    // A working upstream is confirmed: stream its SSE lines through the
    // surface renderer.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let store_clone = store.clone();
    let profile_id_clone = profile_id.clone();

    tokio::spawn(async move {
        let mut renderer = state.adapter.stream_renderer(kind);
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut saw_terminal = false;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buf.find('\n') {
                        let line = buf.drain(..=pos).collect::<String>();
                        if let Some((frame, done)) = decode_line(&target, &line, renderer.as_mut())
                        {
                            saw_terminal |= done;
                            let _ = tx.send(Ok(Bytes::from(frame))).await;
                        }
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
        // Flush any trailing partial line.
        if !saw_terminal {
            if let Some((frame, done)) = decode_line(&target, &buf, renderer.as_mut()) {
                let _ = tx.send(Ok(Bytes::from(frame))).await;
                saw_terminal |= done;
            }
        }
        // A surface that needs a closing frame (OpenAI's `data: [DONE]`, or
        // the Responses API's mandatory `response.completed`) gets its
        // chance here, when the upstream ended without a terminal event.
        if !saw_terminal {
            if let Some(marker) = renderer.finish("chatcmpl-agos") {
                let _ = tx.send(Ok(Bytes::from(marker))).await;
            }
        }
        log_attempt(
            &store_clone,
            &profile_id_clone,
            &target,
            true,
            &Ok(Vec::new()),
            started.elapsed().as_millis() as i64,
        );
    });

    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(body)
        .unwrap()
}

/// Decode a raw stream line against the outbound provider kind and re-encode it
/// as native SSE frames on the inbound surface. Returns `(frames, done)` where
/// `done` reports whether the decoded event terminates the stream.
fn decode_line(
    target: &Target,
    line: &str,
    renderer: &mut dyn StreamRenderer,
) -> Option<(String, bool)> {
    let line = line.trim();
    if !line.starts_with("data:") {
        return None;
    }
    let payload = line.trim_start_matches("data:").trim();
    if payload == "[DONE]" {
        return None;
    }
    let ev: Option<StreamEvent> = outbound::parse_stream_chunk(target.provider.kind, payload);
    let ev = ev?;
    let done = ev.done || ev.finish_reason.is_some();
    renderer.render(&ev, "chatcmpl-agos").map(|f| (f, done))
}

/// List the routes a profile can reach, in the Anthropic models response shape.
pub async fn anthropic_models(State(state): State<AppState>, req: Request) -> Response {
    let token = req
        .extensions()
        .get::<String>()
        .cloned()
        .unwrap_or_default();
    let profile = match state.store.get_profile_by_id(&token).ok().flatten() {
        Some(p) => p,
        None => return bad_request("invalid profile token"),
    };
    match list_native_models(&state, &profile.id) {
        Ok(ids) => {
            let resp = serde_json::json!({
                "data": ids.iter().map(|id| serde_json::json!({
                    "type": "model",
                    "id": id,
                    "display_name": id,
                    "created_at": "2024-01-01T00:00:00Z",
                })).collect::<Vec<_>>()
            });
            json_response(200, &resp)
        }
        Err(e) => bad_request(format!("failed to list models: {e}")),
    }
}

/// List the routes a profile can reach, in the Gemini models response shape.
pub async fn google_models(State(state): State<AppState>, req: Request) -> Response {
    let token = req
        .extensions()
        .get::<String>()
        .cloned()
        .unwrap_or_default();
    let profile = match state.store.get_profile_by_id(&token).ok().flatten() {
        Some(p) => p,
        None => return bad_request("invalid profile token"),
    };
    match list_native_models(&state, &profile.id) {
        Ok(ids) => {
            let resp = serde_json::json!({
                "models": ids.iter().map(|id| serde_json::json!({
                    "name": format!("models/{id}"),
                    "displayName": id,
                })).collect::<Vec<_>>()
            });
            json_response(200, &resp)
        }
        Err(e) => bad_request(format!("failed to list models: {e}")),
    }
}

fn list_native_models(state: &AppState, profile_id: &str) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    for proxy in state.store.list_proxies(profile_id)? {
        for route in state.store.list_routes(proxy.id)? {
            ids.push(format!("{}/{}", proxy.name, route.name));
        }
    }
    Ok(ids)
}

fn json_response(status: u16, value: &serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(value.to_string()))
        .unwrap()
}
