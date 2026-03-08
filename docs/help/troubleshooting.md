# Troubleshooting Guide

> Common issues and how to fix them.

## Table of Contents

- [Installation Issues](#installation-issues)
- [Server Startup Issues](#server-startup-issues)
- [Authentication Errors](#authentication-errors)
- [Provider Errors](#provider-errors)
- [Routing Errors](#routing-errors)
- [Rate Limiting Issues](#rate-limiting-issues)
- [Performance Issues](#performance-issues)
- [Data/Storage Issues](#datastorage-issues)
- [Getting Help](#getting-help)

---

## Installation Issues

### `cargo build` fails with linker errors

**Symptom:** Linker errors during `cargo build --release`.

**Cause:** Missing system build tools.

**Fix:**
```sh
# Ubuntu / Debian
sudo apt-get install build-essential pkg-config libssl-dev

# Fedora / RHEL
sudo dnf install gcc pkg-config openssl-devel

# macOS (with Xcode Command Line Tools)
xcode-select --install

# Alpine (Docker)
apk add musl-dev openssl-dev pkgconfig
```

### `cargo build` fails with "feature `gen-docs` is required"

**Symptom:** Error when running `gen-docs` without the feature flag.

**Fix:**
```sh
cargo run --release --features gen-docs --bin gen-docs
```

---

## Server Startup Issues

### "Address already in use"

**Symptom:** `agos-proxy serve` fails with "address already in use".

**Cause:** Another process is listening on the same port.

**Fix:**
```sh
# Find what's using the port
lsof -i :3000    # macOS/Linux
netstat -ano | findstr :3000  # Windows

# Or use a different port
agos-proxy serve --bind 127.0.0.1:3001
```

### "cannot determine a home config directory"

**Symptom:** Error about missing home directory.

**Cause:** `$HOME` or `$XDG_CONFIG_HOME` is not set.

**Fix:** Set `AGOS_HOME` explicitly:
```sh
export AGOS_HOME=/path/to/agos-data
agos-proxy serve
```

### Server starts but no logs appear

**Symptom:** `agos-proxy serve` starts but produces no output.

**Cause:** Default log level is `info`. Set `RUST_LOG` for more verbosity.

**Fix:**
```sh
RUST_LOG=info agos-proxy serve
```

---

## Authentication Errors

### 401 Unauthorized

**Symptom:** All requests return `401 Unauthorized`.

**Causes & Fixes:**

1. **Missing bearer token:**
   ```sh
   # Wrong
   curl http://localhost:3000/v1/models

   # Right
   curl -H "Authorization: Bearer YOUR_TOKEN" http://localhost:3000/v1/models
   ```

2. **Wrong token:** The token must be the profile `id` (printed at creation). If lost, rotate it:
   ```sh
   agos-proxy profile token rotate <profile-name>
   ```

3. **Token with extra whitespace:** Ensure no trailing spaces or newlines in the token.

### Profile password prompts

**Symptom:** CLI commands ask for a password you didn't set.

**Cause:** The profile was created with a password, or you're operating on a different profile.

**Fix:**
- Enter the password (up to 3 attempts).
- If forgotten, there is no password recovery — delete and recreate the profile.
- Check which profile you're targeting: `agos-proxy profile list`.

---

## Provider Errors

### "All providers failed"

**Symptom:** Requests return errors; `usage recent` shows all attempts failed.

**Diagnosis:**
```sh
# Check what's happening
agos-proxy usage recent --profile <name> --limit 10

# Check route health
agos-proxy route status --route <route-name>
```

**Common Causes:**

| Cause | `usage recent` shows | Fix |
|-------|---------------------|-----|
| Expired API key | `401` from provider | Update token: `provider edit` |
| Rate limited by provider | `429` from provider | Add more providers to chain; reduce traffic |
| Provider outage | `5xx` or timeout | Wait for provider to recover; add fallback |
| Wrong base URL | Connection refused | Fix base URL: `provider edit` |
| Wrong model ID | `404` from provider | Check model name with provider docs |
| Network unreachable | Connection timeout | Check firewall, DNS, proxy settings |

### Provider token is correct but still getting 401

**Cause:** The token may be stored incorrectly, or the provider requires extra headers.

**Fix:**
```sh
# Re-enter the token
agos-proxy provider edit --profile <name>

# Check if extra headers are needed (e.g., Anthropic needs anthropic-version)
# Add them when prompted during provider edit
```

---

## Routing Errors

### "Model not found" / 404

**Symptom:** `404 Not Found` when calling `/v1/chat/completions`.

**Cause:** The model string doesn't match any configured route.

**Diagnosis:**
```sh
# List available models
curl -H "Authorization: Bearer TOKEN" http://localhost:3000/v1/models
```

**Fix:** Use the exact `<proxy>/<route>` format. Case matters:
- If your proxy is `Programmer` and route is `code-gen`, use `"model": "Programmer/code-gen"`.

### Route has no entries

**Symptom:** Route exists but has no model entries.

**Fix:**
```sh
# Add model entries to the route
agos-proxy route model add --proxy <proxy-name>
```

### Wrong provider in route entry

**Cause:** The route entry references a deleted or renamed provider.

**Fix:**
```sh
# Check entries
agos-proxy route status --route <route>

# Re-add the entry with the correct provider
agos-proxy route model remove --proxy <proxy> --route <route> --model <model>
agos-proxy route model add --proxy <proxy>
```

---

## Rate Limiting Issues

### 429 Too Many Requests

**Symptom:** Requests return `429` after a certain volume.

**Cause:** The profile's rate limit has been hit.

**Diagnosis:**
```sh
# Check current limit
agos-proxy profile show <name>

# Check recent usage
agos-proxy usage recent --profile <name> --limit 50
```

**Fixes:**
1. **Increase the limit:**
   ```sh
   agos-proxy profile limit <name> 300  # 300 req/min
   agos-proxy profile limit <name> 0    # unlimited
   ```

2. **Add exponential backoff** to your client:
   ```python
   import time
   from openai import APITimeoutError, RateLimitError

   for attempt in range(5):
       try:
           response = client.chat.completions.create(...)
           break
       except RateLimitError:
           time.sleep(2 ** attempt)
   ```

3. **Distribute across profiles** if you have multiple agents.

---

## Performance Issues

### Slow first request

**Symptom:** The first request after server startup is slow.

**Cause:** Normal — the health probe loop hasn't warmed up yet. The first request may trigger a health check.

**Fix:** Wait 30 seconds after startup before sending traffic, or send a warm-up request.

### Slow failover

**Symptom:** Failover takes a long time (10+ seconds per attempt).

**Cause:** The default per-attempt timeout is 10 seconds. If a provider is slow to respond (not failing), the full timeout elapses before the next entry is tried.

**Fix:**
- Use providers with similar latency in the same chain.
- Put the fastest provider first in priority chains.
- Monitor with `agos-proxy usage stats` to identify slow providers.

### High memory usage

**Cause:** The usage log grows unbounded over time.

**Fix:** The usage log is stored in SQLite and grows with each request. For high-volume deployments, periodically archive old records or implement log rotation.

---

## Data/Storage Issues

### "database is locked"

**Symptom:** SQLite "database is locked" errors under concurrent load.

**Cause:** SQLite allows one writer at a time. High concurrent write load can cause contention.

**Fix:**
- AGOS Proxy uses WAL mode to reduce locking. Ensure your filesystem supports WAL.
- If running multiple processes against the same store, use separate `AGOS_HOME` directories.
- For high-write scenarios, consider the single-process limitation of SQLite.

### Corrupted store after copy

**Symptom:** "decryption failed" after copying the SQLite file.

**Cause:** The master key is tied to the store. Copying the SQLite file without using `config export`/`config import` breaks the encryption.

**Fix:** Always use `config export` and `config import` for migration:
```sh
# Source machine
agos-proxy config export --profile <name> --output backup.json

# Target machine
agos-proxy config import --path backup.json
```

### Store grew too large

**Cause:** The usage log accumulates records over time.

**Fix:** The usage log can be trimmed by deleting old records from the `usage_log` table. Implement a periodic cleanup job, or export/import to a fresh store.

---

## Getting Help

### Check the logs

```sh
# Server logs (if running in foreground)
RUST_LOG=debug agos-proxy serve

# systemd logs
journalctl -u agos-proxy -f

# Docker logs
docker compose logs -f proxy
```

### Check the usage log

```sh
# What happened to recent requests?
agos-proxy usage recent --profile <name> --limit 20

# Per-model aggregates
agos-proxy usage stats --profile <name>
```

### Check route health

```sh
# See the status of every entry in a route
agos-proxy route status --route <route-name>
```

### Open an issue

If the above doesn't resolve your issue:

1. Run `agos-proxy --version` and note the version.
2. Collect the output of `agos-proxy profile list`, `agos-proxy route status`, and `agos-proxy usage recent`.
3. Check the server logs with `RUST_LOG=debug`.
4. Open an issue on GitHub with:
   - What you expected to happen
   - What actually happened
   - Steps to reproduce
   - Version, OS, and relevant config (redact secrets)

### Security issues

Do not open public issues for security vulnerabilities. See [SECURITY.md](../SECURITY.md).
