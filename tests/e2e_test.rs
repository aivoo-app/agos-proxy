//! End-to-end tests: spin up the full axum app against a mock upstream and
//! exercise the request lifecycle.

use std::sync::Arc;
use std::time::Duration;

use agos::domain::{ProviderKind, RouteCapabilities, RoutingStrategy};
use agos::server::{create_app, AppState};
use agos::storage::Store;
use tower::util::ServiceExt;

/// Start a mock upstream that echoes a fixed chat-completions response.
async fn mock_upstream(port: u16) -> tokio::task::JoinHandle<()> {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|body: axum::Json<serde_json::Value>| async move {
            let streaming = body
                .0
                .get("stream")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            let wants_tool = body
                .0
                .get("messages")
                .and_then(|m| m.as_array())
                .map(|msgs| {
                    msgs.iter()
                        .any(|m| m.get("content").and_then(|c| c.as_str()) == Some("call the tool"))
                })
                .unwrap_or(false);

            if streaming {
                let (delta, finish) = if wants_tool {
                    (
                        serde_json::json!({
                            "role": "assistant",
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_mock_1",
                                "type": "function",
                                "function": { "name": "sh", "arguments": "{\"cmd\":\"ls\"}" }
                            }]
                        }),
                        "tool_calls",
                    )
                } else {
                    (
                        serde_json::json!({ "role": "assistant", "content": "hel" }),
                        "stop",
                    )
                };
                let frame = |choice: serde_json::Value, finish_reason: Option<&str>| {
                    let chunk = serde_json::json!({
                        "id": "mock-1",
                        "object": "chat.completion.chunk",
                        "choices": [{
                            "index": 0,
                            "delta": choice,
                            "finish_reason": finish_reason,
                        }]
                    });
                    format!("data: {chunk}\n\n")
                };
                let sse = format!(
                    "{}{}{}data: [DONE]\n\n",
                    frame(delta, None),
                    frame(serde_json::json!({ "content": "lo from mock" }), None),
                    frame(serde_json::json!({}), Some(finish)),
                );
                return (
                    axum::http::StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                );
            }

            let message = if wants_tool {
                serde_json::json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_mock_1",
                        "type": "function",
                        "function": { "name": "sh", "arguments": "{\"cmd\":\"ls\"}" }
                    }]
                })
            } else {
                serde_json::json!({ "role": "assistant", "content": "hello from mock" })
            };
            let finish = if wants_tool { "tool_calls" } else { "stop" };
            let body = serde_json::json!({
                "id": "mock-1",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": finish
                }]
            });
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body.to_string(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("bind mock upstream");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })
}

fn test_state(store: Store) -> AppState {
    AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(5),
        stream_idle_timeout: Duration::from_secs(5),
        http_client: reqwest::Client::new(),
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    }
}

fn setup_store(base_url: &str) -> (Store, String) {
    setup_store_kind(base_url, ProviderKind::OpenAICompatible)
}

/// Same tree as [`setup_store`], but the provider is created with the given
/// kind — used by the OpenAI Responses upstream tests.
fn setup_store_kind(base_url: &str, kind: ProviderKind) -> (Store, String) {
    let store = Store::open_in_memory().expect("open in-memory store");
    let profile = store
        .create_profile("coder1", Some("test profile"), None)
        .expect("create profile");
    let profile_id = profile.id.clone();

    store
        .create_provider(
            &profile_id,
            agos::storage::NewProvider {
                name: "mock-provider".into(),
                description: None,
                base_url: base_url.into(),
                auth_token: "sk-mock".into(),
                kind,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )
        .expect("create provider");

    let providers = store.list_providers(&profile_id).expect("list providers");
    let provider_id = providers[0].id;

    let proxy = store
        .create_proxy(&profile_id, "programmer", Some("dev proxy"))
        .expect("create proxy");
    let route = store
        .create_route(proxy.id, "php-dev", None, RoutingStrategy::Priority, None)
        .expect("create route");
    store
        .add_route_entry(
            route.id,
            provider_id,
            "mock-model",
            1,
            1.0,
            RouteCapabilities {
                tools: true,
                vision: false,
                json_mode: false,
                max_context: None,
            },
        )
        .expect("add route entry");

    (store, profile_id)
}

#[tokio::test]
async fn chat_completions_routes_through_mock_upstream() {
    let mock_port = 19876;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build client");
    let state = AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(5),
        stream_idle_timeout: Duration::from_secs(5),
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    };
    let app = create_app(state);

    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", format!("Bearer {profile_id}"))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(
        json["choices"][0]["message"]["content"], "hello from mock",
        "upstream response should be passed through"
    );
}

