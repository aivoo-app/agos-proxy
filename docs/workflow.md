# AGOS Proxy Workflow

How this project is developed day to day: branching, CI, versioning, and
releases. The goal is a boring, repeatable process that does not depend on any
one person remembering a ritual.

## Repository layout

- `main` is the trunk. It should always be releasable.
- Feature work happens on short-lived branches from `main`.
- Releases are tagged from `main`.
- The `Makefile` and `.github/workflows/ci.yml` encode the same checks that run
  on every push and PR.

## Branching model

- **Trunk-based, PR-gated.** `main` is the only long-lived branch.
- **Feature branches** are named by topic: `feat/`, `fix/`, `docs/`, `chore/`.
- **PRs target `main`.** No release branches, no long-lived integration branches.
- **Squash-merge** by default, so `main` history reads as a sequence of
  intentional changes. The PR title becomes the commit message on merge.

## Pull requests

1. Open a PR from your branch against `main`.
2. CI runs automatically: format, clippy, tests, release build, docs smoke test.
3. At least one review is expected before merge for non-trivial changes.
4. Security-sensitive changes (auth, tokens, crypto, rate limiting, exposure)
   require explicit review attention — mention that in the PR description.
5. Merge when green and reviewed.

PR expectations:

- Small and focused. One concern per PR where practical.
- Docs updated if behavior changes.
- CLI reference regenerated (`make docs`) if CLI flags or commands change.
- Tests added or updated for new behavior.

## Continuous integration

Every push to a PR and every push to `main` runs the same CI job, defined in
`.github/workflows/ci.yml`.

The CI job:

1. Checks out the code.
2. Installs the Rust toolchain (stable + rustfmt + clippy).
3. Caches cargo.
4. Runs `cargo fmt --all --check`.
5. Runs `cargo clippy --all-targets -- -D warnings`.
6. Runs `cargo test --all-targets`.
7. Runs `cargo build --release`.
8. Runs the `gen-docs` smoke test to ensure CLI docs can be regenerated.
9. In a second job, runs the docker-compose stack and exercises the proxy with
   a few smoke requests (models listing, chat completion, streaming).

The docker-compose job is the integration spot-check. It does not replace full
testing; it exists to catch regressions that only appear when the proxy is running
with a real HTTP caller.

Local reproduction of CI:

```sh
make ci
```

## Versioning

AGOS Proxy uses a simple version scheme:

- `Cargo.toml` edition and version are the source of truth for the crate version.
- `src/lib.rs` exposes a `VERSION` constant that mirrors `Cargo.toml`. Keep them
  in sync manually.
- The CLI prints its version with `agos-proxy --version`.

Versioning policy:

- Patch releases for bug fixes and docs.
- Minor releases for new features that do not break existing configurations.
- Major releases for breaking changes (store schema, CLI flags, API surface).

Today the project is pre-1.0, so breaking changes can land in minor bumps. The
intent is to settle on 1.0 once the core surface (CLI commands, API endpoints,
storage schema, routing models) is stable enough that breakage is exceptional.

## Changelog

`CHANGELOG.md` is the human-readable record of what changed. It follows a
Keep-a-Changelog style:

- `## [Unreleased]` for changes in progress.
- `## [x.y.z] - YYYY-MM-DD` for released versions.
- Sections: Added, Changed, Deprecated, Removed, Fixed, Security.

Update `CHANGELOG.md` as part of the PR that introduces the change, not at
release time. That keeps the log accurate and reduces release-day work.

## Release process

A release is a tag on `main` with a matching version bump, changelog entry, and
built artifacts.

### Checklist

- [ ] `make check` is green on `main`.
- [ ] `CHANGELOG.md` is updated for the new version; the `[Unreleased]` section
  is moved into a versioned section with a date.
- [ ] `Cargo.toml` version matches `src/lib.rs` `VERSION`.
- [ ] CLI reference regenerated (`make docs`) and the regenerated files committed.
- [ ] Release binary built: `cargo build --release`.
- [ ] Release binary smoke-tested: `./target/release/agos-proxy --version` and a
  couple of CLI smoke commands.
