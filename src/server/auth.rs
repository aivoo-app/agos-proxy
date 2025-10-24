//! Bearer-token authentication middleware.
//!
//! The caller presents their profile token as a bearer token. On success the
//! profile id is stashed in request extensions so handlers can scope their
//! lookups.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::server::handlers::AppState;

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(String::from);

    match token {
        Some(token)
            if state
                .store
                .get_profile_by_id(&token)
                .ok()
                .flatten()
                .is_some() =>
        {
            request.extensions_mut().insert(token);
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
                .unwrap()
        }
    }
}
