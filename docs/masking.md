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

**Behavior rules for a mask implementation:**

- never read or modify the request/response body — stream it through
  (`proxy_buffering off` on nginx, `redirect: manual` in fetch);
- strip `X-forward-*`, `cf-*`, `x-forwarded-*`, and don't forward `host`;
- allowlist: reject any request whose secret doesn't match (default-deny);
- long timeouts for LLM responses (SSE can idle for minutes).

Ready-made implementations: `examples/mask/worker.js` (Cloudflare),
`examples/mask/lambda.mjs` (Lambda function URL), `examples/mask/nginx.conf`
(any VPS).

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
