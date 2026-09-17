# ADR 005 — Egress masking

*Status: accepted · Version: v0.1.6*

## Context

Some upstreams meter and rate-limit by client IP, so several API keys of the
same upstream share one quota bucket when AGOS egresses from a single address.
Separately, some deployments need egress to appear from a specific region.
Both call for an outbound network-identity hop that AGOS can relay through.

## Decision

Introduce **masking servers**: small HTTP relays AGOS forwards provider
requests through. Design constraints:

1. **Backend-neutral contract.** A mask is any HTTP service implementing: an
   `X-forward-mask: <secret>` gate, an optional `X-forward-target` upstream
   URL, an `X-forward-mask: ok|err` response marker, and a probe endpoint
   returning the mask's own egress identity (`ip`, `asn`, `country`). No
   platform is assumed in core; Cloudflare Workers, Lambda function URLs,
   Cloud Run services and nginx-on-VPS are all supported by the same client
   code (`examples/mask/`).
2. **Provider-level binding** (`providers.masking_server_id`), with an
   optional profile-wide default
   (`profiles.default_masking_server_id`). Route-level binding is deliberately
   omitted: a mask on a route would collapse all of a route's keys to one
   identity — the exact pooling masks exist to avoid.
3. **Resolution at target-build time**, denormalized into the resolved
   `Target`, so `mask::apply` stays pure and unit-testable; the mask rides
   along in `Provider` so every egress path — including health probes —
   inherits it with no signature changes.
4. **Mask failures never demote keys.** The `X-forward-mask` response marker
   (plus an optional per-mask status preset) classifies transport failures as
   mask-origin; only genuine provider responses change `ModelStatus`.
5. **Oversize roll-over.** `masking_servers.max_body_bytes` (e.g. 6 MB for
   Lambda request bodies) makes oversized requests roll to the next target
   without demoting anything.
6. **429-aware cooldown** (`route_entries.cooldown_until`): honoring
   `Retry-After` (fallback `AGOS_RATE_LIMIT_COOLDOWN_SECS`), skipped during
   resolution, with a soonest-expiring fallback so requests never hard-fail.
   Cooldown is independent of `ModelStatus`.
7. **Secrets at rest** are encrypted with the existing ChaCha20 store key and
   never printed; sealed profile bundles round-trip them (the bundle is
   already passphrase-sealed).

## Consequences

- Five keys + five masks + `round_robin` + cooldowns = genuinely spread load;
  `usage stats --by-key` and `mask audit` (warns on shared ASNs) make the
  spread observable.
- Each hop adds RTT and a trust point: the mask sees request bodies and
  provider tokens, so masks belong on infrastructure you control, with one
  secret per mask and default-deny.
- Masking only defeats IP/ASN-keyed limits; account-keyed limits are
  unaffected. `docs/tutorials/multi-key-ip-rotation.md` documents the
  measurement protocol that should precede any deployment.
- Multiplying an IP-metered quota pool may violate an upstream's terms of
  service; the feature is documented as a general egress-control tool with
  that risk stated explicitly.
