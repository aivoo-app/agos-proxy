# AGOS Proxy

<div align="center">

[![CI](https://github.com/aivoo-app/agos-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/aivoo-app/agos-proxy/actions/workflows/ci.yml)
[![Release](https://github.com/aivoo-app/agos-proxy/actions/workflows/release.yml/badge.svg)](https://github.com/aivoo-app/agos-proxy/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75+-blue.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20windows-lightgrey.svg)]()
[![Version](https://img.shields.io/badge/version-0.1.0-green.svg)](Cargo.toml)

**A self-hosted, CLI-managed AI gateway that gives any agent an OpenAI-compatible
endpoint backed by automatic multi-provider failover.**

[Quick Start](#quick-start) •
[Features](#highlights) •
[A-Z Workflow](#a-z-workflow) •
[Docs](docs/) •
[Contributing](CONTRIBUTING.md) •
[Changelog](CHANGELOG.md)

</div>

---

## Table of Contents

- [What is AGOS Proxy?](#what-is-agos-proxy)
- [Highlights](#highlights)
- [Quick Start](#quick-start)
- [A-Z Workflow](#a-z-workflow)
  - [Step 1: Install](#step-1-install)
  - [Step 2: Create a Profile](#step-2-create-a-profile)
  - [Step 3: Add Providers](#step-3-add-providers)
  - [Step 4: Create a Proxy](#step-4-create-a-proxy)
  - [Step 5: Create a Route](#step-5-create-a-route)
  - [Step 6: Start the Server](#step-6-start-the-server)
  - [Step 7: Make a Request](#step-7-make-a-request)
  - [Step 8: Monitor Usage](#step-8-monitor-usage)
  - [Step 9: Test Interactively](#step-9-test-interactively)
  - [Step 10: Export/Import Config](#-step-10-exportimport-config)
- [Common Workflows](#common-workflows)
- [Architecture Overview](#architecture-overview)
- [Documentation Index](#documentation-index)
- [Project Status](#project-status)
- [Glossary](#glossary)
- [License](#license)

---

## What is AGOS Proxy?

AGOS Proxy sits between an agent and its LLM providers. Instead of an agent
calling `deepseek-v4-flash` directly, it calls a **route** such as
`programmer/php-developer-3.5-flash` that AGOS Proxy owns. Behind that single
route is an ordered list of real models from real providers; AGOS Proxy tries
them in priority order, tracks which are healthy, and transparently fails over
so a request keeps getting answered even if several underlying providers are
down at once.

Conceptually it is closest to LiteLLM's proxy/gateway mode, but with
profile-based multi-tenancy and failover as first-class concepts rather than
add-ons.

### When to Use AGOS Proxy

- You have multiple LLM providers and want automatic failover
- You want a single OpenAI-compatible endpoint that never goes down
- You need per-agent or per-team rate limits and usage tracking
- You want to swap providers without changing client code
- You want encrypted storage of provider API keys

### When NOT to Use AGOS Proxy

- You have a single provider and don't need failover — call it directly
- You need multi-node clustering — AGOS Proxy is single-process today
- You need a fully managed service — AGOS Proxy is self-hosted

---

## Highlights

| Feature | Description |
|---------|-------------|
| **OpenAI-compatible API** | `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models` — drop-in replacement for any OpenAI SDK |
| **Multi-profile, multi-tenant** | Separate credentials and routing rules per agent, team, or use case |
| **Ordered model fallback chains** | Per route with automatic health tracking and background recovery |
| **Three routing strategies** | Priority (strict fallback), round-robin, and weighted round-robin |
| **Streaming (SSE) passthrough** | Mirrors each provider's streaming behavior |
| **Provider translation** | Native support for Anthropic (`/v1/messages`) and Google Gemini (`generateContent`), plus pass-through for any OpenAI-compatible provider |
| **CLI-first configuration** | Interactive wizards and scriptable non-interactive flags; a guided `setup` wizard covers first-time users |
| **Encrypted secrets at rest** | Provider tokens encrypted with ChaCha20-Poly1305, master key sealed in the local store; optional per-profile password protection |
| **Usage, cost, and latency visibility** | Per model and per route |
| **Portable config export/import** | Move a full profile setup between machines with a passphrase-sealed file |

---

## Quick Start

### Prerequisites

- **Rust** 1.75+ (or use the Docker image)
- A terminal

### Build from Source

```sh
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
cargo build --release
```

The binary is `target/release/agos-proxy`. Add it to your PATH or invoke it directly.

### Using Docker

```sh
docker compose up -d
```

This starts AGOS Proxy with a mock upstream, seeded with a sample profile. See [docs/deployment.md](docs/deployment.md) for production Docker usage.

---

## A-Z Workflow

This is the complete walkthrough from zero to a working proxy. Follow each step in order.

### Step 1: Install

Choose one of:

```sh
# Option A: Build from source
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
cargo build --release
sudo cp target/release/agos-proxy /usr/local/bin/

# Option B: Install via cargo
cargo install --path .

# Option C: Use Docker
docker pull ghcr.io/aivoo-app/agos-proxy:latest
```

Verify:
```sh
agos-proxy --version
```

### Step 2: Create a Profile

A profile is a tenant — it owns providers, proxies, routes, and has its own API token.

```sh
# Interactive wizard (recommended for first-time users)
agos-proxy profile create

# Non-interactive (for scripts)
agos-proxy profile create --name coder1
```

The CLI prints your **API token** (the profile `id`). Save it — you need it for API calls.

```
Profile "coder1" created.
API token: abc123def456ghi789...
```

**What just happened:**
- A profile named `coder1` was created in the SQLite store
- A random 128-bit API token was generated (doubles as the profile `id`)
- The store lives at `~/.config/agos-proxy/agos.db` by default

### Step 3: Add Providers

A provider is an upstream LLM service (DeepSeek, OpenRouter, Anthropic, etc.).

```sh
# Interactive wizard
agos-proxy provider add --profile coder1

# The wizard prompts for:
#   - name: deepseek (your label)
#   - base URL: https://api.deepseek.com
#   - API token: sk-... (your real DeepSeek key — stored encrypted)
#   - kind: OpenAI-compatible
```

Add more providers for failover:
```sh
agos-proxy provider add --profile coder1
# name: anthropic
# base URL: https://api.anthropic.com
# API token: sk-ant-...
# kind: Anthropic
```

**What just happened:**
- Provider credentials were stored encrypted with ChaCha20-Poly1305
- The master key for encryption lives only in memory + the store's `meta` table
- Provider tokens are never shown in plaintext after creation

### Step 4: Create a Proxy

A proxy is a named group of routes — think of it as an API namespace.

```sh
# Interactive wizard
agos-proxy proxy create --profile coder1

# The wizard prompts for:
#   - name: Programmer (your label)
```

### Step 5: Create a Route

A route is an addressable fallback chain. Callers request `<proxy>/<route>` and AGOS Proxy picks the best model.

```sh
# Interactive wizard — walks through building the model chain
agos-proxy route create --proxy Programmer

# The wizard prompts for:
#   - route name: php-developer-3.5-flash
#   - strategy: priority (default)
#   - model entries: pick a provider + model ID for each rung
```

Example chain built interactively:
```
1. deepseek → deepseek-v4-flash    (priority 1, first try)
2. anthropic → claude-3-haiku      (priority 2, fallback)
```

### Step 6: Start the Server

```sh
# Default: bind to 127.0.0.1:3000
agos-proxy serve

# Custom bind
agos-proxy serve --bind 0.0.0.0:8080

# Give each model attempt 30s before failing over to the next entry
# (default is 10s; slow reasoning models may need more)
agos-proxy serve --attempt-timeout 30
```

The `--attempt-timeout` flag (or the `AGOS_ATTEMPT_TIMEOUT_SECS`
environment variable) controls how long a single model in a route's chain may
take to start responding before the router gives up on it and fails over to
the next entry. Precedence: flag > environment variable > 10s default.

**What happens on startup:**
- The SQLite store is opened (migrations applied if needed)
- The background health-probe loop starts (pings unhealthy providers every 30s)
- The HTTP server starts listening

### Step 7: Make a Request

Use any OpenAI-compatible client. Here's curl:

```sh
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-developer-3.5-flash",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

With the Python OpenAI SDK:
```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:3000/v1",
    api_key="YOUR_PROFILE_TOKEN",
)

response = client.chat.completions.create(
    model="Programmer/php-developer-3.5-flash",
    messages=[{"role": "user", "content": "Hello!"}],
)
print(response.choices[0].message.content)
```

With the Node.js OpenAI SDK:
```js
import OpenAI from 'openai';

const client = new OpenAI({
  baseURL: 'http://localhost:3000/v1',
  apiKey: 'YOUR_PROFILE_TOKEN',
});

const response = await client.chat.completions.create({
  model: 'Programmer/php-developer-3.5-flash',
  messages: [{ role: 'user', content: 'Hello!' }],
});
console.log(response.choices[0].message.content);
```

### Step 8: Monitor Usage

```sh
# Per-model aggregates: calls, failures, latency, tokens
agos-proxy usage stats --profile coder1

# Recent individual requests
agos-proxy usage recent --profile coder1 --limit 20

# Live route health status
agos-proxy route status --route php-developer-3.5-flash
```

### Step 9: Test Interactively

Before wiring up a client, test a route directly:

```sh
# Interactive chat REPL
agos-proxy chat

# Full-screen TUI
agos-proxy chat --tui
```

### Step 10: Export/Import Config

Move a profile setup between machines:

```sh
# Export (you'll be prompted for a passphrase)
agos-proxy config export --profile coder1 --output coder1-sealed.json

# Import on another machine
agos-proxy config import --path coder1-sealed.json
```

The sealed file contains the full profile tree (providers with tokens, proxies, routes, entries), encrypted with a passphrase-derived key. On import, tokens are re-encrypted with the destination store's master key.

---

## Common Workflows

### Guided Setup Wizard

First-time users can run the all-in-one wizard instead of individual commands:

```sh
agos-proxy setup
```

This walks through: profile → providers → proxies → routes → summary.

### Scripted / CI Setup

For containers and CI, use `bootstrap` with a JSON document:

```sh
cat <<'EOF' > setup.json
{
  "profile": "ci-agent",
  "providers": [
    {
      "name": "openrouter",
      "base_url": "https://openrouter.ai/api/v1",
      "auth_token": "sk-or-v1-...",
      "kind": "openai_compatificant"
    }
  ],
  "proxies": [
    {
      "name": "main",
      "routes": [
        {
          "name": "chat",
          "models": [
            {"provider": "openrouter", "model": "openai/gpt-4o"}
          ]
        }
      ]
    }
  ]
}
EOF

agos-proxy bootstrap from-file setup.json
# Prints the generated API token
```

### Adding a New Provider to an Existing Chain

```sh
# Add the provider
agos-proxy provider add --profile coder1
# ... enter details ...

# Add it as a new rung in an existing route
agos-proxy route model add --proxy Programmer
# Pick the route, pick the new provider, set priority
```

### Rotating a Compromised API Token

```sh
# Rotate the profile bearer token
agos-proxy profile token rotate coder1

# Update all clients with the new token
```

### Disaster Recovery

```sh
# 1. Export before anything goes wrong
agos-proxy config export --profile coder1 --output backup.json

# 2. On a new machine, restore
agos-proxy config import --path backup.json
# The imported profile gets a fresh bearer token; tokens are re-encrypted
```

### Setting Rate Limits

```sh
# Limit a profile to 120 requests per minute
agos-proxy profile limit coder1 120

# Remove the limit
agos-proxy profile limit coder1 0
```

---

## Architecture Overview

For the full architecture reference, see [docs/architecture.md](docs/architecture.md).

### Mental Model

```
Profile ("coder1")  ← tenant, owns everything
├── Providers       ← how we reach the outside world
│   ├── Deepseek    base_url + auth_token (encrypted)
│   ├── Anthropic   base_url + auth_token (encrypted)
│   └── Google      base_url + auth_token (encrypted)
└── Proxies         ← what callers may request
    └── Proxy: "Programmer"
        ├── Route: "php-developer-3.5-flash"
        │     1. deepseek-v4-flash    (priority 1)
        │     2. claude-3-haiku       (priority 2)
        └── Route: "code-reviewer"
              1. gpt-4o               (priority 1)
              2. gemini-2.0-flash      (priority 2)
```

### Request Lifecycle

```
Caller ──/v1/chat/completions──► AGOS Proxy ──resolve──► Route
  │                                    │                  │
  │                                    │              strategy?
  │                                    │            ╱    │    ╲
  │                                  auth     Priority  Round  Weighted
  │                                  rate     try in   Robin   Round Robin
  │                                  limit    order
  │                                    │
  │                                    ▼
  │                            Translator (if needed)
  │                            (Anthropic, Google, pass-through)
  │                                    │
  │                                    ▼
  │                            Upstream Provider
  │                                    │
  ◄──────── response ──────────────────┘
```

### Crate Layout

```
src/
├── lib.rs           crate root, module wiring, VERSION
├── main.rs          thin entry point: parse args, call cli::run
├── cli/             command tree (clap) + interactive wizards + TUI
├── domain/          core data model: profiles, providers, proxies, routes
├── storage/         persistence of the domain model (embedded SQLite)
├── server/          OpenAI-compatible HTTP surface + auth + rate limiting
├── router/          model resolution, failover execution, routing state
├── health/          background probe loop for unhealthy entries
├── crypto/          secrets at rest (ChaCha20-Poly1305 + Argon2id)
├── translator/      OpenAI ⇄ provider shape translation (Anthropic, Google)
└── bin/
    └── gen-docs     developer-only: man pages, completions, CLI markdown
```

---

## Documentation Index

### Tutorials
| Document | Contents |
|----------|----------|
| [docs/tutorials/getting-started.md](docs/tutorials/getting-started.md) | Step-by-step install → first request |
| [docs/tutorials/common-patterns.md](docs/tutorials/common-patterns.md) | Common configuration patterns |
| [docs/tutorials/openai-sdk-integration.md](docs/tutorials/openai-sdk-integration.md) | Using with Python, JS, and other OpenAI SDKs |

### Reference
| Document | Contents |
|----------|----------|
| [docs/architecture.md](docs/architecture.md) | Full design, crate layout, data model, build phases |
| [docs/CLI.md](docs/CLI.md) | Auto-generated full command reference |
| [docs/configuration.md](docs/configuration.md) | Config location, env vars, bootstrap JSON schema, secrets |
| [docs/api.md](docs/api.md) | OpenAI-compatible API surface, auth, rate limiting, errors |
| [docs/providers.md](docs/providers.md) | Provider kinds, translators, per-provider notes |
| [docs/security.md](docs/security.md) | Threat model, encryption, password hashing, token handling |
| [docs/reference/environment.md](docs/reference/environment.md) | Complete environment variable reference |

### Operations
| Document | Contents |
|----------|----------|
| [docs/operations/runbook.md](docs/operations/runbook.md) | Day-2 operations runbook |
| [docs/operations/monitoring.md](docs/operations/monitoring.md) | Monitoring, observability, alerting |
| [docs/operations/migrations.md](docs/operations/migrations.md) | Store migration guide |
| [docs/deployment.md](docs/deployment.md) | Docker, binary install, systemd, reverse proxy |

### Help
| Document | Contents |
|----------|----------|
| [docs/help/faq.md](docs/help/faq.md) | Frequently asked questions |
| [docs/help/troubleshooting.md](docs/help/troubleshooting.md) | Common issues and solutions |

### Contributing
| Document | Contents |
|----------|----------|
| [CONTRIBUTING.md](CONTRIBUTING.md) | Contribution workflow and review expectations |
| [docs/contributing/testing.md](docs/contributing/testing.md) | Testing guide (unit, integration, e2e) |
| [docs/contributing/code-of-conduct.md](docs/contributing/code-of-conduct.md) | Code of conduct |
| [docs/development.md](docs/development.md) | Dev environment, testing, code layout |
| [docs/workflow.md](docs/workflow.md) | Branching, releases, CI, versioning policy |

### Architecture Decision Records
| Document | Contents |
|----------|----------|
| [docs/adr/001-why-rust.md](docs/adr/001-why-rust.md) | Why Rust was chosen |
| [docs/adr/002-sqlite-store.md](docs/adr/002-sqlite-store.md) | Why SQLite for persistence |
| [docs/adr/003-chacha20-encryption.md](docs/adr/003-chacha20-encryption.md) | Encryption algorithm choices |
| [docs/adr/004-cli-first-design.md](docs/adr/004-cli-first-design.md) | CLI-first design philosophy |

### Top-Level Quick Reference
| Document | Contents |
|----------|----------|
| [QUICKREF.md](QUICKREF.md) | One-page CLI quick reference |

---

## Project Status

AGOS Proxy is **v0.1.0** — pre-1.0. The core surface is functional:

| Area | Status |
|------|--------|
| Profile CRUD | ✅ Stable |
| Provider CRUD (with encrypted tokens) | ✅ Stable |
| Proxy CRUD | ✅ Stable |
| Route CRUD (with model chains) | ✅ Stable |
| `/v1/chat/completions` (streaming + non-streaming) | ✅ Stable |
| `/v1/completions` (passthrough) | ✅ Stable |
| `/v1/embeddings` (passthrough) | ✅ Stable |
| `/v1/models` (auto-listing) | ✅ Stable |
| Automatic failover with health tracking | ✅ Stable |
| Priority routing | ✅ Stable |
| Round-robin routing | ✅ Stable |
| Weighted round-robin routing | ✅ Stable |
| Background health probe loop | ✅ Stable |
| Bearer token auth | ✅ Stable |
| Per-profile rate limiting | ✅ Stable |
| Interactive CLI wizards | ✅ Stable |
| Non-interactive flag-driven commands | ✅ Stable |
| Full-screen TUI chat | ✅ Stable |
| Config export/import (passphrase-sealed) | ✅ Stable |
| Bootstrap from JSON | ✅ Stable |
| Usage logging + per-model aggregates | ✅ Stable |
| Docker test stack with mock upstream | ✅ Stable |
| Auto-generated CLI docs, man pages, completions | ✅ Stable |
| Capability-aware routing | 🔄 Partial (stored, partially filtered) |
| Cost tracking | 📋 Planned |
| Failover webhooks | 📋 Planned |
| Richer CLI stats / history search | 📋 Planned |
| Provider-specific advanced options | 📋 Planned |
| Multi-node clustering | 📋 Planned |
| TLS termination (built-in) | 📋 Planned |

---

## Glossary

| Term | Definition |
|------|------------|
| **Profile** | A tenant. Owns providers, proxies, and routes. Has an opaque `id` that doubles as the API bearer token. |
| **Provider** | An upstream LLM service (e.g., DeepSeek, Anthropic, OpenRouter). Defined by `base_url`, `auth_token`, and `kind`. |
| **Proxy** | A named group of routes. Think of it as an API namespace (e.g., "Programmer"). |
| **Route** | An addressable fallback chain. Exposed to callers as `<proxy>/<route>`. Behind the scenes, an ordered list of models. |
| **Route Entry / Model Entry** | A single rung in a route's fallback chain. Points at a specific model on a specific provider, with a priority and weight. |
| **Strategy** | How entries in a route are selected: `priority` (strict order), `round_robin` (distribute), `weighted` (weighted distribution). |
| **Health Status** | Per-entry state: `Healthy`, `Degraded`, `Unhealthy`, or `Disabled`. Controls whether an entry receives live traffic. |
| **Failover** | The process of trying the next entry in a chain when the current one fails. |
| **Translator** | Code that converts OpenAI-compatible request/response shapes to a provider's native format (Anthropic, Google). |
| **Bootstrap** | Non-interactive store seeding from a JSON document, for containers and CI. |
| **Portable File** | A passphrase-sealed file containing a full profile tree, for export/import between machines. |
| **Store** | The single SQLite file holding all profiles, providers, proxies, routes, and usage data. |
| **AGOS_HOME** | Environment variable overriding the data directory location. |

---

## License

MIT — see [LICENSE](LICENSE).
