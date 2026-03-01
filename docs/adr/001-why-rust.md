# ADR 001: Why Rust was Chosen

> Status: Accepted

## Context

AGOS Proxy needs to be:
- A single binary with no runtime dependencies
- Fast enough to handle thousands of concurrent connections
- Memory-safe (it handles API keys and secrets)
- Deployable anywhere (Linux, macOS, Windows, containers)

## Decision

We chose Rust as the implementation language.

## Consequences

### Positive

- **Single static binary:** No Python interpreter, no Node runtime, no JVM. Just one file to deploy.
- **Memory safety:** No use-after-free, no data races. Important for a tool that handles API keys.
- **Performance:** Comparable to C/C++ for I/O-bound workloads (async with tokio).
- **Ecosystem:** Excellent crates for HTTP (axum, reqwest), SQLite (rusqlite), crypto (chacha20poly1305, argon2), and CLI (clap).
- **Cross-compilation:** Easy cross-compilation to Linux (x86_64, aarch64), macOS, and Windows.

### Negative

- **Steeper learning curve** than Go, Python, or Node.js.
- **Longer compile times** than Go or interpreted languages.
- **Smaller talent pool** for contributors compared to mainstream languages.
- **SQLite is synchronous** in the rusqlite crate, requiring `spawn_blocking` for async code.

### Alternatives Considered

| Language | Pros | Cons |
|----------|------|------|
| **Go** | Fast compile, great concurrency, single binary | GC pauses, less memory safety guarantees, larger binary |
| **Python** | Easy to write, large ecosystem | Requires runtime, slow, GIL limits concurrency |
| **Node.js** | Easy to write, large ecosystem | Requires runtime, single-threaded event loop, less memory safety |
| **C++** | Maximum performance | Memory safety risks, long compile times, complex tooling |

Rust was chosen because the security requirements (handling encrypted API keys) and deployment requirements (single binary) outweigh the learning curve cost.
