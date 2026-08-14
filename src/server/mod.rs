//! The HTTP surface exposed to callers.
//!
//! AGOS Proxy speaks the OpenAI-compatible API - `/v1/chat/completions`,
//! `/v1/completions`, `/v1/embeddings`, `/v1/models` - so an existing OpenAI
//! SDK client can be pointed at this server unchanged. Requests are
//! authenticated with the caller profile's bearer token and subject to the
//! profile's per-minute rate limit when one is set.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use crate::adapter::Registry;
use crate::cli::data_dir;
use crate::health;
use crate::router::RoutingState;
use crate::storage::Store;

mod auth;
mod handlers;
pub mod middleware;
mod native;
pub mod ratelimit;

pub use handlers::{chat_completions, list_models, AppState};

/// Maximum request body size (10 MB). Protects against memory exhaustion from
/// oversized payloads.
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Total request timeout (120 seconds). Protects against slow clients consuming
/// resources indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Build a CORS layer based on the AGOS_CORS_ORIGINS environment variable.
/// When the env var is set, only the specified origins are allowed.
/// When not set, permissive CORS is used for backward compatibility.
fn build_cors_layer() -> tower_http::cors::CorsLayer {
    let origins: Option<Vec<String>> = std::env::var("AGOS_CORS_ORIGINS")
        .ok()
        .map(|s| s.split(',').map(|o| o.trim().to_string()).collect());

    if let Some(origin_strs) = origins {
        let mut cors = tower_http::cors::CorsLayer::new()
            .allow_methods([
                axum::http::Method::POST,
                axum::http::Method::GET,
                axum::http::Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::header::HeaderName::from_static("x-request-id"),
            ]);
        // Parse each origin; skip any that are malformed rather than panicking
        // on a bad environment value, which would take the whole server down
        // at startup. Log a warning for operators to catch the mistake.
        let header_values: Vec<HeaderValue> = origin_strs
            .iter()
            .filter_map(|o| match o.parse() {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(origin = %o, error = %e, "ignoring invalid origin in AGOS_CORS_ORIGINS");
                    None
                }
            })
            .collect();
        cors = cors.allow_origin(header_values);
        cors
    } else {
        // Default to restrictive CORS when no origins configured.
        // Only allow same-origin requests (no CORS headers sent).
        tower_http::cors::CorsLayer::new()
            .allow_origin([])
            .allow_methods([])
            .allow_headers([])
    }
}

