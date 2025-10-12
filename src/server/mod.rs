//! The HTTP surface exposed to callers.
//!
//! AGOS Proxy speaks the OpenAI-compatible API — `/v1/chat/completions`,
//! `/v1/models` — so an existing OpenAI SDK client can be pointed at this
//! server unchanged.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use crate::cli::data_dir;
use crate::health;
use crate::storage::Store;

mod auth;
mod handlers;

pub use handlers::{chat_completions, list_models, AppState};

/// Build the axum router with all routes and shared state.
pub fn create_app(store: Arc<Store>, attempt_timeout: Duration, http_client: reqwest::Client) -> Router {
    let state = AppState {
        store,
        attempt_timeout,
        http_client,
    };
    Router::new()
        .route("/v1/chat/completions", axum::routing::post(handlers::chat_completions))
        .route("/v1/models", axum::routing::get(handlers::list_models))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::auth_middleware))
        .with_state(state)
}

/// Initialize tracing and start the server on `bind_addr`.
pub async fn serve(bind_addr: &str) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let home = data_dir()?;
    std::fs::create_dir_all(&home)?;
    let store = Arc::new(Store::open(Store::default_path(&home))?);
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let app = create_app(store.clone(), Duration::from_secs(10), http_client.clone());

    // Start the background health-probe runner so dead entries can recover.
    health::spawn(store, http_client);

    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!("AGOS Proxy listening on {bind_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
