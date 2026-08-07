# AGOS Proxy

<div align="center">

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75+-blue.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20windows-lightgrey.svg)()
[![Version](https://img.shields.io/badge/version-0.1.0-green.svg)](Cargo.toml)

**A self-hosted, CLI-managed AI gateway that gives any agent an OpenAI-compatible endpoint backed by automatic multi-provider failover.**

[Quick Start](#quick-start) &bull; [What It Does](#what-it-does) &bull; [The Model You Call](#the-model-you-call) &bull; [Identity Masking](#identity-masking) &bull; [A--Z Walkthrough](#a--z-walkthrough) &bull; [Docs Index](#docs-index) &bull; [Contributing](CONTRIBUTING.md)

</div>

---

## Quick Start

Get a working proxy in about two minutes:

```sh
# 1. Build
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
cargo build --release

# 2. Create a profile and grab its token
./target/release/agos-proxy profile create --name coder1
# saves the token it prints; you will need it for API calls

# 3. Add a provider (your real API key goes here, stored encrypted)
./target/release/agos-proxy provider add --profile coder1
# name: openai
# base_url: https://api.openai.com/v1
# auth_token: sk-...
# kind: openai_compatible

# 4. Create a proxy + route
./target/release/agos-proxy proxy create --profile coder1
# name: Programmer

./target/release/agos-proxy route create --proxy Programmer
# route name: php-dev
# strategy: priority
# then add model entries: openai/gpt-4o at priority 1, etc.

# 5. Start the server
./target/release/agos-proxy serve --bind 127.0.0.1:3000
```

```sh
# 6. Call it like any OpenAI endpoint
curl http://localhost:3000/v1/chat/completions   -H "Authorization: Bearer <YOUR_TOKEN>"   -H "Content-Type: application/json"   -d '{"model":"Programmer/php-dev","messages":[{"role":"user","content":"hi"}]}'
```

For a guided first-time walkthrough, run `./target/release/agos-proxy setup` instead of the steps above. For scripted or container environments, see [Bootstrap JSON](#bootstrap-json).

---

## What It Does

AGOS Proxy sits between an agent and its LLM providers. The agent calls one stable OpenAI-compatible endpoint; behind that endpoint, AGOS Proxy fans the request out across an ordered chain of real providers with automatic failover. If the first provider is down, rate-limited, or slow, the proxy tries the next one within a single caller request. The agent sees one request and one response.

Conceptually it is closest to LiteLLM proxy/gateway mode, with three differences: it is a self-contained Rust binary (no Python runtime), it is configured entirely through a CLI rather than a config file, and it has first-class multi-tenant profiles with per-profile rate limits and encrypted credential storage.

### When to use it

- You have multiple LLM providers and want automatic failover without wiring it into every agent.
- You want a single OpenAI-compatible endpoint whose backends can change without touching client code.
- You run multiple agents or teams and want per-agent API tokens, rate limits, and routing rules.
- You want provider API keys stored encrypted rather than in plaintext config files.

### When not to use it

- You have a single provider and no failover need -- call it directly.
- You need multi-node clustering -- AGOS Proxy is single-process, single-machine today.
- You need a fully managed service -- AGOS Proxy is self-hosted.

---

## The Model You Call

Everything in AGOS Proxy is organized around one idea: a caller never picks a provider or a model directly. The caller picks a **route**, and the route owns the chain.

```
Profile ("coder1")                     # your tenant; its id is your API token
├── Providers                          # how we reach the outside world
│     ├── openai        https://api.openai.com/v1
│     ├── deepseek      https://api.deepseek.com
│     └── openrouter    https://openrouter.ai/api/v1
└── Proxy: "Programmer"               # a namespace for routes
      └── Route: "php-dev"            # what callers actually request
            1. openai/gpt-4o          (priority 1 -- tried first)
            2. deepseek/deepseek-chat (priority 2 -- fallback)
            3. openrouter/google/gemini-2.0-flash-001 (priority 3)
```

A caller requests `Programmer/php-dev`. AGOS Proxy resolves the route, picks the highest-priority healthy entry, forwards the request, and if that attempt fails, tries the next entry. The caller does not know or care which provider answered.

### Routing strategies

| Strategy | Behavior |
|----------|----------|
| `priority` | Try entries in stored order (1, 2, 3...). Default. |
| `round_robin` | Distribute across healthy entries in rotation. |
| `weighted` | Like round-robin, but biased by each entry's weight. |

---

## Identity Masking

A route can carry an optional **identity description**. When set, AGOS Proxy prepends a system message to every request telling the model to adopt that identity and never reveal its underlying provider, model name, or developer. When the identity is not set, the model behaves normally.

```sh
./target/release/agos-proxy route create --proxy Programmer
# Identity description (optional -- hides the real model from users): A senior Python engineer named Maya
```

Or include it in a [bootstrap JSON](#bootstrap-json) document. The field is optional. Omit it and the model responds as itself.

---

## A--Z Walkthrough

### Step 1: Install

```sh
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
cargo build --release
sudo cp target/release/agos-proxy /usr/local/bin/

# Or install via cargo
cargo install --path .

# Or Docker
docker compose up -d
```

Verify: `agos-proxy --version`

### Step 2: Create a Profile

A profile is a tenant -- it owns providers, proxies, routes, and has its own API token (which is also its opaque `id`).

```sh
agos-proxy profile create --name coder1
```

The CLI prints the token once. Save it. You will send it as `Authorization: Bearer <token>` on every API call. If you lose it, rotate it with `agos-proxy profile token rotate coder1` -- the old token stops working immediately.

To protect a profile from interactive changes, create it with a password:

```sh
agos-proxy profile create        # interactive -- prompted for password when you say yes
```

Password-protected profiles prompt for the password on every mutating CLI operation, up to 3 attempts.

### Step 3: Add Providers

```sh
agos-proxy provider add --profile coder1
```

The wizard prompts for: name, base URL, API token (stored encrypted), provider kind, and optional extra headers.

Common providers:

| Provider | Name | Base URL |
|----------|------|----------|
| OpenAI | openai | https://api.openai.com/v1 |
| DeepSeek | deepseek | https://api.deepseek.com |
| OpenRouter | openrouter | https://openrouter.ai/api/v1 |
| Anthropic (native) | claude | https://api.anthropic.com |
| Google Gemini (native) | google | https://generativelanguage.googleapis.com/v1beta |
| Local / self-hosted | ollama | http://localhost:11434/v1 |

Provider tokens are encrypted with ChaCha20-Poly1305 before they reach disk.

### Step 4: Create a Proxy

```sh
agos-proxy proxy create --profile coder1
# name: Programmer
```

A proxy is a namespace for routes -- think of it as an API group label.

### Step 5: Create a Route

```sh
agos-proxy route create --proxy Programmer
```

The wizard prompts for the route name, routing strategy, optional identity, and then walks you through adding model entries one by one.

Example chain:

```
1. openai/gpt-4o                        (priority 1 -- first try)
2. deepseek/deepseek-chat               (priority 2 -- fallback)
3. openrouter/google/gemini-2.0-flash  (priority 3 -- last resort)
```

Non-interactive:

```sh
agos-proxy route model add --proxy Programmer --route php-dev \
  --provider openai --model gpt-4o --priority 1
```

### Step 6: Start the Server

```sh
agos-proxy serve --bind 127.0.0.1:3000
```

On startup the proxy opens the SQLite store (applying any pending migrations), starts the background health-probe loop (pings unhealthy entries every 30 seconds), and begins listening.

For long-running service, wrap it in systemd (see [Deployment](docs/deployment.md)) or run it behind a reverse proxy that terminates TLS. The proxy itself does not terminate TLS.

### Step 7: Make a Request

**curl:**

```sh
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer <YOUR_TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"model":"Programmer/php-dev","messages":[{"role":"user","content":"Write a quicksort in Python."}]}'
```

**Python:**

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:3000/v1",
    api_key="<YOUR_TOKEN>",
)

response = client.chat.completions.create(
    model="Programmer/php-dev",
    messages=[{"role": "user", "content": "Write a quicksort in Python."}],
)
print(response.choices[0].message.content)
```

**Node.js:**

```js
import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "http://localhost:3000/v1",
  apiKey: "<YOUR_TOKEN>",
});

const response = await client.chat.completions.create({
  model: "Programmer/php-dev",
  messages: [{ role: "user", content: "Write a quicksort in Python." }],
});
console.log(response.choices[0].message.content);
```

**Streaming:** Add `"stream": true` to the request body. The response is an SSE stream. Note: if the upstream fails mid-stream, the stream breaks -- that is a fundamental limitation of streaming failover. Use non-streaming when transparent failover matters.

**List available models:**

```sh
curl http://localhost:3000/v1/models \
  -H "Authorization: Bearer <YOUR_TOKEN>"
```

Returns the routes your profile can reach, in `<proxy>/<route>` form.

### Step 8: Monitor Usage

```sh
agos-proxy usage stats --profile coder1
agos-proxy usage recent --profile coder1 --limit 20
agos-proxy route status --route php-dev
```

### Step 9: Test Interactively

```sh
agos-proxy chat          # line-based REPL
agos-proxy chat --tui    # full-screen TUI
```

### Step 10: Export / Import Config

```sh
agos-proxy config export --profile coder1 > coder1-backup.json
agos-proxy config import --file coder1-backup.json
```

Never copy the raw SQLite file between machines with different master keys. Use export/import instead.

---

## The Guided Setup Wizard

For first-time users, one command walks through the whole sequence:

```sh
agos-proxy setup
```

It prompts for a profile, then into a menu where you can add providers, create proxies, build routes, and set rate limits -- all in one session. When you exit the menu, it prints the profile token.

---

## Bootstrap JSON

For scripted or container environments, AGOS Proxy can be seeded from a single JSON document:

```sh
agos-proxy bootstrap from-file setup.json
# prints the generated profile token to stdout
```

Or from stdin:

```sh
cat setup.json | agos-proxy bootstrap from-file -
```

This is the intended way to give an agent or a provisioning script its own self-contained setup.

### Example bootstrap document

```json
{
  "profile": "agent-1",
  "description": "autonomous coding agent",
  "providers": [
    {
      "name": "primary",
      "base_url": "https://api.openai.com/v1",
      "auth_token": "sk-...",
      "kind": "openai_compatible"
    },
    {
      "name": "fallback",
      "base_url": "https://openrouter.ai/api/v1",
      "auth_token": "sk-or-v1-...",
      "kind": "openai_compatible"
    }
  ],
  "proxies": [
    {
      "name": "main",
      "routes": [
        {
          "name": "coder",
          "description": "code generation and review",
          "identity": "A senior Python engineer named Maya",
          "strategy": "priority",
          "models": [
            { "provider": "primary", "model": "gpt-4o", "priority": 1 },
            { "provider": "fallback", "model": "anthropic/claude-3.5-sonnet", "priority": 2 }
          ]
        },
        {
          "name": "chat",
          "description": "cheap, fast chat",
          "strategy": "priority",
          "models": [
            { "provider": "fallback", "model": "google/gemini-2.0-flash-001", "priority": 1 }
          ]
        }
      ]
    }
  ]
}
```

The bootstrap document is the same format that Docker's `AGOS_SETUP` environment variable accepts.

---

## API Surface

AGOS Proxy speaks an OpenAI-compatible API at `/v1/...`.

| Endpoint | Description |
|----------|-------------|
| `POST /v1/chat/completions` | Chat completions (streaming and non-streaming) |
| `POST /v1/completions` | Legacy text completions |
| `POST /v1/embeddings` | Embedding vectors |
| `GET /v1/models` | Lists the routes your profile can call |

Full reference: [API Reference](docs/api-reference.md)

Provider notes: [Providers](docs/providers.md)
Failover details: [Failover](docs/failover.md)

---

## Configuration

- [Configuration Reference](docs/configuration.md) -- data directory, environment variables, bootstrap schema, secrets
- [Environment Variables](docs/reference/environment.md) -- complete env var reference

Key points:

- **Data directory:** `~/.config/agos-proxy` by default. Override with `AGOS_HOME=/path`.
- **No config file to edit.** Everything is administered through the CLI, which writes to the SQLite store.
- **Secrets:** provider tokens are encrypted at rest with ChaCha20-Poly1305; the master key lives only in the store's `meta` table and in memory.

---

## Deployment

- [Deployment Guide](docs/deployment.md) -- binary install, Docker, systemd, reverse proxy, backup/restore, health checking

Highlights:

- Bind to `127.0.0.1` for local development (the default). Put it behind a TLS-terminating reverse proxy when exposing it on the network.
- Back up the whole data directory, or use `config export`/`config import` for portable, cross-key backup.
- A restart preserves profiles, providers, routes, and usage history. It resets in-memory health state and rate-limit windows.

---

## Security

- [Security Model](docs/security.md) -- threat model, encryption, password hashing, token handling, operational guidance

In brief: the host is trusted; provider tokens are encrypted at rest; profile passwords gate interactive management; bearer tokens are sensitive and should be treated like API keys; the proxy does not terminate TLS.

---

## Architecture

- [Architecture](docs/architecture.md) -- mental model, crate layout, data model, request lifecycle, health subsystem, routing strategies

---

## Monitoring & Operations

- [Failover](docs/failover.md) -- health states, probe loop, status transitions, operational guidance
- [Troubleshooting](docs/help/troubleshooting.md) -- common issues and fixes
- [FAQ](docs/help/faq.md) -- common questions

Key operational tools:

```sh
agos-proxy route status --route <name>     # which entries are healthy
agos-proxy usage recent --profile <name>    # what happened to recent requests
agos-proxy usage stats --profile <name>     # per-model aggregates
RUST_LOG=debug agos-proxy serve            # verbose server logs
```

---

## Reference

| Document | Purpose |
|----------|---------|
| [API Reference](docs/api-reference.md) | HTTP endpoints, auth, errors, streaming |
| [CLI Reference](docs/CLI.md) | Auto-generated command help |
| [Configuration](docs/configuration.md) | Data dir, env vars, bootstrap schema, secrets |
| [Environment Variables](docs/reference/environment.md) | Complete env var reference |
| [Providers](docs/providers.md) | Provider kinds, setup, auth styles |
| [Failover](docs/failover.md) | Health states, probe loop, streaming limitation |
| [Security](docs/security.md) | Threat model, encryption, operational guidance |
| [Architecture](docs/architecture.md) | Design, crate layout, data model, request lifecycle |
| [Deployment](docs/deployment.md) | Binary, Docker, systemd, reverse proxy, backup |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow, PR expectations, and the release process. Keep changes small and focused. Update docs when behavior changes.

---

## License

MIT -- see [LICENSE](LICENSE).