#[tokio::test]
async fn chat_completions_rejects_missing_auth() {
    let mock_port = 19877;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, _profile_id) = setup_store(&mock_base);
    let http_client = reqwest::Client::new();
    let state = AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(5),
        stream_idle_timeout: Duration::from_secs(5),
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    };
    let app = create_app(state);

    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(
        response.status(),
        401,
        "missing bearer token should be rejected"
    );
}

#[tokio::test]
async fn list_models_returns_caller_routes() {
    let mock_port = 19878;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let http_client = reqwest::Client::new();
    let state = AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(5),
        stream_idle_timeout: Duration::from_secs(5),
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    };
    let app = create_app(state);

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("Authorization", format!("Bearer {profile_id}"))
        .body(axum::body::Body::empty())
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    let models = json["data"].as_array().expect("data array");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "programmer/php-dev");
}

/// An Anthropic client can address the proxy through `/anthropic/v1/messages`
/// using its native `x-api-key` header; AGOS translates the request out and the
/// response back into Anthropic's message shape.
#[tokio::test]
async fn anthropic_surface_translates_to_anthropic_shape() {
    let mock_port = 19879;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let http_client = reqwest::Client::new();
    let state = AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(5),
        stream_idle_timeout: Duration::from_secs(5),
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    };
    let app = create_app(state);

    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "system": "be terse",
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "hi" }] }
        ],
        "max_tokens": 128,
        "stream": false
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/anthropic/v1/messages")
        .header("x-api-key", &profile_id)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(json["type"], "message");
    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "hello from mock");
    assert_eq!(json["stop_reason"], "end_turn");
    assert_eq!(json["usage"]["input_tokens"], 0);
}

/// The Gemini surface is served under `/google/v1beta/models/{model}:generateContent`
/// and authenticated with a `key=` query parameter. As with the other surfaces the
/// response is translated back into Gemini's native shape.
#[tokio::test]
async fn google_surface_translates_to_gemini_shape() {
    let mock_port = 19880;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let http_client = reqwest::Client::new();
    let state = AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(5),
        stream_idle_timeout: Duration::from_secs(5),
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    };
    let app = create_app(state);

    let req_body = serde_json::json!({
        "contents": [
            { "role": "user", "parts": [{ "text": "hi" }] }
        ],
        "generationConfig": { "maxOutputTokens": 64 }
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(format!(
            "/google/v1beta/models/programmer%2Fphp-dev:generateContent?key={profile_id}"
        ))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(
        json["candidates"][0]["content"]["parts"][0]["text"],
        "hello from mock"
    );
    assert_eq!(json["candidates"][0]["finishReason"], "STOP");
}

/// A Codex client hitting `/codex/v1/chat/completions` is served through the
/// Codex inbound adapter and routed to the upstream via the canonical pipeline.
#[tokio::test]
async fn codex_responses_non_streaming() {
    let mock_port = 19881;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let app = create_app(test_state(store));

    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "instructions": "be terse",
        "input": [{ "type": "message", "role": "user", "content": "write a function" }],
        "stream": false,
        "store": false,
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/codex/v1/responses")
        .header("Authorization", format!("Bearer {profile_id}"))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(json["object"], "response");
    assert_eq!(json["status"], "completed");
    assert_eq!(json["output"][0]["type"], "message");
    assert_eq!(json["output"][0]["content"][0]["text"], "hello from mock");
    assert_eq!(json["usage"]["total_tokens"], 0);
}

#[tokio::test]
async fn codex_responses_stream_emits_completed() {
    let mock_port = 19882;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let app = create_app(test_state(store));

    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "input": [{ "type": "message", "role": "user", "content": "hi" }],
        "stream": true,
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/codex/v1/responses")
        .header("Authorization", format!("Bearer {profile_id}"))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8(body.to_vec()).expect("utf8");

    // The events Codex depends on, in order.
    let created = text.find("response.created").expect("response.created");
    let delta = text.find("response.output_text.delta").expect("text delta");
    let done = text.find("response.output_item.done").expect("item done");
    let completed = text.find("response.completed").expect("response.completed");
    assert!(created < delta && delta < done && done < completed);
    // The accumulated message item carries the full text, not a fragment.
    let item = &text[done..completed];
    assert!(item.contains("hello from mock"), "item: {item}");
}

#[tokio::test]
async fn codex_responses_tool_call_round_trip() {
    let mock_port = 19883;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let app = create_app(test_state(store));

    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "input": [
            { "type": "message", "role": "user", "content": "call the tool" },
        ],
        "tools": [{
            "type": "function",
            "name": "sh",
            "description": "run a shell command",
            "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } } },
        }],
        "stream": false,
    });
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/codex/v1/responses")
        .header("Authorization", format!("Bearer {profile_id}"))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    let call = &json["output"][0];
    assert_eq!(call["type"], "function_call");
    assert_eq!(call["name"], "sh");
    assert_eq!(call["call_id"], "call_mock_1");
    // `arguments` is a JSON string on the wire, which Codex parses itself.
    assert_eq!(call["arguments"], "{\"cmd\":\"ls\"}");
}

