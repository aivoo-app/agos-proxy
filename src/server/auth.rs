//! Bearer-token authentication middleware.
//!
//! The caller presents their profile token as a bearer token. On success the
//! profile id is stashed in request extensions so handlers can scope their
//! lookups, and the profile's per-minute rate limit — when one is set — is
//! enforced with a 429 reply before the request reaches any handler.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::server::handlers::AppState;

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Health endpoints may be authenticated depending on configuration.
    // /health is always unauthenticated for basic liveness probing.
    // /ready can be gated if require_auth_on_health is set.
    if path == "/health" {
        return next.run(request).await;
    }

    if path == "/ready" && !state.require_auth_on_health {
        return next.run(request).await;
    }

    // All other endpoints (including /ready when auth is required) need auth.
    let token = request
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(String::from);

    let profile = token
        .as_ref()
        .and_then(|t| state.store.get_profile_by_id(t).ok().flatten());

    match profile {
        Some(profile) => {
            if profile.rpm_limit > 0 && !state.rate_limiter.check(&profile.id, profile.rpm_limit) {
                let body = serde_json::json!({
                    "error": {
                        "message": format!(
                            "rate limit exceeded: {} requests per minute for this profile",
                            profile.rpm_limit
                        ),
                        "type": "rate_limit_error",
                    }
                });
                return axum::response::Response::builder()
                    .status(axum::http::StatusCode::TOO_MANY_REQUESTS)
                    .header("Content-Type", "application/json")
                    .header("Retry-After", "60")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap_or_else(|_| {
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                            .header("Content-Type", "application/json")
                            .body(axum::body::Body::from("internal error"))
                            .unwrap()
                    });
            }
            // Token is guaranteed present here because `get_profile_by_id`
            // returned `Some` only when `token` was `Some`.
            if let Some(token) = token {
                request.extensions_mut().insert(token);
            }
            next.run(request).await
        }
        _ => {
            let body = serde_json::json!({
                "error": {
                    "message": "missing or invalid bearer token",
                    "type": "auth_error",
                }
            });
            axum::response::Response::builder()
                .status(axum::http::StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap_or_else(|_| {
                    axum::response::Response::builder()
                        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "application/json")
                        .body(axum::body::Body::from("internal error"))
                        .unwrap()
                })
        }
    }
}
