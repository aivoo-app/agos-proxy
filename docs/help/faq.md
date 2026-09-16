# Frequently Asked Questions

> Common questions about AGOS Proxy — installation, configuration, troubleshooting, and more.

## Table of Contents

- [General](#general)
- [Installation](#installation)
- [Configuration](#configuration)
- [Providers](#providers)
- [Routing & Failover](#routing--failover)
- [Security](#security)
- [Performance](#performance)
- [Operations](#operations)
- [SDK Integration](#sdk-integration)
- [Troubleshooting](#troubleshooting)

---

## General

### What does AGOS Proxy do?

AGOS Proxy sits between your AI agent and its LLM providers. Instead of your agent calling DeepSeek or Anthropic directly, it calls AGOS Proxy, which routes the request through a chain of providers with automatic failover. If the first provider is down, AGOS Proxy tries the next one — all within a single request.

### Is this a replacement for OpenAI's API?

No. AGOS Proxy is a **proxy** — it forwards requests to real LLM providers (OpenAI, DeepSeek, Anthropic, Google, OpenRouter, etc.) and returns their responses. You still need API keys from real providers.

### What's the difference between AGOS Proxy and LiteLLM?

Both provide multi-provider failover, but AGOS Proxy is:
- **Self-hosted, single-binary** — no Python environment, no pip install
- **CLI-first** — configured entirely through an interactive CLI, not a config file
- **Multi-tenant** — profile-based isolation with per-profile rate limits and tokens
- **Encrypted at rest** — provider keys stored encrypted, not in plaintext config

### What Rust version do you need?

Rust 1.75 or later. The project tracks stable Rust.

### What platforms are supported?

Linux, macOS, and Windows. The CI tests on Linux (x86_64, aarch64) and macOS (x86_64, aarch64). Windows builds are not CI-tested but should work.

---

## Installation

### How do I install AGOS Proxy?

Three ways:

```sh
# 1. Build from source
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
cargo build --release
sudo cp target/release/agos-proxy /usr/local/bin/

# 2. Install via cargo
cargo install --path .

# 3. Use Docker
docker compose up -d
```

### Can I use `cargo install` from crates.io?

Not yet. The crate is not published to crates.io. Use `cargo install --path .` from a local clone.

### What are the system requirements?

- A modern Linux/macOS/Windows system
- ~50 MB disk space for the release binary
- No runtime dependencies (all deps are statically linked or bundled)

---

## Configuration

### Where is my data stored?

Default: `~/.config/agos-proxy/agos.db`. Override with `AGOS_HOME=/path/to/dir`.

### Can I run multiple instances?

Yes. Each instance needs its own `AGOS_HOME` directory:

```sh
AGOS_HOME=/data/instance1 agos-proxy serve --bind 127.0.0.1:3000
AGOS_HOME=/data/instance2 agos-proxy serve --bind 127.0.0.1:3001
```

### Do I need to edit a config file?

No. AGOS Proxy is configured entirely through the CLI. There is no config file to edit.

### How do I migrate my setup to another machine?

```sh
# On the source machine
agos-proxy config export --profile coder1 --output backup.json

# On the target machine
agos-proxy config import --path backup.json
```

The exported file is passphrase-sealed. On import, provider tokens are re-encrypted with the destination store's master key.

---

## Providers

### Which providers are supported?

Any provider that speaks one of these protocols:
- **OpenAI-format** — DeepSeek, OpenRouter, Together AI, Ollama, local servers
- **Anthropic** — Claude API (native translation)
- **Google** — Gemini API (native translation)

### Can I add a custom provider?

If it speaks an OpenAI-format API, yes — use kind `generic`. If it speaks a completely different protocol, you'd need to add a translator module (see [docs/providers.md](docs/providers.md)).

### How do I set up OpenRouter?

```sh
agos-proxy provider add --profile myprofile
# name: openrouter
# base_url: https://openrouter.ai/api/v1
# auth_token: sk-or-v1-...
# kind: generic
```

Then in a route entry: `{ "provider": "openrouter", "model": "openai/gpt-4o" }`.

### How do I set up a local model (Ollama)?

```sh
agos-proxy provider add --profile dev
# name: ollama
# base_url: http://localhost:11434/v1
# auth_token: (leave empty)
# kind: generic
```

---

## Routing & Failover

### What's a route?

A route is an addressable fallback chain. It's exposed to callers as `<proxy>/<route>` (e.g., `Programmer/code-gen`). Behind the scenes, it's an ordered list of models from different providers.

### What's the difference between priority and round-robin?

| Strategy | Behavior |
|----------|----------|
| `priority` | Try entries in strict order. Entry 1 first, then 2 on failure, etc. |
| `round_robin` | Distribute requests across all healthy entries. |
| `weighted` | Like round-robin, but biased by each entry's weight. |

### How does failover work?

When a request fails on one entry (connection error, timeout, 5xx, 429), AGOS Proxy marks it `Degraded` and tries the next entry. This happens within a single caller request — the caller sees one response. A background probe loop wakes every 30 seconds to check unhealthy entries and recover them.

### What happens if all providers fail?

The request returns a `503 Service Unavailable` with an error message. Check `agos-proxy usage recent` to see what went wrong.

### Can I see the health status of my route?

```sh
agos-proxy route status --route code-gen
```

Shows each entry's status: `healthy`, `degraded`, `unhealthy`, or `disabled`.

---

## Security

### Are provider tokens encrypted?

Yes. Provider `auth_token` values are encrypted with ChaCha20-Poly1305 before they reach disk. The master key is randomly generated on first store creation and lives only in memory while the store is open.

### What if someone steals my SQLite file?

They can't decrypt the tokens without the master key. The master key is stored in the `meta` table and is itself derived from store-level state. However, if an attacker has access to the running process memory, they can extract the master key — so protect the host.

### How do I protect a profile with a password?

```sh
agos-proxy profile create --name sensitive-profile
# When prompted: "Protect this profile with a password?" → Yes
```

A password hash (Argon2id) is stored with the profile. Every mutating operation prompts for the password (up to 3 attempts).

### Does AGOS Proxy support TLS?

Not built-in. AGOS Proxy does not terminate TLS. Put it behind a TLS-terminating reverse proxy (nginx, Caddy, Envoy) when exposing it on the network.

---

## Performance

### How much latency does AGOS Proxy add?

Minimal for the happy path — typically < 1ms overhead for auth, rate limiting, and routing. The actual latency is dominated by the upstream provider. Failover adds the per-attempt timeout (default 10s) for each failed entry.

### What's the throughput?

Depends on the upstream providers. AGOS Proxy itself is async (tokio + axum) and can handle thousands of concurrent connections. The bottleneck is almost always the upstream LLM provider.

### Can I run it behind a load balancer?

AGOS Proxy is single-process today. For horizontal scaling, run multiple instances behind a load balancer, each with its own `AGOS_HOME`. Rate limits and usage logs are per-instance.

---

## Operations

### How do I back up my data?

```sh
# Option 1: Copy the data directory
cp -a "$AGOS_HOME" /backup/agos-proxy-$(date +%Y%m%d)

# Option 2: Export sealed config (per-profile)
agos-proxy config export --profile coder1 --output backup.json
```

Option 2 is portable across machines and master keys.

### How do I upgrade?

1. Back up your data directory.
2. Replace the binary.
3. Restart — migrations run automatically on startup.

### How do I rotate a compromised token?

```sh
# Profile bearer token
agos-proxy profile token rotate coder1

# Provider token
agos-proxy provider edit --profile coder1
```

### How do I monitor usage?

```sh
# Per-model stats
agos-proxy usage stats --profile coder1

# Recent requests
agos-proxy usage recent --profile coder1 --limit 20
```

---

## SDK Integration

### Can I use the OpenAI Python SDK?

Yes. Just change `base_url` and `api_key`:

```python
client = OpenAI(base_url="http://localhost:3000/v1", api_key="YOUR_PROFILE_TOKEN")
```

### Can I use the OpenAI Node.js SDK?

Yes:

```js
const client = new OpenAI({ baseURL: 'http://localhost:3000/v1', apiKey: 'TOKEN' });
```

### Can I use LangChain?

Yes. Any framework that accepts a custom `base_url` works:

```python
from langchain_openai import ChatOpenAI

llm = ChatOpenAI(
    base_url="http://localhost:3000/v1",
    api_key="YOUR_PROFILE_TOKEN",
    model="Programmer/code-gen",
)
```

---

## Troubleshooting

### "No profiles configured"

You need to create a profile first: `agos-proxy profile create --name myprofile`.

### "No providers configured"

Add a provider: `agos-proxy provider add --profile myprofile`.

### "Route not found"

Check your route exists: `agos-proxy route status`. Ensure you're using the `<proxy>/<route>` format.

### "All providers failed"

Check `agos-proxy usage recent --profile <name>` to see which providers failed and why. Common causes: expired API key, rate limit, provider outage.

### "Rate limit exceeded"

The profile has hit its RPM limit. Increase it: `agos-proxy profile limit <name> <rpm>`, or add exponential backoff to your client.

### "Decryption failed"

The master key doesn't match the encrypted tokens. This can happen if you copy the SQLite file to a different store directory. Use `config export`/`config import` for portable migration instead.
