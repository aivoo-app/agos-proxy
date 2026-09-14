#!/usr/bin/env bash
# ==============================================================================
# AGOS Proxy — top-level development & CI makefile.
#
# This is the entry point for day-to-day work. It delegates most real work to
# the per-host CI scripts in `scripts/ci/` so the same steps can be run on a
# laptop and in CI with identical results.
#
# Quick reference:
#   make help
#   make check          # format + lint + clippy + test (fast feedback)
#   make test           # run the test suite
#   make fmt            # format with rustfmt
#   make lint           # clippy full warnings
#   make build          # release build
#   make install        # install binary to ~/.cargo/bin
#   make docs           # generate CLI reference + man pages + completions
#   make docker-up      # start the docker-compose test stack
#   make docker-down    # stop and remove it
#   make ci             # run the full CI pipeline as on GitHub Actions
# ==============================================================================

# ---------------------------------------------------------------------------
# Layout
# ---------------------------------------------------------------------------
PROJECT_ROOT := $(shell pwd)
CARGO := cargo
RUSTUP := rustup
SHELL := /usr/bin/env bash
.SHELLFLAGS := -c -euo pipefail

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------
.PHONY: help
help: ## Print this help text.
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

# ---------------------------------------------------------------------------
# Build & test
# ---------------------------------------------------------------------------
.PHONY: check
check: fmt lint test ## Format, lint, and test (fast feedback loop).

.PHONY: fmt
fmt: ## Format the crate with rustfmt.
	$(CARGO) fmt --all

.PHONY: lint
lint: ## Run clippy with all warnings treated as errors.
	$(CARGO) clippy --all-targets -- -D warnings

.PHONY: test
test: ## Run the test suite.
	$(CARGO) test --all-targets

.PHONY: test-e2e
test-e2e: ## Run only the end-to-end integration tests.
	$(CARGO) test --test e2e_test

.PHONY: build
build: ## Build a release binary.
	$(CARGO) build --release

.PHONY: install
install: build ## Install the release binary to ~/.cargo/bin.
	$(CARGO) install --path . --root $$HOME/.cargo

# ---------------------------------------------------------------------------
# Documentation
# ---------------------------------------------------------------------------
.PHONY: docs
docs: build ## Generate CLI reference, man pages, and shell completions.
	./target/release/agos-proxy gen-docs --all --output-dir docs/

# ---------------------------------------------------------------------------
# Docker test stack
# ---------------------------------------------------------------------------
.PHONY: docker-up
docker-up: ## Start the docker-compose test stack (proxy + mock upstream).
	docker compose up -d --build

.PHONY: docker-down
docker-down: ## Stop and remove the docker-compose test stack.
	docker compose down -v --remove-orphans

.PHONY: docker-logs
docker-logs: ## Tail logs from the docker-compose stack.
	docker compose logs -f

# ---------------------------------------------------------------------------
# CI
# ---------------------------------------------------------------------------
.PHONY: ci
ci: ## Run the full CI pipeline (identical to GitHub Actions).
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test --all-targets
	$(CARGO) build --release
	./target/release/agos-proxy gen-docs --all --output-dir /tmp/agos-ci-docs

.PHONY: ci-check
ci-check: ## CI with --check style (used by pre-merge gates).
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test --all-targets

# ---------------------------------------------------------------------------
# Housekeeping
# ---------------------------------------------------------------------------
.PHONY: clean
clean: ## Remove build artifacts.
	$(CARGO) clean

.PHONY: outdated
outdated: ## Show outdated dependencies (requires cargo-outdated or cargo upgrade).
	$(CARGO) outdated || true

.PHONY: update
update: ## Update dependencies (interactive).
	$(CARGO) update
