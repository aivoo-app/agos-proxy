# Common Configuration Patterns

> Real-world examples of how to configure AGOS Proxy for common use cases.

## Table of Contents

- [Multi-Provider Failover Chain](#multi-provider-failover-chain)
- [Per-Team Profiles](#per-team-profiles)
- [Weighted Routing for Cost Optimization](#weighted-routing-for-cost-optimization)
- [Round-Robin Load Balancing](#round-robin-load-balancing)
- [Multiple Models per Use Case](#multiple-models-per-use-case)
- [Using OpenRouter as a Universal Fallback](#using-openrouter-as-a-universal-fallback)
- [Local Models for Development](#local-models-for-development)
- [Docker Compose for CI](#docker-compose-for-ci)
- [Production with systemd](#production-with-systemd)

---

## Multi-Provider Failover Chain

**Goal:** Build a chain that tries your preferred provider first, then falls back across multiple providers.

### CLI Setup

```sh
# Create profile
agos-proxy profile create --name production

# Add three providers
agos-proxy provider add --profile production
# name: openai, base_url: https://api.openai.com/v1, kind: generic

agos-proxy provider add --profile production
# name: deepseek, base_url: https://api.deepseek.com, kind: generic

agos-proxy provider add --profile production
# name: openrouter, base_url: https://openrouter.ai/api/v1, kind: generic

# Create proxy + route
agos-proxy proxy create --profile production
# name: chat

agos-proxy route create --proxy chat
# route name: smart-chat
# strategy: priority

# Chain: GPT-4o → DeepSeek → OpenRouter
# entry 1: openai/gpt-4o, priority 1
# entry 2: deepseek/deepseek-chat, priority 2
# entry 3: openrouter/google/gemini-2.0-flash-001, priority 3
```

### Bootstrap Setup (JSON)

```json
{
  "profile": "production",
  "providers": [
    {
      "name": "openai",
      "base_url": "https://api.openai.com/v1",
      "auth_token": "sk-...",
      "kind": "generic"
    },
    {
      "name": "deepseek",
      "base_url": "https://api.deepseek.com",
      "auth_token": "sk-...",
      "kind": "generic"
    },
    {
      "name": "openrouter",
      "base_url": "https://openrouter.ai/api/v1",
      "auth_token": "sk-or-v1-...",
      "kind": "generic"
    }
  ],
  "proxies": [
    {
      "name": "chat",
      "routes": [
        {
          "name": "smart-chat",
          "strategy": "priority",
          "models": [
            { "provider": "openai", "model": "gpt-4o", "priority": 1 },
            { "provider": "deepseek", "model": "deepseek-chat", "priority": 2 },
            { "provider": "openrouter", "model": "google/gemini-2.0-flash-001", "priority": 3 }
          ]
        }
      ]
    }
  ]
}
```

---

## Per-Team Profiles

**Goal:** Give each team its own API token, rate limits, and routing rules.

```sh
# Team Alpha — budget-conscious
agos-proxy profile create --name team-alpha
agos-proxy profile limit team-alpha 60    # 60 req/min

# Team Beta — high-throughput
agos-proxy profile create --name team-beta
agos-proxy profile limit team-beta 300   # 300 req/min

# Team Gamma — internal only, no rate limit
agos-proxy profile create --name team-gamma
# rpm_limit 0 = unlimited
```

Each team calls the same proxy endpoint but uses their own token. Rate limits are enforced per-profile.

```sh
# Team Alpha request
curl -H "Bearer: TEAM_ALPHA_TOKEN" http://localhost:3000/v1/chat/completions -d '...'

# Team Beta request
curl -H "Bearer: TEAM_BETA_TOKEN" http://localhost:8080/v1/chat/completions -d '...'
```

---

## Weighted Routing for Cost Optimization

**Goal:** Send 80% of traffic to a cheap model and 20% to an expensive one.

```sh
agos-proxy route create --proxy chat
# route name: weighted-chat
# strategy: weighted
# entry 1: deepseek/deepseek-chat, weight 0.8
# entry 2: openai/gpt-4o, weight 0.2
```

The weighted strategy picks a starting entry probabilistically (80% chance of deepseek, 20% chance of OpenAI), then falls back through the rest on failure.

### Why this matters

If deepseek is 10x cheaper than OpenAI, weighted routing gives you 80% cost savings while still having GPT-4o as a quality backstop for requests where DeepSeek fails.

---

## Round-Robin Load Balancing

**Goal:** Spread traffic evenly across multiple healthy providers.

```sh
agos-proxy route create --proxy chat
# route name: balanced-chat
# strategy: round_robin
# entry 1: openai/gpt-4o-mini
# entry 2: deepseek/deepseek-chat
# entry 3: openrouter/google/gemini-2.0-flash-001
```

With round-robin, requests cycle through entries 1 → 2 → 3 → 1 → ... Unhealthy entries are skipped.

---

## Multiple Models per Use Case

**Goal:** Expose different routes for different tasks, all under one proxy.

```sh
# Code generation — use a strong coding model
agos-proxy route create --proxy Programmer
# route name: code-gen, priority chain: [deepseek-coder, gpt-4o, claude-sonnet]

# Code review — use a model with large context
agos-proxy route create --proxy Programmer
# route name: review, priority chain: [gpt-4o, gemini-pro]

# Chat — cheap and fast
agos-proxy route create --proxy Programmer
# route name: chat, priority chain: [deepseek-chat, gpt-4o-mini]
```

Callers pick the route for their use case:

```python
# Code generation
client.chat.completions.create(model="Programmer/code-gen", messages=[...])

# Code review
client.chat.completions.create(model="Programmer/review", messages=[...])

# Chat
client.chat.completions.create(model="Programmer/chat", messages=[...])
```

---

## Using OpenRouter as a Universal Fallback

**Goal:** Use OpenRouter as the last-resort fallback since it aggregates hundreds of models.

```json
{
  "profile": "my-agent",
  "providers": [
    {
      "name": "primary",
      "base_url": "https://api.openai.com/v1",
      "auth_token": "sk-...",
      "kind": "generic"
    },
    {
      "name": "fallback",
      "base_url": "https://openrouter.ai/api/v1",
      "auth_token": "sk-or-v1-...",
      "kind": "generic"
    }
  ],
  "proxies": [
    {
      "name": "main",
      "routes": [
        {
          "name": "chat",
          "strategy": "priority",
          "models": [
            { "provider": "primary", "model": "gpt-4o", "priority": 1 },
            { "provider": "fallback", "model": "anthropic/claude-3.5-sonnet", "priority": 2 },
            { "provider": "fallback", "model": "google/gemini-2.0-flash-001", "priority": 3 }
          ]
        }
      ]
    }
  ]
}
```

---

## Local Models for Development

**Goal:** Use Ollama or LM Studio locally without real API keys.

```sh
# Add Ollama as a provider
agos-proxy provider add --profile dev
# name: ollama
# base_url: http://localhost:11434/v1
# auth_token: (leave empty — Ollama doesn't require auth)
# kind: generic
```

Ollama speaks an OpenAI-compatible API, so no translation is needed. Common Ollama model IDs: `llama3.2`, `codellama`, `mistral`, `qwen2.5`.

---

## Docker Compose for CI

**Goal:** Run AGOS Proxy in CI with a mock upstream for integration testing.

The repository includes a `docker-compose.yml` with a mock upstream:

```sh
# Start the stack
make docker-up

# Run a smoke request
curl -H "Authorization: Bearer sk-mock" \
  http://localhost:8080/v1/chat/completions \
  -d '{"model":"main/chat","messages":[{"role":"user","content":"hi"}]}'

# Tear down
make docker-down
```

The mock upstream (`docker/mock.py`) speaks a minimal OpenAI-compatible API and doesn't require real credentials.

---

## Production with systemd

**Goal:** Run AGOS Proxy as a long-running service with auto-restart.

Create `/etc/systemd/system/agos-proxy.service`:

```ini
[Unit]
Description=AGOS Proxy — AI Gateway
After=network.target

[Service]
Type=exec
User=agos
Group=agos
WorkingDirectory=/var/lib/agos-proxy
Environment=AGOS_HOME=/var/lib/agos-proxy
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/agos-proxy serve --bind 0.0.0.0:8080
Restart=on-failure
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --create-home --home-dir /var/lib/agos-proxy agos
sudo systemctl daemon-reload
sudo systemctl enable --now agos-proxy
sudo systemctl status agos-proxy

# View logs
journalctl -u agos-proxy -f
```

> **Security:** AGOS Proxy does not terminate TLS. Put it behind a reverse proxy (nginx, Caddy, Envoy) when exposing it on the network. See [docs/deployment.md](../deployment.md) for details.
