//! The HTTP surface exposed to callers.
//!
//! AGOS Proxy speaks the OpenAI-compatible API — `/v1/chat/completions`,
//! `/v1/completions`, `/v1/embeddings`, `/v1/models` — so an existing OpenAI
//! SDK client can be pointed at this server unchanged. Requests are
//! authenticated with the caller profile's bearer token and subject to the
//! profile's per-minute rate limit when one is set.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::State;
use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use crate::cli::data_dir;
use crate::health;
use crate::router::RoutingState;
use crate::storage::Store;

mod auth;
mod handlers;
pub mod middleware;
pub mod ratelimit;

pub use handlers::{chat_completions, list_models, AppState};

/// Maximum request body size (10 MB). Protects against memory exhaustion from
/// oversized payloads.
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Total request timeout (120 seconds). Protects against slow clients consuming
/// resources indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Server start time for uptime tracking.
static SERVER_START_TIME: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);

/// Build the axum router with all routes and shared state.
pub fn create_app(
    store: Arc<Store>,
    attempt_timeout: Duration,
    http_client: reqwest::Client,
) -> Router {
    let state = AppState {
        store,
        attempt_timeout,
        http_client,
        routing_state: RoutingState::default(),
        rate_limiter: Arc::new(ratelimit::RateLimiter::new()),
    };
    Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(handlers::chat_completions),
        )
        .route(
            "/v1/completions",
            axum::routing::post(handlers::completions),
        )
        .route("/v1/embeddings", axum::routing::post(handlers::embeddings))
        .route("/v1/models", axum::routing::get(handlers::list_models))
        // Health endpoints are intentionally outside the auth layer so
        // orchestrators can probe them without a bearer token.
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
        // Security hardening applied to every response.
        .layer(axum::middleware::from_fn(middleware::security_headers))
        // Request id propagation + structured access logging.
        .layer(axum::middleware::from_fn(
            middleware::request_id_and_logging,
        ))
        // CORS — allow web clients to call the API directly. Permissive by
        // default; tighten with a custom layer in production if needed.
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        // Response compression — gzip responses when the client supports it.
        .layer(tower_http::compression::CompressionLayer::new())
        // Request timeout — drop requests that take too long.
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        // Body size limit applied as the outermost layer so it short-circuits
        // before any deserialization work.
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY_SIZE))
        .with_state(state)
}

/// Liveness probe — always returns 200 once the server is up.
async fn health_check() -> axum::response::Response {
    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(r#"{"status":"ok"}"#))
        .unwrap()
}

/// Readiness probe — returns 200 only when the store is reachable.
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

/// Initialize tracing and start the server on `bind_addr`.
pub async fn serve(bind_addr: &str) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let home = data_dir()?;
    std::fs::create_dir_all(&home)?;
    let store = Arc::new(Store::open(Store::default_path(&home))?);
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()?;
    let app = create_app(store.clone(), Duration::from_secs(10), http_client.clone());

    // Start the background health-probe runner so dead entries can recover.
    health::spawn(store, http_client);

    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!("AGOS Proxy listening on {bind_addr}");

    // Graceful shutdown on Ctrl+C or SIGTERM so in-flight requests finish.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Basic metrics endpoint — returns request counts and uptime.
async fn metrics_handler(State(state): State<AppState>) -> axum::response::Response {
    let profiles = state.store.list_profiles().unwrap_or_default();
    let total_profiles = profiles.len();
    let mut total_providers = 0;
    let mut total_routes = 0;
    let mut healthy_entries = 0;
    let mut unhealthy_entries = 0;

    for profile in &profiles {
        let providers = state.store.list_proxies(&profile.id).unwrap_or_default();
        total_providers += providers.len();
        let proxies = state.store.list_proxies(&profile.id).unwrap_or_default();
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

    let uptime_seconds = SERVER_START_TIME.elapsed().as_secs();

    let metrics = serde_json::json!({
        "profiles": total_profiles,
        "proxies": total_providers,
        "routes": total_routes,
        "healthy_entries": healthy_entries,
        "unhealthy_entries": unhealthy_entries,
        "uptime_seconds": uptime_seconds,
    });

    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(metrics.to_string()))
        .unwrap()
}

/// Provider health status endpoint — returns health status of all route entries.
async fn provider_health_handler(State(state): State<AppState>) -> axum::response::Response {
    let mut entries = Vec::new();
    let profiles = state.store.list_profiles().unwrap_or_default();

    for profile in &profiles {
        let proxies = state.store.list_proxies(&profile.id).unwrap_or_default();
        for proxy in &proxies {
            let routes = state.store.list_routes(proxy.id).unwrap_or_default();
            for route in &routes {
                let route_entries = state.store.route_entries(route.id).unwrap_or_default();
                for entry in &route_entries {
                    let provider = state.store.get_provider(entry.provider_id).ok().flatten();
                    let provider_name = provider.map(|p| p.name.clone()).unwrap_or_default();
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

    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({ "entries": entries }).to_string(),
        ))
        .unwrap()
}

/// Future that resolves when a shutdown signal is received.
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