- [ ] Docker image built and smoke-tested with the docker-compose stack.
- [ ] Tag pushed: `git tag -a vX.Y.Z -m "vX.Y.Z"` and `git push --tags`.
- [ ] Release notes published (GitHub release or equivalent).
- [ ] Any announcement (if needed) sent.

### Pre-release

For a release candidate, tag `-rc.N` and test. Do not merge the RC tag into
release history; retag the final version when ready.

## Docs discipline

Docs are part of the deliverable, not an afterthought. The docs tree is:

### Landing
- `README.md` — landing page, quickstart, A-Z workflow, highlights, glossary.
- `QUICKREF.md` — one-page CLI quick reference.
- `CHANGELOG.md` — version history.

### Architecture
- `docs/architecture.md` — design, crate layout, data model, build phases.
- `docs/adr/001-why-rust.md` — why Rust was chosen.
- `docs/adr/002-sqlite-store.md` — why SQLite for persistence.
- `docs/adr/003-chacha20-encryption.md` — encryption algorithm choices.
- `docs/adr/004-cli-first-design.md` — CLI-first design philosophy.

### Tutorials
- `docs/tutorials/getting-started.md` — step-by-step install to first request.
- `docs/tutorials/common-patterns.md` — common configuration patterns.
- `docs/tutorials/openai-sdk-integration.md` — Python, JS, Go, Rust SDK usage.

### Reference
- `docs/CLI.md` — auto-generated command reference.
- `docs/configuration.md` — config location, env vars, bootstrap schema, secrets.
- `docs/reference/environment.md` — complete environment variable reference.
- `docs/api.md` — HTTP API surface, auth, rate limiting, errors, streaming.
- `docs/providers.md` — provider kinds, translators, per-provider setup notes.
- `docs/failover.md` — failover mechanics, health states, streaming limitation.
- `docs/security.md` — threat model, encryption, passwords, token handling.

### Operations
- `docs/deployment.md` — Docker, binary install, systemd, reverse proxy.
- `docs/operations/runbook.md` — day-2 operations runbook.
- `docs/operations/monitoring.md` — monitoring, observability, alerting.
- `docs/operations/migrations.md` — store migration guide.

### Contributing
- `CONTRIBUTING.md` — contribution workflow and review expectations.
- `docs/development.md` — dev environment, testing, code layout.
- `docs/contributing/testing.md` — testing guide (unit, integration, e2e).
- `docs/contributing/code-of-conduct.md` — code of conduct.
- `docs/workflow.md` — this file (branching, releases, CI, versioning).

### Help
- `docs/help/faq.md` — frequently asked questions.
- `docs/help/troubleshooting.md` — common issues and solutions.

When a change affects any of these, update the relevant doc in the same PR. If
you are not sure which doc covers a change, pick the most specific one and note
in the PR that the others may need a glance.

## Issue hygiene

- Use the issue tracker for bugs, features, and doc gaps.
- Use `SECURITY.md` for security vulnerabilities (private, not public issues).
- A good issue has: what is expected, what is happening, steps to reproduce,
  environment (binary version, OS, relevant config).
- Labels are used sparingly and consistently: `bug`, `feat`, `docs`, `security`.

## Reviews and merges

- Maintainers review for correctness, safety, test coverage, and docs.
- Squash-merge to keep `main` linear and readable.
- After merge, delete the feature branch.

## Automation

Current automation:

- CI formatting, linting, testing, build, docs smoke test.
- Docker-compose integration smoke test on CI.
- `make docs` for CLI reference regeneration.

Future automation, as the project grows:

- Release drafting from the changelog.
- Automatic updating of the generated CLI reference on merge to `main`.
- Dependency update PRs via dependabot or similar.

## Day-to-day rhythm

For a maintainer:

1. Review PRs in `main`'s queue.
2. Merge when green and reviewed.
3. Cut releases from `main` on a cadence that matches the changelog — not too
   often, not too rarely.
4. Keep the docker-compose stack and CI healthy; they are the tripwires.

For a contributor:

1. Pick an issue or open one.
2. Branch, change, test, document, PR.
3. Respond to review feedback.
4. Merge and move on.

The intent is that neither side depends on undocumented ritual. If a step is not
written down here or in the relevant doc, it should be added.
