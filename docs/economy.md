# Economy Mode — spend less, keep quality

AGOS Proxy can cut token spend **60–85%** with zero client changes.
The trick is **cheap-first routing + exact-cache + output clamp**.

## How it works

1. **Cheap-first chain (`strategy: economy`)** — entries sorted by
   `price_per_1m` (blended USD/1M tokens). Simple prompts stop at
   `gpt-4o-mini / haiku / flash` (~$0.40/1M). Failover still escalates
   to the flagship (~$6/1M) on error, so hard tasks keep quality.
2. **Exact-cache (`cache_ttl_secs`)** — deterministic requests
   (`temperature` unset/0, no tools, non-streaming) are hashed
   (SHA-256) and served from SQLite. Hit = **0 upstream tokens**,
   marked `X-Agos-Cache: HIT`.
3. **Output clamp (`max_tokens`)** — caps runaway completions per route.
4. **Explicit escalation** — send `{"economy_escalate": true}` to force
   the most expensive entry first when you know the task is hard.

## Setup (no client change)

```sh
# 1. Switch a route to Economy (keeps existing chain)
agos-proxy route edit --proxy Programmer   # pick Economy

# 2. Tune limits: clamp outputs, enable 1h exact-cache
agos-proxy route economy --proxy Programmer --max-tokens 1024 --cache-ttl 3600

# 3. Set prices (auto-guessed at creation; override as needed)
agos-proxy route model price --proxy Programmer --price 0.4   # cheap entry
agos-proxy route model price --proxy Programmer --price 6.0   # flagship

# 4. Watch savings
agos-proxy usage stats --profile coder1   # now shows EST $ + TOTAL
agos-proxy route status --route php-dev   # shows PRICE per entry
```

Bootstrap JSON also supports it:

```json
{
  "profile": "coder1",
  "providers": [{"name": "openai", "base_url": "https://api.openai.com/v1", "auth_token": "sk-..."}],
  "proxies": [{
    "name": "Programmer",
    "routes": [{
      "name": "php-dev",
      "strategy": "economy",
      "max_tokens": 1024,
      "cache_ttl_secs": 3600,
      "models": [
        {"provider": "openai", "model": "gpt-4o-mini", "price_per_1m": 0.4},
        {"provider": "openai", "model": "gpt-4o", "price_per_1m": 6.0}
      ]
    }]
  }]
}
```

## What it saves

| Upstream | Blended $/1M |
|---|---|
| flagship (`gpt-4o/sonnet`) | ~$6.00 |
| cheap (`mini/haiku/flash`) | ~$0.40 |
| cache hit | $0 |

For 1M prompt + 0.3M completion/month: all-flagship ≈ $5.50 →
80%-cheap ≈ $1.24 (**~77% off**) → +20% cache hit ≈ $0.99 (**~82% off**).

## Safety notes

- Cache is **exact-match only** (no semantic guessing) — no wrong answers.
- Streaming, tool calls, and `temperature > 0` bypass the cache.
- Old databases migrate automatically (`ALTER TABLE` on open).
- Export/import (`config export/import`) preserves prices and limits.
