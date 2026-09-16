# Changelog

All notable changes to AGOS Proxy are documented here.

This project adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and uses the Rust ecosystem convention of a `semver`-compatible version in
`Cargo.toml` mirrored by `src/lib.rs`.

## [0.1.3] - 2026-09-16

### Fixed

- Streaming requests (`stream: true`) now fail over correctly across
  upstreams. Previously the proxy committed to a `200 OK` + SSE response
  before contacting any upstream; when the first upstreams failed (e.g.
  413/429), clients received an empty chunked stream that terminated
  abruptly (`incomplete chunked read`). Streaming handlers now probe each
  upstream with the real request and only return the event-stream response
  once an upstream answers `2xx`, streaming the already-obtained response
  body without re-sending the request.
- When every upstream fails for a streaming request, the proxy now returns
  a proper `502 Bad Gateway` JSON error before any response is sent,
  instead of closing an empty stream.
- Streaming requests no longer hit the chosen upstream twice (probe +
  re-send); the confirmed response is streamed directly.

Affected surfaces: OpenAI chat completions, legacy `/v1/completions`,
Anthropic, OpenAI Responses (Codex), and Gemini native endpoints.

## [Unreleased]

### Added

- **Vision passthrough:** multimodal chat requests (OpenAI parts arrays with
  `text` + `image_url` entries) now keep their images end to end. The
  Anthropic outbound adapter maps image parts to `image` blocks (`url` and
  `base64` sources), the Google adapter maps them to Gemini `fileData` /
  `inlineData` parts, and the Anthropic and Gemini inbound surfaces accept
  their native image shapes and normalize them into the canonical parts array.
  OpenAI/custom upstreams were already lossless and are unchanged; text-only
  requests serialize byte-identically to before.
- Failover for image-rejecting upstreams: a 4xx whose error body names
  images/vision marks the route entry Unhealthy so the next chain entry is
  tried, instead of surfacing the rejection to the caller. Other 4xx keep the
  existing no-demotion behavior.
- Comprehensive documentation tree: README, architecture, configuration,
  security, API, providers, failover, development, deployment, workflow,
  contributing guide, and security policy.
- Top-level `Makefile` with common targets: `check`, `fmt`, `lint`, `test`,
  `build`, `docs`, `docker-up`, `docker-down`, `ci`.
- GitHub Actions CI workflow with formatting, linting, tests, release build,
  docs generation smoke test, and a docker-compose integration smoke test.
- `SECURITY.md` with the vulnerability reporting process.
- `CONTRIBUTING.md` with contribution workflow and review expectations.
- `CHANGELOG.md` established as the project change log.

### Changed

- README reorganized into a structured document with quick start, architecture
  overview, command summary, request lifecycle, health model, routing strategies,
  security model, tech stack, and documentation index.

### Fixed

- Provider-kind examples in the documentation, README, quick reference and the
  docker-compose seed used `generic`, a tag the CLI has never accepted; the
  binary's canonical tag is `openai`. Every example now matches the code, and
  the stale `openai_compatible` references in `docs/configuration.md` and
  `docs/providers.md` were corrected too. Before this fix, seeding the compose
  stack failed with `unknown provider kind "generic"`.
- `bootstrap` setup documents accept `"kind": "custom"` again (the alias
  `provider add --kind custom` always accepted); it regressed out of the
  bootstrap parser during the provider-kind rename.
- New regression test asserts the provider kinds embedded in
  `docker-compose.yml` are tags the CLI actually parses, so docs/config and
  code cannot silently drift apart again.

## [0.1.1] - 2026-09-14

### Fixed

- Panic on unknown route entry status tags (e.g. legacy `"draining"` from
  previous database schemas). `status_from_tag` now falls back to `Unhealthy`
  with a warning instead of panicking.
- Schema migration in `migrate_columns` now normalizes stale status tags to
  `"unhealthy"` on store open, preventing future panics on upgraded databases.
- CI race condition: added a "Wait for mock upstream to be ready" step in the
  docker-compose integration test so the Python mock server is fully listening
  before smoke tests are sent.
- CI robustness: chat completions smoke test now retries up to 3 times with
  2s delay to handle transient startup hiccups.
- Makefile `.SHELLFLAGS` now includes `-c` and uses `/usr/bin/env bash` to
  avoid `set -euo pipefail` errors under `/bin/sh`.
- Clippy lint: `.items(&choices)` → `.items(choices)` (needless borrows).
- `cargo fmt` indentation fix in `src/cli/util.rs`.

## [0.1.0] - 2025-01-01

### Added

- Codex CLI compatibility: native OpenAI **Responses API** surface at
  `POST /codex/v1/responses`, with streaming (`response.created` →
  `response.output_text.delta` → `response.output_item.done` →
  `response.completed`) and function/tool calling round trips. Configure Codex
  with `wire_api = "responses"` and `base_url = http://127.0.0.1:3000/codex/v1`;
  see [the Codex CLI tutorial](docs/tutorials/codex-cli.md).
- Initial release.
- Profile, provider, proxy, and route CRUD via CLI.
- OpenAI-compatible HTTP server: chat completions, completions, embeddings,
  models listing, health endpoint.
- Automatic failover with health tracking and background recovery.
- Priority, round-robin, and weighted routing strategies.
- Streaming (SSE) and non-streaming passthrough.
- Provider translators for OpenAI-compatible, Anthropic, and Google.
- Encrypted provider secrets at rest (ChaCha20-Poly1305) and Argon2id password
  hashing.
- Per-profile bearer token auth and rate limiting.
- Interactive CLI wizards and non-interactive flag-driven commands.
- Full-screen TUI chat and interactive REPL chat.
- Portable config export/import with passphrase sealing.
- Usage logging, per-model stats, and recent request history.
- Bootstrap from JSON document for scripted and container use.
- Docker Compose test stack with a mock OpenAI-compatible upstream.
- Auto-generated CLI reference, man pages, and shell completions.
