# Development Guide

How to set up a development environment, run the test suite, and contribute.

## Prerequisites

- **Rust** 1.75+ (the project tracks stable; CI runs the latest stable).
- **Make** (optional but recommended) — the top-level `Makefile` wraps the most
  common tasks.
- **Docker** (optional) — only needed to run the `docker-compose` mock upstream
  stack for integration testing.
- **Python 3.12+** (optional) — only needed if you are editing `docker/mock.py`.

## Getting started

```sh
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
make check
```

That runs format, lint, and tests. If you don't have `make`:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Project structure

```
src/
├── bin/
│   └── gen-docs.rs         # generates CLI reference, man pages, completions
├── cli/
│   ├── mod.rs             # command registry, global options, output style
│   ├── app.rs             # clap app builder
│   ├── setup.rs           # guided first-time wizard
│   ├── profile.rs         # profile create/list/show/edit/token/limit/delete
│   ├── provider.rs        # provider add/list/edit/delete
│   ├── proxy.rs           # proxy create/list/edit/delete
│   ├── route.rs           # route create/status/edit/delete
│   ├── route_model.rs     # route model add/remove/move (chain editing)
│   ├── chat.rs            # interactive chat REPL
│   ├── tui.rs             # full-screen chat UI (ratatui)
│   ├── config.rs          # config export/import (passphrase-sealed files)
│   ├── usage.rs           # usage stats + recent requests
│   ├── bootstrap.rs       # seed store from a JSON document
│   └── commands.rs        # shared helpers + usage output
├── domain/
│   └── mod.rs             # pure data model; no I/O
├── storage/
│   └── mod.rs             # embedded SQLite persistence, encrypted tokens
├── server/
│   └── mod.rs             # HTTP server: auth, rate limit, handlers
├── router/
│   └── mod.rs             # model resolution, failover, logging
├── health/
│   └── mod.rs             # background probe loop, status machine
├── crypto/
│   └── mod.rs             # master key, ChaCha20-Poly1305, Argon2id, passwords
├── translator/
│   └── mod.rs             # OpenAI-compatible shaping per provider kind
├── lib.rs                 # crate root, module wiring, VERSION
└── main.rs                # thin entry point: parse args, call cli::run
```

There is also a top-level `tests/` directory for integration tests:

```
tests/
├── e2e_test.rs             # server against a real HTTP caller
└── ...                     # (future integration suites)
```

## Conventions

### Style

- Run `cargo fmt --all` before every commit.
- Clippy with warnings treated as errors (`-D warnings`) in CI and locally.
- No `#![allow(...)]` globs without a comment explaining why.

### Documentation

- Module-level doc comments (`//!`) for every public module.
- Public types, functions, and fields documented.
- Examples are tested where practical.
- The `gen-docs` binary regenerates the CLI reference, man pages, and shell
  completions from the clap definition, so keep that definition consistent with
  the actual behavior.

### Testing

- Unit tests live inline with the code they cover, in `#[cfg(test)]` blocks.
- Integration tests live in `tests/` and exercise the binary or library from the
  outside.
- E2E tests that need a running proxy should use the mock upstream in
  `docker/mock.py`. Start it with `make docker-up` and tear down with
  `make docker-down`.
- Tests must not depend on network access to real providers.

### Error handling

- Use `thiserror` for domain errors where appropriate (current crate uses `anyhow`
  for the app layer and descriptive strings where error types are not yet
  extracted).
- Do not leak provider tokens in error messages, logs, or output.

## Common tasks

### Adding a new CLI command

1. Add the subcommand to the clap builder in `cli/app.rs`.
2. Add the handler in the appropriate `cli/*.rs` module.
3. Wire the handler into the dispatch in `cli/mod.rs`.
4. Run `make docs` to regenerate the reference.
5. Update this guide if the layout changed.

### Adding a new data type

1. Add the type to `domain/mod.rs` with `#[derive(Debug, Clone, Serialize, Deserialize)]`.
2. Add persistence methods in `storage/mod.rs` (migrations included if the
   table is new).
3. Update the CLI that manages it.
4. Update the architecture and configuration docs if the model changed.

### Changing the storage schema

1. Add a migration step in `storage/mod.rs` that checks for the new column/table
   and adds it if missing.
2. Make sure existing stores upgrade without data loss.
3. Note the change in the development notes for the next release.

## Code quality gates

These run in CI and are expected to pass before merge:

| Gate                  | Command                               | What it checks                               |
|-----------------------|---------------------------------------|----------------------------------------------|
| Format                | `cargo fmt --all --check`             | Runs rustfmt over the whole crate.           |
| Clippy                | `cargo clippy --all-targets -- -D warnings` | Lints with warnings as errors.         |
| Tests                 | `cargo test --all-targets`            | All unit and integration tests.              |
| Release build         | `cargo build --release`               | Produces a release binary.                   |
| Docs generation       | `./target/release/agos-proxy gen-docs --all` | CLI reference, man pages, completions. |

Run them locally with `make ci`.

## Docker test stack

The `docker-compose.yml` spins up two services:

- `mock` — a tiny Python server (`docker/mock.py`) that speaks a minimal
  OpenAI-compatible API on port 9999.
- `proxy` — the AGOS Proxy binary, seeded from the `AGOS_SETUP` document.

Start it:

```sh
make docker-up
```

Stop it and wipe state:

```sh
make docker-down
```

The stack is used for integration testing the HTTP server and routing. You can
also use it to manually exercise the proxy:

```sh
curl -H "Authorization: Bearer sk-mock" \
     -H "Content-Type: application/json" \
     http://localhost:8080/v1/chat/completions \
     -d '{"model":"main/chat","messages":[{"role":"user","content":"hi"}]}'
```

## Mock upstream

`docker/mock.py` is a minimal OpenAI-compatible server. It handles:

- `POST /v1/chat/completions` (streaming and non-streaming).
- `POST /v1/completions`.
- `POST /v1/embeddings`.
- `GET /v1/models`.

It is intentionally simple — it echoes back a fixed response and does not
implement full OpenAI semantics. It exists only so the test stack does not need
real API keys.

## Versioning

The crate version lives in `src/lib.rs` as `VERSION` (and the same value is
bumped in `Cargo.toml`). The CLI prints its version with `agos-proxy --version`.

See `docs/workflow.md` for the release process.

## Contribution workflow

See `CONTRIBUTING.md` for the full contribution guide. In brief:

1. Fork and branch.
2. Keep changes small and focused.
3. Make sure `make check` passes.
4. Open a PR against `main`.
5. Address review feedback.

## Release checklist

Before cutting a release:

- [ ] `make check` is green.
- [ ] `Cargo.toml` version and `src/lib.rs` `VERSION` match.
- [ ] `CHANGELOG.md` is updated.
- [ ] Tagged release notes are prepared.
- [ ] Release binary built and smoke-tested.
- [ ] Docker image rebuilt and smoke-tested.
- [ ] Docs regenerated (`make docs`).
