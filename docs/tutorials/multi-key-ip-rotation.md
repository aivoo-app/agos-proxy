# Multi-key IP rotation — measure before you build

This recipe is for the case "I have N API keys for the same upstream, but the
upstream meters **by IP**, so N keys buy me one key's worth of credit."
Egress masking (see [masking.md](../masking.md)) is the fix — *if* the premise
holds. Run this experiment first; it costs ~30 minutes and decides whether
masking helps you at all.

## Step 0 — establish the baseline (no masks yet)

1. Note the remaining credit on key **A** (from the upstream's dashboard or a
   usage endpoint, if one exists).
2. Use key **B** for a few real completions from your normal IP.
3. Re-check key **A**'s remaining credit.

| Observation | Meaning |
|---|---|
| A's credit dropped | Limit is IP-keyed: both keys share one bucket. Masking **can** help. |
| A's credit unchanged | Limit is keyed on the account (email, payment, device). Masking **cannot** help; N accounts were never N×. Stop here. |

## Step 1 — does one mask give a fresh bucket?

1. Stand up **one** masking server (the fastest is a Cloudflare Worker, ~10
   minutes — see `examples/mask/worker.js`).
2. Register it: `agos-proxy mask add ... --secret ...`, then
   `agos-proxy mask test --mask hop1 --egress-ip --repeat 5` — note the
   reported egress IP/ASN (CF often gives only 1–3 IPs per colo; that is
   expected and recorded for later).
3. Route one key through the mask and run real completions until it would
   normally exhaust, watching the upstream's credit view.

**Pass:** the masked key consumed from a *fresh* bucket. **Fail:** still the
shared pool → the limit is not plain-IP; you may stop here.

## Step 2 — exact IP, /24, or ASN?

This tells you how much diversity you actually need:

1. Get **two** masks on the **same** ASN (e.g. two Workers): use key A through
   mask 1 until throttled, then immediately through mask 2.
2. Get **one** mask on a **different** ASN (a cheap VPS): same test.

| Result | Upstream granularity | What you need |
|---|---|---|
| Mask 2 (same ASN) also throttled, VPS fresh | per-IP | any N distinct IPs, even one platform |
| Mask 2 (same ASN) fresh, VPS fresh | /24 or looser | ≥ 2 distinct IPs, still easy |
| Both throttled / VPS also throttled | **per-ASN** or account | only *distinct ASNs* help: N VPSs on N providers |

## Step 3 — wire up N keys in AGOS

```sh
# One mask per key (distinct identities!)
for i in 1 2 3 4 5; do
  agos-proxy mask add --name key-$i --kind nginx \
    --endpoint-url https://hop$i.example.com --secret "$SECRET$i"
done

# Bind each to its own provider (one provider row per key)
agos-proxy provider add --name upstream-1 --mask key-1 ...
agos-proxy provider add --name upstream-2 --mask key-2 ...

# All five go into one route; RoundRobin spreads load evenly
agos-proxy route edit          # pick round_robin

# Verify identities — exits non-zero if two masks share an ASN
agos-proxy mask audit
```

Then confirm the spread works under load:

```sh
agos-proxy usage stats --by-key --profile coder1
```

All keys should accumulate requests roughly evenly, and 429s on one key
should never starve the others: a 429 puts that key in cooldown (`Retry-After`
or `AGOS_RATE_LIMIT_COOLDOWN_SECS`) while the router rotates to the next.

## Placement guidance

- **Distinct ASNs matter more than distinct IPs** in most observed deployments
  (step 2 tells you which). Five cheap VPSs on five different providers give
  five ASNs for $0–30/mo total. Five serverless deployments usually share one
  platform ASN — `mask audit` warns about exactly that.
- Put masks **near the upstream** to cut added RTT; the mask is a plain
  forwarder, so region choice is yours.
- Watch for the platform's payload/stream limits (table in masking.md) — set
  `--max-body-bytes` on Lambda masks so oversized vision requests roll to the
  next key instead of failing.

## Caveats (read once, take seriously)

- If step 0 shows account-keyed limits, **no infrastructure fixes it**.
- Rotating IPs to multiply an IP-metered free-credit pool is typically
  classified as rate-limit evasion under the upstream's terms of service.
  Realistic downsides: credit/account revocation, or ASN-level bans that also
  affect unrelated users of the same ranges. Decide with eyes open.
- One secret per mask, HTTPS only, and keep the hop on infrastructure you
  control — the mask sees your prompts and provider tokens.
