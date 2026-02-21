# Failover in AGOS Proxy

How the gateway keeps a request alive when one or more underlying providers
slip. This document describes the mechanism end-to-end so you can reason about
the behavior you will see in production.

## What "failover" means here

A route in AGOS Proxy never represents a single model. It represents an
**ordered chain** of models, each on a specific provider. When a caller asks a
route for a completion, AGOS Proxy picks an entry from that chain and tries it.
If the attempt fails in a way that suggests the provider is having trouble, AGOS
Proxy marks that entry unhealthy and tries the next one — all within a single
caller request.

So the caller asks for `Programmer/php-developer-3.5-flash`, and behind the
scenes AGOS Proxy may touch several providers before returning an answer. The
caller sees one request, one response, and possibly a little extra latency — not
a sequence of failures.

## When a model is skipped

A model entry is skipped from live traffic when it is not `Healthy`. The states
that exclude an entry from the rotation are:

| State        | Live traffic? | Probed? | Notes                                           |
|--------------|---------------|---------|-------------------------------------------------|
| Healthy      | Yes           | No      | Normal rotation.                                |
| Degraded     | Yes (briefly) | No      | Recently failed; will become Unhealthy on next probe cycle. |
| Unhealthy    | No            | Yes     | Skipped; probed until it proves itself.         |
| Disabled     | No            | No      | Manually switched off by the user.              |

An entry is marked unhealthy when a live request hits any of:

- Connection failure (network error, refused connection, DNS failure).
- Timeout (the upstream took too long to start answering).
- HTTP 5xx from the upstream.
- HTTP 429 from the upstream (rate limit — treated as an unhealthy signal so
  the chain can move on; the entry may recover on the next probe cycle).
- A response that fails schema validation (the upstream returned something the
  translator could not parse).

## Health and the background probe loop

Failover handles the immediate failure. Recovery is handled by a background
probe loop.

The probe loop:

1. Wakes every 30 seconds.
2. Collects every entry that is not Healthy.
3. Pings each candidate with `GET /v1/models` (cheap, fast, provider-agnostic).
4. Transitions statuses according to the state machine below.

### Status transitions

```
Healthy ──────► Healthy   (stays healthy; probes do not touch it)
             ▲
             │ live-traffic failure
             │ (immediate, during a request)

Degraded ──probe success──► Healthy
         ──probe failure──► Unhealthy

Unhealthy ─probe success──► Degraded   (one success; not fully trusted yet)
          ──probe failure──► Unhealthy

Disabled ────────────────► Disabled    (never probed, never live)
```

Points to internalize:

- A Healthy entry never becomes unhealthy because of a probe. Only a live
  request that fails can demote it. This is deliberate: a probe failure during
  a quiet period should not take a known-good entry out of rotation.
- An Unhealthy entry must succeed **once** as a probe before it becomes
  Degraded. It does not jump straight back to Healthy. This prevents flapping
  entries from rejoining full rotation immediately after a transient recovery.
- A Degraded entry that fails its probe becomes Unhealthy; one that succeeds
  becomes Healthy.

## Ordering and strategy

The order in which models are tried depends on the route's routing strategy.

### Priority (default)

The chain is tried in stored order: entry with `priority = 1` first, then
`2`, then `3`, and so on. Vulnerability order is explicit; the user decides which
providers are preferred and which are backups.

### Round robin

Traffic is distributed across all currently-healthy entries. Failover still
happens on failure, but there is no single "primary" and "backup" — every
healthy entry participates in the rotation.

### Weighted

Like round robin, but weighted by each entry's `weight`. Higher weight means
more traffic. Failover still applies: a failed weighted pick is skipped and the
next healthy entry is tried.

All three strategies respect the healthy filter — an unhealthy entry is never
picked for live traffic regardless of strategy.

## Streaming and failover

Non-streaming requests: failover is transparent. The proxy reads the full
response before returning, so if the first provider fails, the next is tried and
the caller never sees the intermediate failure.

Streaming requests (SSE): failover has a documented limitation. If a stream fails
after tokens have begun flowing, that partial failure is logged distinctly
because it cannot be invisibly retried. The caller will see the stream break.

This is not a bug in the usual sense — it is the nature of stream failover. Once
a caller has started consuming a stream and the upstream dies, there is no clean
way to "continue" that stream from a different provider. AGOS Proxy logs the
partial failure so it is visible in the usage log without pretending it did not
happen.

Design implication: if you need transparent failover and streaming, prefer
non-streaming for the cases where provider reliability is critical, or accept
that a streaming request may break mid-stream if the provider dies.

## What the caller sees

In the normal case: one request, one response, latency that depends on the
chosen provider.

In a failover case (non-streaming): the caller's request takes longer than a
single-provider call would, but returns successfully (assuming the chain has a
healthy provider). The caller does not see the intermediate failure.

In an exhausted-chain case: the route returns an error. The error includes
enough information for the caller to understand that the entire chain was tried
and none succeeded.

In a streaming failover case: if the failure happens before the stream starts,
failover behaves like the non-streaming case. If it happens mid-stream, the
stream breaks; the partial failure is logged.

## Health visibility

Health state is observable in several places:

- `atos-proxy route status <proxy> <route>` — shows each entry's current status.
- `atos-proxy usage stats --profile <name>` — shows per-model aggregates,
  including failure counts and average latency.
- `atos-proxy usage recent --profile <name>` — shows recent individual requests,
  including which entry handled each one and whether it succeeded.

These are the tools you use to answer: "Is this route actually healthy, or is it
constantly failing over?"

## Operational guidance

### Interpreting continuous failover

If a route is constantly failing over, one of these is happening:

- A provider is down or rate-limited. Check `usage recent` for the failing
  provider's errors. Check provider status pages.
- The "healthy" provider is actually slower or flakier than you think. Check
  `usage stats` for latency and failure counts per entry.
- The route chain has no healthy provider. Check `route status`.

### Recovering a route

1. Check `route status` to see which entries are unhealthy.
2. Check `usage recent` to see what errors those entries produced.
3. If a provider is genuinely down, decide whether to wait (the probe loop will
   eventually recover it) or to manually disable it (`route model disable`) so
   it is not used.
4. If a provider is rate-limited, reduce its weight or move it lower in the
   priority chain.
5. Re-enable entries with `route model enable` once the provider recovers.

### Disabling an entry

`atos-proxy route model disable --proxy <proxy> --route <route> --model <model>`
marks an entry `Disabled`. Disabled entries are never probed and never used for
live traffic, regardless of their underlying health. Use this to permanently
remove a bad provider from a chain without deleting the route entry.

### Tuning a chain

The recommended way to build a resilient chain:

1. Put your preferred provider first (or give it high weight).
2. Put a different provider as backup (different provider, different model if
   possible, so a single provider incident does not take out the whole chain).
3. If you need a third layer, add it with lower priority or weight.
4. Watch `usage stats` for a few days and adjust.

Avoid putting every entry on the same provider — that is not a failover chain,
that is a retry loop with extra steps.

## Diagnostics

When failover is behaving unexpectedly, the useful data sources are:

1. The route status (`atos-proxy route status`).
2. The recent usage log (`atos-proxy usage recent`).
3. The per-model stats (`atos-proxy usage stats`).
4. The proxy's own logs (set `RUST_LOG=debug` if you need more detail).

Together these tell you which entries failed, why, and how often.