/// Economy tier end-to-end: cheap-first routing, `X-Agos-Cache: HIT` on the
/// exact repeat, and escalation forcing the flagship.
#[tokio::test]
async fn economy_routes_cheap_first_caches_and_escalates() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Mock upstream records which model string it was hit with.
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mock_app = {
        let hits = hits.clone();
        let seen = seen.clone();
        axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |body: axum::Json<serde_json::Value>| {
                let hits = hits.clone();
                let seen = seen.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let model = body
                        .0
                        .get("model")
                        .and_then(|m| m.as_str())
                        .unwrap_or("?")
                        .to_string();
                    seen.lock().unwrap().push(model);
                    let resp = serde_json::json!({
                        "id": "mock-eco", "object": "chat.completion",
                        "choices": [{"index": 0,
                            "message": {"role": "assistant", "content": "eco answer"},
                            "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                    });
                    (
                        axum::http::StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        resp.to_string(),
                    )
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:19884")
        .await
        .expect("bind mock");
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Store: one Economy route with a cheap + flagship entry.
    let store = Store::open_in_memory().expect("open store");
    let profile = store.create_profile("eco", None, None).expect("profile");
    let profile_id = profile.id.clone();
    let provider = store
        .create_provider(
            &profile_id,
            agos::storage::NewProvider {
                name: "mock".into(),
                description: None,
                base_url: "http://127.0.0.1:19884".into(),
                auth_token: "sk-mock".into(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )
        .expect("provider");
    let proxy = store
        .create_proxy(&profile_id, "prog", None)
        .expect("proxy");
    let route = store
        .create_route(proxy.id, "eco", None, RoutingStrategy::Economy, None)
        .expect("route");
    store
        .set_route_economy(route.id, 1024, 3600)
        .expect("economy");
    // Insert flagship first — Economy must still try the cheap entry first.
    let flagship = store
        .add_route_entry(
            route.id,
            provider.id,
            "gpt-4o",
            1,
            1.0,
            RouteCapabilities::default(),
        )
        .expect("flagship");
    let cheap = store
        .add_route_entry(
            route.id,
            provider.id,
            "gpt-4o-mini",
            2,
            1.0,
            RouteCapabilities::default(),
        )
        .expect("cheap");
    store
        .set_route_entry_price(flagship.id, 6.0)
        .expect("price");
    store.set_route_entry_price(cheap.id, 0.4).expect("price");

    let app = create_app(test_state(store));
    let build = |body: serde_json::Value, escalate: bool| {
        let mut b = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", format!("Bearer {profile_id}"))
            .header("Content-Type", "application/json");
        if escalate {
            b = b.header("X-Economy-Escalate", "true");
        }
        b.body(axum::body::Body::from(body.to_string()))
            .expect("request")
    };
    let meta = |resp: axum::response::Response| async move {
        let cache = resp
            .headers()
            .get("X-Agos-Cache")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
            .to_string();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, cache, body)
    };

    // 1. First request -> MISS, served by the cheap entry.
    let req_body = serde_json::json!({"model": "prog/eco",
        "messages": [{"role": "user", "content": "what is 2+2?"}], "stream": false});
    let resp = app
        .clone()
        .oneshot(build(req_body.clone(), false))
        .await
        .expect("oneshot");
    let (status, cache, _b) = meta(resp).await;
    assert_eq!(status, 200);
    assert_eq!(cache, "MISS");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(seen.lock().unwrap().last().unwrap(), "gpt-4o-mini");

    // 2. Exact repeat -> HIT, no new upstream call.
    let resp = app
        .clone()
        .oneshot(build(req_body.clone(), false))
        .await
        .expect("oneshot");
    let (status, cache, body) = meta(resp).await;
    assert_eq!(status, 200);
    assert_eq!(cache, "HIT");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "cache hit must not hit upstream"
    );
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["choices"][0]["message"]["content"], "eco answer");

    // 3. Header escalation -> bypasses cache, hits the flagship.
    let resp = app
        .clone()
        .oneshot(build(req_body.clone(), true))
        .await
        .expect("oneshot");
    let (status, _c, _b) = meta(resp).await;
    assert_eq!(status, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(seen.lock().unwrap().last().unwrap(), "gpt-4o");

    // 4. Body-flag escalation -> also hits the flagship.
    let esc = serde_json::json!({"model": "prog/eco",
        "messages": [{"role": "user", "content": "what is 2+2?"}],
        "stream": false, "economy_escalate": true});
    let resp = app.oneshot(build(esc, false)).await.expect("oneshot");
    let (status, _c, _b) = meta(resp).await;
    assert_eq!(status, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert_eq!(seen.lock().unwrap().last().unwrap(), "gpt-4o");
}

// --- OpenAI Responses upstream ------------------------------------------------

/// A mock Responses-only upstream that returns a Responses-shaped reply.
async fn mock_responses_upstream(port: u16, fail: bool) -> tokio::task::JoinHandle<()> {
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(move || async move {
            if fail {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({ "error": { "message": "boom" } })),
                );
            }
            (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "id": "resp_mock",
                    "created_at": 1_700_000_000i64,
                    "model": "mock-model",
                    "output": [
                        { "type": "reasoning", "summary": [] },
                        {
                            "type": "message",
                            "content": [{ "type": "output_text", "text": "hello from responses" }]
                        }
                    ],
                    "usage": { "input_tokens": 5, "output_tokens": 3 }
                })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("bind mock responses upstream");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })
}

fn responses_request(stream: bool) -> axum::http::Request<axum::body::Body> {
    let req_body = serde_json::json!({
        "model": "programmer/php-dev",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": stream
    });
    axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", "Bearer profile")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(req_body.to_string()))
        .expect("build request")
}

/// A Responses-only upstream serves a non-streaming chat completion, reshaped
/// back into chat-completion form with the upstream usage counts.
#[tokio::test]
async fn responses_upstream_serves_non_streaming_chat() {
    let mock_port = 19891;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_responses_upstream(mock_port, false).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store_kind(&mock_base, ProviderKind::OpenAIResponses);
    let mut state = test_state(store);
    state.attempt_timeout = Duration::from_secs(5);
    let app = create_app(state);

    let mut request = responses_request(false);
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {profile_id}").parse().unwrap(),
    );

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(json["object"], "chat.completion");
    assert_eq!(
        json["choices"][0]["message"]["content"],
        "hello from responses"
    );
    assert_eq!(json["choices"][0]["finish_reason"], "stop");
    assert_eq!(json["usage"]["input_tokens"], 5);
    assert_eq!(json["usage"]["output_tokens"], 3);
}

/// A streaming request against a Responses-only upstream is served from the
/// non-streamed answer, wrapped into a minimal OpenAI SSE stream.
#[tokio::test]
async fn responses_upstream_streams_sse_wrapped_completion() {
    let mock_port = 19892;
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_responses_upstream(mock_port, false).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store_kind(&mock_base, ProviderKind::OpenAIResponses);
    let mut state = test_state(store);
    state.attempt_timeout = Duration::from_secs(5);
    let app = create_app(state);

    let mut request = responses_request(true);
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {profile_id}").parse().unwrap(),
    );

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("chat.completion.chunk"), "body: {text}");
    assert!(text.contains("hello from responses"), "body: {text}");
    assert!(text.contains("finish_reason\":\"stop\""), "body: {text}");
    assert!(text.contains("data: [DONE]"), "body: {text}");
}

