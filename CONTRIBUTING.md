# Contributor Guide

How to contribute to AGOS Proxy: branching, PRs, review expectations, and the
release process.

## Code of conduct

Be kind, be precise, and assume good faith. We are small and we move fast; the
goal is good software, not perfect process.

## Getting started

1. Fork the repository.
2. Clone your fork.
3. Create a branch from `main`.
4. Make your change.
5. Run `make check` (or the equivalent `cargo fmt`, `cargo clippy -- -D warnings`,
   `cargo test`).
6. Open a PR against `main`.

## Branching

- Branch from `main` for feature work.
- Use descriptive branch names: `fix/provider-auth-header`, `feat/weighted-routing`,
  `docs/add-deployment-guide`.
- Keep branches short-lived. Rebase on `main` before opening the PR if `main`
  has moved.

## PR guidelines

- Keep PRs small and focused. One concern per PR.
- Include a description of what the PR does and why.
- If the PR changes user-visible behavior, update the relevant docs (README,
  architecture, configuration, API, CLI, security, failover, providers,
  development, deployment, workflow).
- If the PR changes the CLI, run `make docs` to regenerate the reference and
  include the regenerated files in the PR.
- If the PR changes the storage schema, document the migration and test
  upgrade from an older store.
- Link any related issue.

## Review expectations

- Reviewers look for correctness, safety, test coverage, and documentation.
- Security-sensitive changes (auth, tokens, crypto, rate limiting, exposure)
  get extra scrutiny.
- Tests are expected for new behavior. Existing behavior that changes gets tests
  too.
- Docs are expected to stay in sync with code. A PR that changes behavior without
  updating the docs may be returned for the doc update.

## Commit style

- Use clear, imperative commit messages: "Add weighted routing strategy", not
  "added weighted routing stuff".
- Keep commits atomic where practical.
- A PR may be squash-merged; the final message should summarize the change.

## Documentation

This project aims to keep its documentation in sync with the code. The docs live
in `docs/` and the top-level `README.md`. When you change behavior, update the
doc that covers that behavior.

The CLI reference is auto-generated from the clap definition via the `gen-docs`
binary. If you change CLI flags or commands, regenerate it with:

```sh
make docs
```

and include the regenerated files in your PR.

## Testing

- Unit tests live inline in the modules they cover.
- Integration tests live in `tests/`.
- E2E tests that need a running proxy use the mock upstream from `docker/mock.py`.
- Tests must not depend on real provider access.

Run the full suite:

```sh
make check
```

Or the components individually:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## Dependencies

- Minimize new dependencies.
- If you add one, justify it in the PR description.
- Keep `Cargo.lock` in sync.

## Security-sensitive changes

Changes to authentication, token handling, encryption, password hashing, rate
limiting, or exposure (bind address, CORS, etc.) are security-sensitive. Please
mention that in the PR description so reviewers can prioritize.

Do not open a public issue for a security vulnerability — see `SECURITY.md`.

## Releases

Releases are cut from `main` by a maintainer. The process:

1. Update `Cargo.toml` version and `src/lib.rs` `VERSION` to the new version.
2. Update `CHANGELOG.md` with the release notes.
3. Commit and tag.
4. Build the release binary and smoke-test it.
5. Build and smoke-test the Docker image.
6. Regenerate docs with `make docs`.
7. Push the tag and the commit.
8. Publish the release notes.

See `docs/workflow.md` for the detailed release checklist and versioning policy.

## Questions

Open an issue for questions that are not security-sensitive. For security issues,
see `SECURITY.md`.

## Thank you

Contributions of any size are welcome — bug fixes, docs, tests, provider notes,
and small refactors all count.
