# Egress masking — route each key through its own network identity

Some upstreams meter and rate-limit **by client IP**, not by API key. If you
hold several keys for the same upstream, all of them share one bucket because
AGOS Proxy egresses from your machine's IP. Egress masking fixes that: requests
to a provider are sent through a small **masking server** (a relay you
control), so each provider/key gets its own network identity — its own rate
limit, its own quota view.

Masks bind at the **provider level** (optionally with a profile-wide default),
so five keys = five providers = five masks = five distinct egress identities.

> **Verify the premise first.** Masking only helps if the limit is keyed on
> IP/ASN. If the upstream keys on account identity (email, payment method,
> device), masks change nothing. See
> `docs/tutorials/multi-key-ip-rotation.md` for the 30-minute experiment that
> decides whether masking helps you, *before* you deploy anything.

## The contract (backend-neutral)

Any HTTP service that satisfies this contract is a valid mask — a Cloudflare
Worker, an AWS Lambda function URL, a Cloud Run service, or a 20-line nginx
config on a VPS. AGOS core assumes none of them.

**Request** (AGOS → mask):

| Header | Meaning |
|---|---|
| `X-forward-mask: <secret>` | shared secret; wrong/absent → reject with 403 |
| *(original upstream URL)* | the mask forwards the request body unmodified to this URL |

**Response** (mask → AGOS):

| Header | Meaning |
|---|---|
| `X-forward-mask: ok` | the failure, if any, came from the upstream provider |
| `X-forward-mask: err` | the mask itself failed (reject, upstream fetch error) |

**Probe endpoint** (optional but recommended): a request carrying
`X-forward-probe: ip` (plus the secret) returns the mask's own egress identity
as JSON:

```json
{"ip": "5.9.8.7", "asn": "AS24940 Hetzner", "country": "DE"}
```

**Behavior rules for a mask implementation:** the complete algorithm is
specified in [Masking server handling](#masking-server-handling) below.

Ready-made, conforming implementations: `examples/mask/worker.js`
(Cloudflare), `examples/mask/lambda.mjs` (Lambda function URL),
`examples/mask/nginx.conf` (any VPS).

## Masking server handling

A normative description of what a masking server must do with every request.
Order matters: authentication first, probe second, everything else only after
both pass. "MUST" items are what AGOS's failure classification relies on.

### 1. Authenticate (MUST, always first)

- Read `X-forward-mask`. Compare against the configured secret.
- Wrong **or absent** secret → `403` and stop. Default-deny: every path, every
  method, no exceptions. There is no unauthenticated endpoint.
- Never echo the secret back — not in a response body, not in a log line, not
  in an error message. Rate-limit 403s (the nginx example rate-limits the
  probe zone) to slow brute-forcing.

### 2. Probe (SHOULD)

- Request carries `X-forward-probe: ip` (and the secret): answer `200` with
  the mask's **own egress identity** as JSON
  `{"ip": "...", "asn": "AS24940 Hetzner", "country": "DE"}` and stop — do
  not forward a probe upstream. `asn`/`country` may be `null` if unknown;
  `ip` must be the address the *upstream* would see. This endpoint is what
  `mask test --egress-ip` and `mask audit` consume.

### 3. Validate the target (MUST)

- `X-forward-target` must be an **absolute `https://` URL**. Missing, relative,
  or plain-http target → `502` with `X-forward-mask: err`. Plain http would
  expose the body and the provider token in transit.
- **SSRF discipline:** the client picks the destination, so anyone holding the
  secret can relay anywhere. If the mask endpoint is reachable by more than
  your AGOS instance, SHOULD allowlist upstream hostnames (e.g. only
  `api.openai.com`, `api.anthropic.com`) and reject everything else with
  `err`. A secret is the only gate otherwise — treat its compromise as full
  relay compromise.

### 4. Sanitize request headers (MUST)

Strip, before forwarding:

| Strip | Why |
|---|---|
| `x-forward-*` (`x-forward-mask/target/probe`) | control headers are for this hop only |
| **`x-forwarded-*`** (`-for`, `-host`, `-proto`) | forwarding the client's `X-Forwarded-For` **reveals your real IP** to the upstream — the one mistake that defeats the whole mask |
| `cf-*` | platform-injected, meaningless upstream |
| `host` | must be the target's host, never yours |
| `content-length` | recomputed by the forwarding layer |
| `accept-encoding` | either pass it through and stream bytes opaquely (nginx), or drop it and let the runtime renegotiate (Workers/Lambda decompress transparently). Never combine negotiated compression with buffering |
| hop-by-hop (`connection`, `keep-alive`, `transfer-encoding`, `upgrade`, `expect`, `proxy-*`) | invalid across a proxy hop |

Keep everything else untouched — in particular the upstream `Authorization`
header AGOS attached, `anthropic-version`, and similar.

### 5. Forward (MUST)

- Same method, **body streamed unmodified**. Never read, parse, or buffer the
  full body: nginx `proxy_buffering off; proxy_request_buffering off;`,
  Workers fetch pass-through (`duplex: "half"`), Lambda
  `streamifyResponse`. LLM streams run for minutes; buffering breaks SSE and
  times out clients.
- **`redirect: manual` (or equivalent) — never follow redirects.** Following
  one would re-send the body to a different host and report a different
  status to AGOS. Pass 3xx through verbatim.
- Generous timeouts: read/idle ≥ 3600 s, and no wall-clock cap below the
  upstream's longest possible stream (serverless ceilings are in the table
  below — pick the backend accordingly).