/// When the Responses-only upstream fails, the router fails over to the next
/// entry in the chain (an OpenAI-compatible fallback here).
#[tokio::test]
async fn responses_upstream_failure_fails_over_to_next_entry() {
    let responses_port = 19893;
    let fallback_port = 19894;
    let responses_base = format!("http://127.0.0.1:{responses_port}");
    let fallback_base = format!("http://127.0.0.1:{fallback_port}");
    let _responses = mock_responses_upstream(responses_port, true).await;
    let _fallback = mock_upstream(fallback_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store_kind(&responses_base, ProviderKind::OpenAIResponses);
    // Second (fallback) provider + entry at lower priority.
    store
        .create_provider(
            &profile_id,
            agos::storage::NewProvider {
                name: "fallback".into(),
                description: None,
                base_url: fallback_base,
                auth_token: "sk-fallback".into(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )
        .expect("create fallback provider");
    let providers = store.list_providers(&profile_id).expect("list providers");
    let fallback = providers.iter().find(|p| p.name == "fallback").unwrap();
    let proxy = &store.list_proxies(&profile_id).expect("proxies")[0];
    let routes = store.list_routes(proxy.id).expect("routes");
    store
        .add_route_entry(
            routes[0].id,
            fallback.id,
            "mock-model",
            2,
            1.0,
            RouteCapabilities::default(),
        )
        .expect("add fallback entry");

    let mut state = test_state(store);
    state.attempt_timeout = Duration::from_secs(5);
    let app = create_app(state);

    let mut request = responses_request(false);
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {profile_id}").parse().unwrap(),
    );

    let response = app.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    assert_eq!(
        json["choices"][0]["message"]["content"], "hello from mock",
        "the fallback entry must have served the request"
    );
}
