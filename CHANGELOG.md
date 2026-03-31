# Changelog

All notable changes to AGOS Proxy are documented here.

This project adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and uses the Rust ecosystem convention of a `semver`-compatible version in
`Cargo.toml` mirrored by `src/lib.rs`.

## [Unreleased]

### Added

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

## [0.1.0] - 2025-01-01

### Added

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
