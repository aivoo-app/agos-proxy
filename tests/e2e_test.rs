//! End-to-end tests: spin up the full axum app against a mock upstream and
//! exercise the request lifecycle.

use std::sync::Arc;
use std::time::Duration;

use agos::domain::{ProviderKind, RoutingStrategy};
use agos::server::{create_app, AppState};
use agos::storage::Store;
use tower::util::ServiceExt;

/// Start a mock upstream that echoes a fixed chat-completions response.
async fn mock_upstream(port: u16) -> tokio::task::JoinHandle<()> {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            let body = serde_json::json!({
                "id": "mock-1",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "hello from mock" },
                    "finish_reason": "stop"
                }]
            });
            (axum::http::StatusCode::OK, axum::Json(body))
        }),
    );
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("bind mock upstream");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })
}

fn setup_store(base_url: &str) -> (Store, String) {
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
                kind: ProviderKind::OpenAICompatible,
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
            Default::default(),
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
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
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
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
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
        http_client,
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
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
