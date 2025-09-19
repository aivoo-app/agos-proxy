# Architecture

AGOS Proxy is a self-hosted AI gateway. From the caller's point of view it is a
single OpenAI-compatible provider; underneath, it fans requests out across
configured LLM providers with automatic failover.

## Mental model

Two parallel hierarchies share a common top-level tenant, the **Profile**:

```
Profile ("coder1")
│
├── Providers                     how we reach the outside world
│     ├── Deepseek        base_url, auth_token
│     ├── Claude          base_url, auth_token
│     └── Google AI Studio base_url, auth_token
│
└── Proxies                      what the outside world is allowed to ask for
      └── Proxy: "Programmer"
            ├── Route: "php-developer-3.5-flash"
            │     1. deepseek-v4-flash        (Deepseek)
            │     2. glm-5.3-flash            (OpenRouter)
            │     3. gemini-3.5-pro           (Google AI Studio)
            │     4. claude-sonnet-5          (Claude)
            │
            └── Route: "flutter-expert-2-preview"
                  1. ...
```

- **Provider** = *how* a call is made (credentials + base URL).
- **Proxy / Route** = *what* callers may request.
- A **route never exposes a single model** — it exposes an *ordered fallback
  chain* of models, each pointing at one configured provider.

This split is the whole point: the caller configures a route name once
(`programmer/php-developer-3.5-flash`) and never sees Deepseek, Claude or
Gemini directly. Everything behind that name can change (models swapped,
providers added or removed) without the caller's configuration changing.

## Crate layout

The crate is organised so each subsystem is isolated and testable:

```
src/
├── lib.rs        crate root, module wiring, VERSION
├── main.rs       thin entry point that parses args and calls cli::run
├── cli/          the command tree (clap) + interactive wizards
├── domain/       the core data model (profiles, providers, proxies, routes)
├── storage/      persistence of the domain model (embedded SQLite)
├── server/       the OpenAI-compatible HTTP surface
└── crypto/       secrets at rest (encrypted provider tokens)
```

## Request lifecycle

1. A caller sends `POST /v1/chat/completions` with
   `Authorization: Bearer <profile_uid>` and `model: "programmer/php-developer-3.5-flash"`.
2. The server authenticates the UID, resolves it to a profile, then resolves
   the `proxy/route` string to a route.
3. The route's model chain is filtered to healthy, enabled entries (and, when
   the request needs it, to entries with the required capabilities).
4. Models are tried in order. Each attempt has a configurable timeout.
5. On success: the response (or stream) is returned untouched; usage and latency
   are logged against that model.
6. On failure: that model is marked unhealthy and the next one is tried
   immediately — the caller perceives extra latency, not a failed request.
7. Only if the whole chain is exhausted does the route return an error.

## Health / circuit breaking

Each route entry has a status (`healthy` / `degraded` / `unhealthy` /
`disabled`). Failure conditions: connection failure, timeout, HTTP 5xx,
HTTP 429, or a response that fails schema validation. A background task probes
`unhealthy` entries on a backoff schedule and promotes them back to `healthy`
when they recover, automatically rejoining rotation.

## Routing strategies

- **Priority** (default): strict pipeline order; fallback on failure.
- **Round robin**: spread traffic across all healthy entries.
- **Weighted**: like round robin, but biased by each entry's weight.

## Streaming

Responses are passed through as a live SSE stream. As with any gateway there is
a documented limitation: if a stream fails *after* tokens have begun flowing,
that partial failure is logged distinctly because it cannot be invisibly retried.

## Security

- Provider `auth_token` values are encrypted at rest (never in plaintext files).
- The profile UID acts as the bearer token — treated like any API key.
- A profile may be password-gated for interactive management.
- Token rotation is supported per profile.

## Tech stack

- **Runtime:** `tokio`
- **HTTP server / proxy:** `axum` (streams response bodies natively, which
  matters for SSE passthrough).
- **Outbound provider calls:** `reqwest` (async, streaming-capable).
- **Storage:** embedded SQLite (single file, no external DB).
- **Serialization:** `serde` / `serde_json`.
- **Secrets:** AES-GCM at rest, key derived via argon2 from an optional profile
  password.
- **CLI:** `clap` for flags, `dialoguer`/`inquire` for interactive wizards.

## Build phases

1. **MVP** — single profile, provider CRUD, proxy/route CRUD, priority failover,
   basic health checks, `/v1/chat/completions`, plaintext streaming passthrough.
2. **V1** — multi-profile, encrypted secrets, per-profile passwords, weighted /
   round-robin strategies, usage logging, config export/import.
3. **V2** — capability-aware routing, cost tracking, failover webhooks,
   `/v1/models` auto-listing, richer CLI stats.