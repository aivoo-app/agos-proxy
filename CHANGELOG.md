# Changelog

All notable changes to AGOS Proxy are documented here.

This project adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and uses the Rust ecosystem convention of a `semver`-compatible version in
`Cargo.toml` mirrored by `src/lib.rs`.

## [0.1.7] - 2026-09-18

### Added

- **Cross-profile resource sharing:** providers, egress masks, and routes gain
  a `shared` flag so an owner profile can publish a resource to the whole
  instance. Any profile may *use* a shared resource at runtime, but only the
  owner may edit it, and secrets never leave the encrypted store. CLI:
  `provider add/edit --share`, `mask add/set --share`, `route share`,
  share-aware pickers that list foreign shared resources, and bootstrap JSON
  support.
- **Routes as models:** a route entry may reference another route
  (`--from-route <proxy>/<route>` or `<profile>/<proxy>/<route>` for shared
  foreign routes) instead of a provider model. Resolution expands nested
  chains recursively with cycle detection, a depth cap, and the share check
  applied at every hop. Capability filtering uses the union of the target
  route's leaves; health probing skips nested entries (their leaves are
  probed directly); usage is attributed to the leaf entry; nested entries
  render as `→ <profile>/<proxy>/<route>` in the CLI.
- **Provider-native prompt caching:** new route-level `prompt_cache = off |
  auto` setting. Anthropic requests get ephemeral `cache_control` breakpoints
  on the system block array and the last user content block (client-supplied
  `cache_control` is honoured, never double-injected); OpenAI requests get a
  stable proxy-derived `prompt_cache_key` for cache affinity; Google forwards
  `cachedContent` when the caller supplies it.
- **Cached-token accounting:** `usage_log` now records provider-reported
  cached prompt tokens (Anthropic `cache_read_input_tokens`, OpenAI
  `cached_tokens`), surfaced through `usage stats` so the economy story can
  show prompt-cache savings.

### Changed

- Capability flags are now **inferred from the model id at entry creation**,
  so common tool/vision/JSON-capable families advertise support out of the
  box instead of defaulting to all-off.
- The exact-match economy cache now skips image payloads (never hashed into
  SQLite blobs) and keys on route + prompt-cache mode in addition to the
  request hash.

### Fixed

- Empty target lists now distinguish their cause: when entries exist but none
  declares a capability the request needs, the proxy returns
  `400 capability_unavailable` naming the missing capability instead of the
  misleading `503 "no healthy providers available for this route"`. Plain
  503s are kept for genuinely-down providers so clients still back off. This
  fixes image requests "failing" on healthy routes whose entries under-claimed
  vision, including one level up when the route is used as a model.
- An upstream image refusal now marks the entry `vision = false` (a capability
  miss) instead of `Unhealthy`, so one over-claimed flag never takes a working
  key dark — the next image request skips the entry without rediscovery.
- No outbound adapter silently drops image parts: unsupported adapters reject
  with the capability-skip marker so failover moves on to a capable entry.
- The old `route_entries` table shape (non-nullable `provider_id`, no
  `target_route_id`) is rebuilt once on open with foreign keys held off so the
  `usage_log ON DELETE CASCADE` hazard cannot wipe history; the migration is
  covered by a regression test asserting usage rows survive.

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
- Failover for image-rejecting upstreams: a 4xx whose error message explicitly
  refuses image/vision capability marks the route entry Unhealthy so the next
  chain entry is tried. Other 4xx keep the existing no-demotion behavior.
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

- Image-error classification no longer demotes healthy entries merely because
  an unrelated 4xx mentions images or echoes image fields. Both request paths
  now share a conservative capability-refusal classifier that inspects JSON
  error messages rather than entire response bodies. Error text is extracted
  from dedicated error fields only (`error.message`, a string `error`,
  `message`, or `detail`), handling string, content-block array and object
  shapes, so Anthropic and Gemini envelopes classify correctly. Rejections
  about the payload rather than the model — unsupported media type/MIME,
  unsupported image format, corrupt or undecodable data, oversized or
  wrong-resolution images, download failures — no longer demote either.
  Regression tests cover request echoes, corrupt images, model names, Unicode,
  real upstream envelopes, and streaming health persistence; rate-limit and
  server-error classifications are unchanged.
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
