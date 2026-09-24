//! HTTP middleware: request IDs, security headers, and request logging.
//!
//! These layers are applied to every request passing through the proxy so that
//! callers get consistent observability and hardening without each handler
//! having to do it by hand.

use std::time::Instant;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use tracing::info;
use uuid::Uuid;

/// Header used to propagate a caller-supplied request ID, or to echo back the
/// server-generated one.
pub const REQUEST_ID_HEADER: &str = "X-Request-ID";

/// Request metadata carried through extensions without colliding with the
/// authenticated profile token, which auth middleware also stores as `String`.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Generate or propagate a request ID, attach it to the request extensions for
/// handlers to read, echo it back in the response, and log the request on
/// completion with method, path, status and latency.
pub async fn request_id_and_logging(request: Request, next: Next) -> Response {
    let start = Instant::now();

    // Trust a caller-supplied ID (great for correlating across services) or
    // generate a fresh v4 UUID.
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let method = request.method().clone();
    let uri = request.uri().clone();

    // Stash the id so handlers can reference it in their own log lines.
    let mut request = request;
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let mut response = next.run(request).await;

    // Echo the id back so the caller can tie our logs to theirs.
    if let Ok(value) = request_id.parse() {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }

    let duration = start.elapsed();
    let status = response.status();

    info!(
        method = %method,
        uri = %uri,
        status = %status.as_u16(),
        duration_ms = duration.as_millis() as i64,
        request_id = %request_id,
        "request completed"
    );

    response
}

/// Add baseline security headers to every response.
///
/// These are the same small set every HTTP service should send: MIME-type
/// sniffing protection, frame denial, XSS filter and a referrer policy. The
/// `Server` header is set to the product name rather than the default.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    // Prevent MIME-type sniffing.
    if let Ok(v) = "nosniff".parse() {
        headers.insert("X-Content-Type-Options", v);
    }
    // Never allow the response to be embedded in a frame.
    if let Ok(v) = "DENY".parse() {
        headers.insert("X-Frame-Options", v);
    }
    // Enable the browser's built-in XSS filter.
    if let Ok(v) = "1; mode=block".parse() {
        headers.insert("X-XSS-Protection", v);
    }
    // Leak the least amount of referrer information.
    if let Ok(v) = "strict-origin-when-cross-origin".parse() {
        headers.insert("Referrer-Policy", v);
    }
    // Identify the server by product name.
    if let Ok(v) = "agos-proxy".parse() {
        headers.insert("Server", v);
    }

    response
}