/// Build the axum router with all routes and shared state.
pub fn create_app(state: AppState) -> Router {
    let cors_layer = build_cors_layer();

    Router::new()
        // Legacy OpenAI-compatible surface (backward-compatible alias).
        .route(
            "/v1/chat/completions",
            axum::routing::post(handlers::chat_completions),
        )
        // Namespaced native surfaces served through the master adapter registry.
        .route(
            "/openai/v1/chat/completions",
            axum::routing::post(native::openai_chat),
        )
        .route(
            "/openai/v1/completions",
            axum::routing::post(handlers::completions),
        )
        .route(
            "/openai/v1/embeddings",
            axum::routing::post(handlers::embeddings),
        )
        .route(
            "/openai/v1/models",
            axum::routing::get(handlers::list_models),
        )
        .route(
            "/anthropic/v1/messages",
            axum::routing::post(native::anthropic_messages),
        )
        .route(
            "/anthropic/v1/models",
            axum::routing::get(native::anthropic_models),
        )
        .route(
            "/google/v1beta/models/{*path}",
            axum::routing::any(native::google_generate),
        )
        .route(
            "/google/v1beta/models",
            axum::routing::get(native::google_models),
        )
        .route(
            "/v1/completions",
            axum::routing::post(handlers::completions),
        )
        .route("/v1/embeddings", axum::routing::post(handlers::embeddings))
        .route("/v1/models", axum::routing::get(handlers::list_models))
        .route("/health", axum::routing::get(health_check))
        .route("/ready", axum::routing::get(readiness_check))
        .route("/metrics", axum::routing::get(metrics_handler))
        .route(
            "/v1/providers/health",
            axum::routing::get(provider_health_handler),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .layer(axum::middleware::from_fn(middleware::security_headers))
        .layer(axum::middleware::from_fn(
            middleware::request_id_and_logging,
        ))
        .layer(cors_layer)
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .with_state(state)
}

async fn health_check() -> axum::response::Response {
    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(r#"{"status":"ok"}"#))
        .unwrap()
}

async fn readiness_check(State(state): State<AppState>) -> axum::response::Response {
    match state.store.list_profiles() {
        Ok(_) => axum::response::Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(r#"{"status":"ready"}"#))
            .unwrap(),
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            axum::response::Response::builder()
                .status(503)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"status":"not ready"}"#))
                .unwrap()
        }
    }
}

pub async fn serve(bind_addr: &str, cli_attempt_timeout: Option<Duration>) -> Result<()> {
    if std::env::var("RUST_LOG").unwrap_or_default() != "off" {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .init();
    }

    let home = data_dir()?;
    std::fs::create_dir_all(&home)?;
    let store = Arc::new(Store::open(Store::default_path(&home))?);
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(5)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()?;
    let rate_limiter = Arc::new(ratelimit::RateLimiter::new());
    // Per-attempt failover timeout: how long one model may take before the
    // router gives up on it and moves to the next entry in the chain.
    // Configurable via `--attempt-timeout` or `AGOS_ATTEMPT_TIMEOUT_SECS`.
    // Precedence: --attempt-timeout flag > AGOS_ATTEMPT_TIMEOUT_SECS env > 10s.
    let attempt_timeout = cli_attempt_timeout
        .or_else(|| {
            std::env::var("AGOS_ATTEMPT_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
        })
        .unwrap_or(Duration::from_secs(10));
    let state = AppState {
        store: store.clone(),
        attempt_timeout,
        http_client: http_client.clone(),
        routing_state: RoutingState::default(),
        rate_limiter: rate_limiter.clone(),
        require_auth_on_health: false,
        adapter: Registry::default(),
    };
    let app = create_app(state);

    let health_handle = health::spawn(store, http_client, Some(rate_limiter));

    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!("AGOS Proxy listening on {bind_addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    health_handle.abort();
    Ok(())
}

async fn metrics_handler(State(state): State<AppState>) -> axum::response::Response {
    let profiles = state.store.list_profiles().unwrap_or_default();
    let total_profiles = profiles.len();
    let mut total_providers = 0;
    let mut total_routes = 0;
    let mut healthy_entries = 0;
    let mut unhealthy_entries = 0;

    let mut total_proxies = 0;

    for profile in &profiles {
        // Count actual providers (not proxies)
        let profile_providers = state.store.list_providers(&profile.id).unwrap_or_default();
        total_providers += profile_providers.len();

        let proxies = state.store.list_proxies(&profile.id).unwrap_or_default();
        total_proxies += proxies.len();

        for proxy in &proxies {
            let routes = state.store.list_routes(proxy.id).unwrap_or_default();
            total_routes += routes.len();
            for route in &routes {
                let entries = state.store.route_entries(route.id).unwrap_or_default();
                for entry in &entries {
                    match entry.status {
                        crate::domain::ModelStatus::Healthy
                        | crate::domain::ModelStatus::Degraded => {
                            healthy_entries += 1;
                        }
                        _ => {
                            unhealthy_entries += 1;
                        }
                    }
                }
            }
        }
    }

    let metrics = serde_json::json!({
        "profiles": total_profiles,
        "providers": total_providers,
        "proxies": total_proxies,
        "routes": total_routes,
        "healthy_entries": healthy_entries,
        "unhealthy_entries": unhealthy_entries,
    });

    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(metrics.to_string()))
        .unwrap()
}

async fn provider_health_handler(State(state): State<AppState>) -> axum::response::Response {
    // Any store failure is surfaced as a 500 so the caller knows the reported
    // health is incomplete rather than silently returning an empty list.
    let entries = match collect_provider_health(&state.store) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(error = %e, "failed to collect provider health");
            return axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({ "error": "failed to read provider health" }).to_string(),
                ))
                .unwrap();
        }
    };

    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({ "entries": entries }).to_string(),
        ))
        .unwrap()
}

/// Collect per-entry health information across every profile. Returns a typed
/// error on any store failure instead of silently skipping rows, so the caller
/// can respond with a 5xx rather than an incomplete 200.
fn collect_provider_health(store: &Store) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut entries = Vec::new();
    let profiles = store.list_profiles()?;

    for profile in &profiles {
        let proxies = store.list_proxies(&profile.id)?;
        for proxy in &proxies {
            let routes = store.list_routes(proxy.id)?;
            for route in &routes {
                for entry in store.route_entries(route.id)? {
                    let provider_name = store
                        .get_provider(entry.provider_id)?
                        .map(|provider| provider.name)
                        .unwrap_or_default();
                    entries.push(serde_json::json!({
                        "entry_id": entry.id,
                        "profile": profile.name,
                        "proxy": proxy.name,
                        "route": route.name,
                        "provider": provider_name,
                        "model": entry.model_id,
                        "status": format!("{:?}", entry.status),
                    }));
                }
            }
        }
    }

    Ok(entries)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl+C, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
