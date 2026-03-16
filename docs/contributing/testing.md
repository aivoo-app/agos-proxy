# Testing Guide

> How to write and run tests for AGOS Proxy.

## Table of Contents

- [Test Organization](#test-organization)
- [Unit Tests](#unit-tests)
- [Integration Tests](#integration-tests)
- [End-to-End Tests](#end-to-end-tests)
- [Mock Upstream](#mock-upstream)
- [Writing New Tests](#writing-new-tests)
- [CI Pipeline](#ci-pipeline)
- [Test Coverage](#test-coverage)

---

## Test Organization

AGOS Proxy uses three levels of testing:

| Level | Location | Speed | What it tests |
|-------|----------|-------|---------------|
| **Unit tests** | Inline in each module (`#[cfg(test)]`) | Fast | Domain logic, crypto, routing, rate limiting |
| **Integration tests** | `tests/` directory | Medium | Full request lifecycle against mock upstream |
| **E2E tests** | `tests/e2e_test.rs` | Medium | Server handlers against a real mock upstream |

---

## Unit Tests

Unit tests live alongside the code they cover. Each module has a `#[cfg(test)]` mod at the bottom.

### Running Unit Tests

```sh
# All unit tests
cargo test --lib

# Specific module
cargo test crypto::
cargo test router::
cargo test storage::

# With output
cargo test -- --nocapture
```

### What's Covered

| Module | Tests |
|--------|-------|
| `crypto` | Encrypt/decrypt roundtrip, wrong key rejection, Argon2id derivation |
| `router` | Model resolution, strategy application, failover filtering, profile scoping |
| `storage` | CRUD round-trips, RPM limit persistence, encrypted token round-trip |
| `cli::util` | Password hashing, verification, legacy hash support |
| `cli::config` | Export/import roundtrip, sealed bundle passphrase |
| `cli::bootstrap` | Kind/strategy parsing, capabilities defaults |
| `health` | Status state machine transitions |
| `server::ratelimit` | Admit/refuse behavior, key independence |
| `translator::google` | URL building, request/response translation |

---

## Integration Tests

Integration tests exercise a component against a real dependency (e.g., an in-memory store or a mock upstream).

### Running Integration Tests

```sh
# All integration tests
cargo test --test e2e_test

# Specific test
cargo test chat_completions_routes_through_mock_upstream
```

### What's Covered

| Test | What it does |
|------|-------------|
| `chat_completions_routes_through_mock_upstream` | Spins up a mock upstream, sends a request through the full `axum` app, verifies the response |
| `chat_completions_rejects_missing_auth` | Verifies that missing bearer token returns 401 |
| `list_models_returns_caller_routes` | Verifies that `GET /v1/models` returns the caller's routes |

---

## End-to-End Tests

E2E tests live in `tests/e2e_test.rs` and exercise the server against a real HTTP caller (the `oneshot` method from `tower::util::ServiceExt`).

### Running E2E Tests

```sh
make test-e2e
# or
cargo test --test e2e_test -- --test-threads=auto
```

### The Mock Upstream

The tests spin up a tiny axum server that responds to `POST /v1/chat/completions` with a fixed response:

```rust
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
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await.unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })
}
```

This mock is intentionally simple — it doesn't implement full OpenAI semantics. It exists so tests don't need real API keys.

---

## Writing New Tests

### Unit Test Template

Add a `#[cfg(test)]` block to the module you're testing:

```rust
// In src/domain/model.rs (or any module)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn my_new_feature_works() {
        // Arrange
        let input = ...;

        // Act
        let result = my_function(input);

        // Assert
        assert_eq!(result, expected);
    }
}
```

### Integration Test Template

Add a test to `tests/e2e_test.rs`:

```rust
#[tokio::test]
async fn my_new_endpoint_works() {
    let mock_port = 19900;  // Use a unique port
    let mock_base = format!("http://127.0.0.1:{mock_port}");
    let _mock = mock_upstream(mock_port).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (store, profile_id) = setup_store(&mock_base);
    let http_client = reqwest::Client::new();
    let app = create_app(Arc::new(store), Duration::from_secs(5), http_client);

    // Build and send a request
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/your-endpoint")
        .header("Authorization", format!("Bearer {profile_id}"))
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(request_body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
}
```

### Test Guidelines

1. **Tests must not depend on real provider access.** Use the mock upstream.
2. **Tests must be deterministic.** Avoid time-dependent assertions where possible.
3. **Tests should be fast.** The full test suite should run in under 30 seconds.
4. **Tests should be isolated.** Each test sets up its own store and mock upstream.
5. **Name tests descriptively.** `chat_completions_rejects_missing_auth` not `test_1`.

---

## CI Pipeline

The CI pipeline (`.github/workflows/ci.yml`) runs:

1. `cargo fmt --all --check` — formatting
2. `cargo clippy --all-targets -- -D warnings` — linting
3. `cargo test --all-targets -- --test-threads=auto` — all tests
4. `cargo build --release` — release build
5. `./target/release/agen-docs gen-docs --all --output-dir /tmp/agos-ci-docs` — docs smoke test
6. Docker compose smoke test — starts the stack, sends smoke requests

### Running CI Locally

```sh
make ci
```

This runs the same checks as CI, so you can verify before pushing.

---

## Test Coverage

### Current Coverage

The test suite covers:
- ✅ Crypto round-trips (encrypt/decrypt, key derivation)
- ✅ Routing resolution and strategy
- ✅ Storage CRUD and encrypted token persistence
- ✅ Rate limiting
- ✅ Health status state machine
- ✅ Request authentication
- ✅ Model listing
- ✅ Chat completions routing (via mock)
- ✅ Config export/import roundtrip

### Areas for Improvement

- ❌ Streaming request/response handling
- ❌ Failover behavior with multiple providers
- ❌ Provider translation (Anthropic, Google)
- ❌ TUI chat (hard to test headless)
- ❌ CLI wizard interactions

### Adding Coverage

When adding a new feature, include tests. When fixing a bug, add a regression test. See [CONTRIBUTING.md](../../CONTRIBUTING.md) for the expectations.

---

## Debugging Tests

### Run a single test with output

```sh
cargo test --test e2e_test chat_completions -- --nocapture
```

### Run tests with logging

```sh
RUST_LOG=debug cargo test --test e2e_test -- --nocapture
```

### Test a specific module

```sh
cargo test router::
```

### Check for compile warnings in tests

```sh
cargo test --all-targets 2>&1 | grep warning
```