### 6. Annotate the response (MUST)

- Pass the upstream status and headers through unchanged and add
  `X-forward-mask: ok`. Never modify the body.
- Upstream errors (400/401/429/500…) are **upstream truth**: forward them as
  `ok` — AGOS needs to see the provider's 429 to park the key. Only failures
  of the mask itself are `err`.

### 7. Failure paths (MUST)

| Situation | Status | `X-forward-mask` |
|---|---|---|
| wrong/absent secret | `403` | *(absent)* |
| probe request | `200` + identity JSON | — |
| missing/invalid target | `502` | `err` |
| fetch/TLS/timeout before first byte | `502` | `err` |
| upstream responded (any status) | upstream's status | `ok` |

AGOS classifies on this header: `err` means "mask broken, leave the key
alone"; `ok` with a 429 means "key rate-limited, park it". If a mask
implementation cannot set `err` (e.g. minimal nginx), AGOS falls back to
status-code heuristics only if the mask's optional status preset is
configured — that's why the header contract is strongly preferred.
After the first byte, a broken upstream connection simply ends the stream;
AGOS surfaces it as a truncated response.

### 8. Privacy and hardening (MUST/SHOULD)

- Never log request/response bodies, the secret, or provider tokens. If you
  log at all, log method + target host + status + duration only.
- HTTPS-only listener; keep the secret in an env var or secret store, never in
  a committed file. One secret per mask; rotate from AGOS with
  `mask set --secret ...` and redeploy the new value.
- Do not add identifying headers of your own (`x-relay-by`, `server` tokens
  are fine to leave default, but nothing custom) — the goal is a boring,
  unremarkable client.
- Keep the probe endpoint cheap and rate-limited; it's the only endpoint that
  answers without touching the upstream.


## Backend notes (verified limits)

| Backend | Streaming ceiling | Payload caps | Egress identity |
|---|---|---|---|
| CF Worker | unlimited | 100 MB request | platform pool; colo follows *you*, not the provider |
| Lambda fn URL | **15 min hard cap** | 6 MB request; 6 MB buffered / 200 MB streamed response (2 MB/s after 6 MB) | shared AS16509 pool by default |
| Cloud Run | ≤ 60 min (configurable) | HTTP limits | static IP needs VPC egress + Cloud NAT (paid) |
| VPS (nginx) | none | none | **you choose region, IP and ASN** |

Key insight: five Workers = one ASN (AS13335), five Lambdas = one ASN
(AS16509). If the upstream buckets by ASN rather than exact IP, five
serverless deployments are still **one identity**. For genuinely distinct
identities use cheap VPSs from different providers/regions — the nginx example
is ~20 lines.

## CLI

```sh
# Register a mask (secret is stored encrypted, never displayed again)
agos-proxy mask add --profile coder1 --name hop1 \
  --kind cf_worker --endpoint-url https://hop1.example.workers.dev \
  --secret "$(openssl rand -hex 24)" [--max-body-bytes 6000000] \
  [--expected-egress-ip 1.2.3.4]

# Mark it the profile default (providers without their own mask use it)
agos-proxy mask set-default --profile coder1 --mask hop1

# Bind it to a provider key (or pass --mask at `provider add` time)
agos-proxy provider edit      # pick the mask interactively

# Verify the secret and see the egress identity (repeat to test rotation)
agos-proxy mask test --profile coder1 --mask hop1 --egress-ip --repeat 5

# Audit ALL masks: warns and exits non-zero if two share an ASN
agos-proxy mask audit --profile coder1
```

## How AGOS applies a mask

At target-build time AGOS resolves the mask (provider mask → profile default →
none) and rewrites the outbound request: the upstream URL is sent to the mask
endpoint, the secret header is attached. This includes **health probes**, so a
key's health is judged through the same identity its traffic uses. Responses
carry `X-forward-mask`, which distinguishes mask failures from provider
failures — a broken mask never demotes a healthy key.

Bodies larger than a mask's `max_body_bytes` (e.g. base64 images vs Lambda's
6 MB request cap) roll over to the next target instead of failing.

## Export / import

`config export`/`import` and `config` sealed bundles carry masks end-to-end:
`masking_servers` (with secrets, inside the sealed payload), each provider's
bound mask by name, and the profile's default mask. Bundles from older
versions import unchanged (all mask fields are optional with defaults).

`bootstrap`-seeded providers start unbound; attach masks afterwards with
`provider edit` or `provider add --mask <name>`.

## Security notes

- **One secret per mask.** A leaked secret is an open relay on someone else's
  bill; rotate with `mask set --secret ...`.
- Secrets are encrypted at rest (`crypto::encrypt`, same as provider tokens)
  and never appear in `mask list`, logs, or API responses.
- HTTPS-only endpoints; the secret header only travels over TLS.
- The mask sees your request bodies and provider tokens. Run it on
  infrastructure you control.

## Honest limits

- Masking defeats IP/ASN-keyed limits only. Account-keyed limits (same email,
  payment method, device fingerprint) are unaffected.
- Each hop adds RTT; pick a mask region near the upstream.
- Multiplying an IP-metered quota pool may violate the upstream's terms of
  service. Understand the risk: credit/account revocation, or ASN-level bans
  that also affect other users of the same ranges.
