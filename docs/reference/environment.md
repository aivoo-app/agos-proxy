# Environment Variable Reference

> Complete reference for all environment variables used by AGOS Proxy.

## Table of Contents

- [Core Variables](#core-variables)
- [Logging](#logging)
- [Docker / Bootstrap](#docker--bootstrap)
- [Platform-Specific Defaults](#platform-specific-defaults)
- [Examples](#examples)

---

## Core Variables

### `AGOS_HOME`

| | |
|---|---|
| **Purpose** | Override the data directory location |
| **Default** | `$XDG_CONFIG_HOME/agos-proxy` (typically `~/.config/agos-proxy`) |
| **Used by** | All CLI commands and `serve` |
| **Example** | `AGOS_HOME=/tmp/agos-test agos-proxy profile list` |

The data directory contains:
- `agos.db` — the SQLite store (profiles, providers, routes, usage log)
- (master key is stored inside the `meta` table, not as a separate file)

Setting `AGOS_HOME` is useful for:
- **Testing:** use an isolated store without affecting production
- **Multiple instances:** run multiple AGOS Proxy instances with separate stores
- **Containers:** mount a volume at a known path

---

## Logging

### `RUST_LOG`

| | |
|---|---|
| **Purpose** | Tracing filter for the `serve` command |
| **Default** | `info` |
| **Used by** | `agos-proxy serve` only |
| **Backend** | `tracing-subscriber` with `EnvFilter` |

Common values:

| Value | Effect |
|-------|--------|
| `off` | Silence all logging |
| `error` | Only errors |
| `warn` | Warnings and errors |
| `info` | Normal operational messages (default) |
| `debug` | Detailed debug output |
| `trace` | Very verbose — includes request/response details |
| `agos=debug` | Debug output only from the agos crate |
::content_factor
| `agos::router=trace` | Trace-level output from the router module only |

Examples:

```sh
# Debug logging
RUST_LOG=debug agos-proxy serve

# Trace only the router
RUST_LOG=agos::router=trace agos-proxy serve

# Quiet mode
RUST_LOG=off agos-proxy serve
```

---

### `AGOS_ATTEMPT_TIMEOUT_SECS`

| | |
|---|---|
| **Purpose** | Per-attempt failover timeout: how long one model may take before the router moves to the next entry in the chain |
| **Default** | `10` seconds |
| **Used by** | `agos-proxy serve` only |
| **Example** | `AGOS_ATTEMPT_TIMEOUT_SECS=30 agos-proxy serve` |

The `--attempt-timeout` command-line flag takes precedence over this variable,
which in turn takes precedence over the built-in default. Raise it if your
chain contains slow reasoning models that legitimately take longer than 10
seconds to produce their first token.

---

### `AGOS_STREAM_IDLE_TIMEOUT_SECS`

| | |
|---|---|
| **Purpose** | Idle timeout for committed streams: how long the upstream may stay silent between SSE body chunks before the stream is failed instead of hanging the client |
| **Default** | `60` seconds |
| **Used by** | `agos-proxy serve` only |
| **Example** | `AGOS_STREAM_IDLE_TIMEOUT_SECS=120 agos-proxy serve` |

`--attempt-timeout` only bounds the time to receive response *headers*. Once a
stream is committed (HTTP 200 with `Content-Type: text/event-stream`), each
subsequent chunk must arrive within this window; otherwise the proxy emits an
SSE error event, closes the stream, logs the attempt as failed, and demotes the
route entry. Set it to `0` to disable the idle timeout entirely (not
recommended — a stalled upstream will then hang the client indefinitely).

The `--stream-idle-timeout` command-line flag takes precedence over this
variable, which in turn takes precedence over the built-in default.

---

### `AGOS_RATE_LIMIT_COOLDOWN_SECS`

| | |
|---|---|
| **Purpose** | How long a route entry is skipped after the upstream answers `429 Too Many Requests`, when the upstream does not send a usable `Retry-After` header |
| **Default** | `60` seconds |
| **Used by** | `agos-proxy serve` only |
| **Example** | `AGOS_RATE_LIMIT_COOLDOWN_SECS=120 agos-proxy serve` |

When the router's failover loop demotes an entry for a 429, the entry gets a
`cooldown_until` timestamp. Resolution skips cooling entries (falling back to
the soonest-expiring one if everything is cooling, so a request never
hard-fails). If the upstream honors `Retry-After` or exposes a rate-limit
reset header, that value wins over this constant.

Cooldowns are deliberately independent of `ModelStatus`: a health probe may
report a key as `Healthy` again, but `cooldown_until` still blocks it until
the window expires — a green ping cannot cancel an upstream rate-limit window.

---

## Docker / Bootstrap

### `AGOS_SETUP`

| | |
|---|---|
| **Purpose** | JSON document to seed the store on first run |
| **Default** | (none) |
| **Used by** | Docker entrypoint (`docker/entrypoint.sh`) |
| **Format** | Same as `bootstrap from-file` JSON schema |

This is only used in Docker/CI environments. The entrypoint writes the document to `$AGOS_HOME/setup.json` and runs `agos-proxy bootstrap from-file` if the store is empty.

Example in `docker-compose.yml`:

```yaml
environment:
  AGOS_SETUP: |
    {
      "profile": "ci-agent",
      "providers": [
        {"name": "mock", "base_url": "http://mock:9999", "auth_token": "sk-mock", "kind": "openai"}
      ],
      "proxies": [
        {"name": "main", "routes": [
          {"name": "chat", "models": [
            {"provider": "mock", "model": "mock-model"}
          ]}
        ]}
      ]
    }
```

---

## Platform-Specific Defaults

### Linux

```
AGOS_HOME = $XDG_CONFIG_HOME/agos-proxy  (if XDG_CONFIG_HOME is set)
           $HOME/.config/agos-proxy        (otherwise)
```

### macOS

```
AGOS_HOME = $HOME/Library/Application Support/agos-proxy
```

> Note: The code currently uses `$XDG_CONFIG_HOME` or `$HOME/.config` on all Unix platforms. macOS users should set `AGOS_HOME` explicitly.

### Windows

```
AGOS_HOME = {FOLDERID_RoamingAppData}\agos-proxy
```

### Override (all platforms)

Set `AGOS_HOME` explicitly to any directory:

```sh
# Linux / macOS
export AGOS_HOME=/opt/agos/data

# Windows (PowerShell)
$env:AGOS_HOME = "C:\ProgramData\agos-proxy"
```

---

## Examples

### Development (isolated store)

```sh
AGOS_HOME=/tmp/agos-dev RUST_LOG=debug agos-proxy serve
```

### Production (systemd)

In the systemd unit file:

```ini
Environment=AGOS_HOME=/var/lib/agos-proxy
Environment=RUST_LOG=info
```

### Docker

```sh
docker run -d \
  -p 8080:8080 \
  -v /data/agos:/data \
  -e AGOS_HOME=/data \
  -e RUST_LOG=info \
  agos-proxy serve --bind 0.0.0.0:8080
```

### CI / Testing

```sh
export AGOS_HOME=$(mktemp -d)
agos-proxy bootstrap from-file test-setup.json
agos-proxy serve --bind 127.0.0.1:3000 &
# ... run tests ...
kill %1
```

### Multiple Instances

```sh
# Instance 1 — port 3000
AGOS_HOME=/data/instance1 agos-proxy serve --bind 127.0.0.1:3000 &

# Instance 2 — port 3001
AGOS_HOME=/data/instance2 agos-proxy serve --bind 127.0.0.1:3001 &
```

Each instance has its own store, its own profiles, and its own rate limits.
