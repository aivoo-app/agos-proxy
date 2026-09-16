//! Live integration tests against the real Zen Responses endpoint.
//!
//! Gated behind the `AGOS_LIVE_ZEN_KEY` environment variable: when unset the
//! tests exit early so normal CI never makes network calls. Set the variable
//! to a valid Zen API key to exercise them:
//!
//! ```console
//! $ AGOS_LIVE_ZEN_KEY=sk-... cargo test --test responses_live_test -- --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use agos::domain::{ProviderKind, RouteCapabilities, RoutingStrategy};
use agos::server::{create_app, AppState};
use agos::storage::{NewProvider, Store};
use tower::util::ServiceExt;

fn zen_key() -> Option<String> {
    std::env::var("AGOS_LIVE_ZEN_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

fn zen_base() -> String {
    std::env::var("AGOS_LIVE_ZEN_BASE").unwrap_or_else(|_| "https://api.zen.ai".to_string())
}

#[tokio::test]
async fn live_zen_list_models_and_chat() {
    let Some(key) = zen_key() else {
        eprintln!("skipping: AGOS_LIVE_ZEN_KEY not set");
        return;
    };
    let base = zen_base();

    // 1. Sanity: the key can list models.
    let client = reqwest::Client::new();
    let models = client
        .get(format!("{}/v1/models", base.trim_end_matches('/')))
        .bearer_auth(&key)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .expect("list models");
    assert!(models.status().is_success(), "list models: {models:?}");

    // 2. One direct Responses call.
    let resp = client
        .post(format!("{}/v1/responses", base.trim_end_matches('/')))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "model": "muse-spark-1-contributor-free",
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
                name: "zen".into(),
                description: None,
                base_url: base.clone(),
                auth_token: key,
                kind: ProviderKind::OpenAIResponses,
                extra_headers: Default::default(),
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
            "muse-spark-1-contributor-free",
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
