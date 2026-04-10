# AGOS Proxy — Quick Reference

> One-page CLI cheatsheet. For full docs, see [README.md](README.md) and [docs/](docs/).

---

## Installation

```sh
cargo build --release                    # build from source
sudo cp target/release/agos-proxy /usr/local/bin/
cargo install --path .                   # install to ~/.cargo/bin
docker compose up -d                     # run with Docker
```

## Data

| | |
|---|---|
| **Default store** | `~/.config/agos-proxy/agos.db` |
| **Override** | `AGOS_HOME=/path/to/dir` |
| **Logs** | `RUST_LOG=info` (levels: error, warn, info, debug, trace) |

---

## Server

```sh
agos-proxy serve                         # bind 127.0.0.1:3000
agos-proxy serve --bind 0.0.0.0:8080     # custom bind
```

## Profile

```sh
agos-proxy profile create                # interactive
agos-proxy profile create --name coder1  # non-interactive
agos-proxy profile list
agos-proxy profile show coder1
agos-proxy profile edit --name coder1
agos-proxy profile delete --name coder1
agos-proxy profile token rotate coder1   # rotate API token
agos-proxy profile limit coder1 120      # set RPM limit (0 = unlimited)
```

## Provider

```sh
agos-proxy provider add --profile coder1     # interactive wizard
agos-proxy provider list --profile coder1
agos-proxy provider edit --profile coder1
agos-proxy provider delete --profile coder1
```

**Provider kinds:** `openai_compatible`, `anthropic`, `google`, `custom`

## Proxy

```sh
agos-proxy proxy create --profile coder1     # interactive
agos-proxy proxy list --profile coder1
agos-proxy proxy edit --profile coder1
agos-proxy proxy delete --profile coder1
```

## Route

```sh
agos-proxy route create --proxy Programmer      # interactive chain builder
agos-proxy route status --route php-dev         # show health
agos-proxy route edit --proxy Programmer
agos-proxy route delete --proxy Programmer

# Model chain management
agos-proxy route model add --proxy Programmer
agos-proxy route model remove --proxy Programmer
agos-proxy route model move --proxy Programmer
```

**Strategies:** `priority`, `round_robin`, `weighted`

## Usage & Monitoring

```sh
agos-proxy usage stats --profile coder1          # per-model aggregates
agos-proxy usage recent --profile coder1 -l 20   # recent requests
agos-proxy route status --route php-dev          # route health
```

## Interactive Testing

```sh
agos-proxy chat                  # line-mode REPL
agos-proxy chat --tui            # full-screen TUI
```

## Guided Setup

```sh
agos-proxy setup                 # all-in-one wizard: profile → providers → proxies → routes
```

## Config Export/Import

```sh
agos-proxy config export --profile coder1 --output backup.json
agos-proxy config import --path backup.json
```

## Bootstrap (CI / Containers)

```sh
agos-proxy bootstrap from-file setup.json
cat setup.json | agos-proxy bootstrap from-file -
```

**Bootstrap JSON schema:**
```json
{
  "profile": "name",
  "description": "optional",
  "providers": [
    {
      "name": "deepseek",
      "base_url": "https://api.deepseek.com",
      "auth_token": "sk-...",
      "kind": "openai_compatible"
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
            { "provider": "deepseek", "model": "deepseek-chat", "priority": 1 }
          ]
        }
      ]
    }
  ]
}
```

---

## API Endpoints

| Endpoint | Methods | Auth |
|----------|---------|------|
| `GET /health` | health check | none |
| `GET /v1/models` | list routes | bearer |
| `POST /v1/chat/completions` | chat | bearer |
| `POST /v1/completions` | text completion | bearer |
| `POST /v1/embeddings` | embeddings | bearer |

## Quick API Call

```sh
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model":"Programmer/chat","messages":[{"role":"user","content":"hi"}]}'
```

## Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `AGOS_HOME` | Data directory | `~/.config/agos-proxy` |
| `RUST_LOG` | Log level | `info` |
| `AGOS_SETUP` | Bootstrap JSON (Docker) | — |

---

## File Locations

| File | Purpose |
|------|---------|
| `~/.config/agos-proxy/agos.db` | SQLite store |
| `~/.config/agos-proxy/` | Data directory |
| `target/release/agos-proxy` | Binary (after build) |

---

## Common Workflows

**First-time setup:**
```sh
agos-proxy setup
agos-proxy serve
```

**Add a provider to existing route:**
```sh
agos-proxy provider add --profile coder1
agos-proxy route model add --proxy Programmer
```

**Rotate compromised token:**
```sh
agos-proxy profile token rotate coder1
```

**Backup:**
```sh
agos-proxy config export --profile coder1 --output backup.json
```

**Docker test:**
```sh
make docker-up
make docker-down
```
