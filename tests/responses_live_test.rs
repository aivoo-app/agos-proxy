//! Live integration tests against a real OpenAI Responses upstream.
//!
//! Gated behind the `AGOS_LIVE_RESPONSES_KEY` environment variable: when unset
//! the tests exit early so normal CI never makes network calls. Set the
//! variables to a valid account to exercise them:
//!
//! ```console
//! $ AGOS_LIVE_RESPONSES_KEY=sk-... \
//!   AGOS_LIVE_RESPONSES_BASE=https://your-provider.example \
//!   AGOS_LIVE_RESPONSES_MODEL=your-responses-only-model \
//!   cargo test --test responses_live_test -- --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use agos::domain::{ProviderKind, RouteCapabilities, RoutingStrategy};
use agos::server::{create_app, AppState};
use agos::storage::{NewProvider, Store};
use tower::util::ServiceExt;

fn live_key() -> Option<String> {
    std::env::var("AGOS_LIVE_RESPONSES_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

fn live_base() -> String {
    std::env::var("AGOS_LIVE_RESPONSES_BASE")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "https://api.example.com".to_string())
}

fn live_model() -> Option<String> {
    std::env::var("AGOS_LIVE_RESPONSES_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
}

#[tokio::test]
async fn live_responses_list_models_and_chat() {
    let (Some(key), Some(model)) = (live_key(), live_model()) else {
        eprintln!(
            "skipping: set AGOS_LIVE_RESPONSES_KEY and AGOS_LIVE_RESPONSES_MODEL to run this test"
        );
        return;
    };
    let base = live_base();
    let trimmed_base = base.trim_end_matches('/');

    // 1. Sanity: the key can list models.
    let client = reqwest::Client::new();
    let models = client
        .get(format!("{trimmed_base}/v1/models"))
        .bearer_auth(&key)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .expect("list models");
    assert!(models.status().is_success(), "list models: {models:?}");

    // 2. One direct Responses call.
    let resp = client
        .post(format!("{trimmed_base}/v1/responses"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "model": model,
            "input": "user: Reply with exactly: pong",
        }))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .expect("responses call");
    assert!(resp.status().is_success(), "responses call: {resp:?}");

    // 3. One end-to-end chat call through a test proxy + route.
    let store = Store::open_in_memory().expect("open store");
    let profile = store.create_profile("live", None, None).expect("profile");
    store
        .create_provider(
            &profile.id,
            NewProvider {
                name: "responses-upstream".into(),
                description: None,
                base_url: base.clone(),
                auth_token: key,
                kind: ProviderKind::OpenAIResponses,
                extra_headers: Default::default(),
                masking_server_id: None,
            },
        )
        .expect("provider");
    let provider = &store.list_providers(&profile.id).expect("providers")[0];
    let proxy = store
        .create_proxy(&profile.id, "live", None)
        .expect("proxy");
    let route = store
        .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
        .expect("route");
    store
        .add_route_entry(
            route.id,
            provider.id,
            &model,
            1,
            1.0,
            RouteCapabilities::default(),
        )
        .expect("entry");

    let state = AppState {
        store: Arc::new(store),
        attempt_timeout: Duration::from_secs(60),
        stream_idle_timeout: Duration::from_secs(30),
        http_client: reqwest::Client::new(),
        routing_state: agos::router::RoutingState::default(),
        rate_limiter: Arc::new(agos::server::ratelimit::RateLimiter::new()),
        require_auth_on_health: false,
        adapter: agos::adapter::Registry::default(),
    };
    let app = create_app(state);

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", profile.id))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({
                "model": "live/r1",
                "messages": [{ "role": "user", "content": "Reply with exactly: pong" }]
            })
            .to_string(),
        ))
        .expect("build request");

    let response = app.oneshot(request).await.expect("oneshot");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert!(status.is_success(), "chat call failed: {status} {body:?}");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.to_lowercase().contains("pong"),
        "unexpected reply: {json}"
    );
}
