# AGOS Proxy — Security Model

> Threat model, encryption, password hashing, token handling, and operational
> guidance. Read this before deploying AGOS Proxy in a shared or internet-facing
> environment.

## Threat model

AGOS Proxy is a self-hosted gateway. The trust model is:

- **The host is trusted.** AGOS Proxy runs on a machine you control. If an
  attacker has root on that machine, they can read the SQLite store, the master
  key in memory, and any plaintext tokens the process holds. There is no
  protection against a compromised host.
- **The network is not trusted.** The proxy should be placed behind a reverse
  proxy, VPN, or firewall when exposed beyond localhost. The bearer token is the
  only authentication mechanism today; there is no TLS termination inside AGOS
  Proxy. Use a front-end proxy (nginx, Caddy, Envoy, cloud LB) for TLS.
- **Provider tokens are sensitive.** They unlock real spending on real providers.
  Their confidentiality is the primary security goal of the encryption subsystem.
- **Profile tokens are sensitive.** They grant access to a profile's routes and
  providers. Treat them like API keys.

### What AGOS Proxy assumes

| Assumption                              | Mitigation                                              |
|-----------------------------------------|---------------------------------------------------------|
| Host is not compromised                | Operational discipline; no technical control today.     |
| Local CLI user is authorized           | Profile passwords gate interactive management.          |
| Bearer token is kept secret            | Generated with 128 bits of entropy; show once, store safely. |
| Provider tokens must not leak to disk  | ChaCha20-Poly1305 encryption at rest.                   |
| One process = one store               | Embedded SQLite; no shared state across instances.      |

### What AGOS Proxy does not do today

- TLS termination (use a front-end proxy).
- Per-token revocation other than rotation.
- IP allow-listing or CIDR filtering.
- Audit logging to an external system (usage log is local SQLite).
- Multi-process or multi-node coordination.

## Encryption at rest

### Algorithm

Provider tokens are encrypted with **ChaCha20-Poly1305** (AEAD). For each token:

1. Generate a random 96-bit nonce.
2. Encrypt the plaintext token with the master key and nonce.
3. Store `nonce || ciphertext` as the blob in the `providers.auth_token` column.

The nonce is never reused because it is randomly generated for each encryption
operation (256-bit master key, 96-bit nonce — the probability of collision is
negligible for any realistic store size).

### Master key

- 256-bit (32-byte) symmetric key, randomly generated on first store creation.
- Stored in the `meta` table as a raw blob.
- Held in memory (inside the `Store` struct) only while the store is open.
- Never written to disk in plaintext.

If the `meta` table or master key is lost, encrypted tokens cannot be recovered.
Back up the entire data directory if you need to preserve provider tokens.

### Key scope

The master key is local to a store. Copying the SQLite file to another machine
does **not** transfer the ability to decrypt tokens — the receiving machine has
no knowledge of the original master key. This is intentional: the portable
export/import flow re-encrypts tokens with the destination's master key under a
passphrase.

## Password hashing

### Algorithm

Profile passwords are hashed with **Argon2id**, the recommended Argon2 variant
for password hashing. Parameters used:

| Parameter       | Value            | Notes                              |
|-----------------|------------------|------------------------------------|
| Memory          | 64 MiB           | Relatively high; adjust for your threat model. |
| Iterations      | 3                |                                    |
| Parallelism     | 1                |                                    |
| Output length   | 32 bytes         |                                    |
| Salt           | 16 random bytes  | Generated per password.            |

The stored hash format is:

```
argon2id:<salt-hex>:<derived-key-hex>
```

### Verification

Verification re-derives the key from the recorded salt and compares in constant
time (bytewise XOR fold). The comparison is not a full constant-time library
call, but it avoids early-return string comparison and length-based leaks.

### Migration

Older stores may contain `sha256:`-prefixed marker hashes. These are still
accepted for verification so existing profiles are not locked out. New passwords
are always hashed with the `argon2id:` format.

## Bearer token (profile ID)

The profile `id` is an opaque hex-encoded 128-bit random value. It serves as:

1. The primary key for the profile in the store.
2. The bearer token presented by callers to the HTTP API.

Generation: 16 random bytes, hex-encoded, no padding. This gives 128 bits of
entropy, which is sufficient to prevent brute-force guessing.

The token is printed once at profile creation (or bootstrap) and never shown
again. Store it securely — if lost, rotate with `atos-proxy profile token rotate`.

## Token rotation

`atos-proxy profile token rotate <name>` generates a new profile ID and updates
all internal references. The old token stops working immediately.

Rotation does **not** change provider tokens — those are independent secrets.
Rotating the profile token only changes the bearer token used to authenticate
API calls.

## Rate limiting

Per-profile rate limiting is enforced in-memory with a sliding one-minute
window. When a profile has an `rpm_limit` set, the middleware:

1. Records a timestamp for each accepted request.
2. Evicts timestamps older than 60 seconds.
3. Refuses a request (HTTP 429) once the count reaches the limit.

The limiter is per-process. If you run multiple proxy instances, each has its
own independent window — they do not coordinate. For coordinated rate limiting
across instances, place a rate-limiting front-end proxy in front of AGOS Proxy.

## Secrets in logs and error messages

- Provider tokens are never logged.
- Profile tokens are printed once at creation/bootstrap; avoid capturing that
  output in logs.
- Error responses from upstream providers are returned to the caller as-is
  (so the caller can see *why* a provider failed), but the proxy does not add
  its own sensitive data to error bodies.

## Operational guidance

### Running on localhost only

For development and single-machine use, binding to `127.0.0.1` (the default) is
appropriate. No further network isolation is needed beyond the host's firewall.

```sh
atos-proxy serve --bind 127.0.0.1:3000
```

### Running on a network

When exposing the proxy beyond localhost:

1. Put it behind a TLS-terminating reverse proxy.
2. Restrict the reverse proxy to known clients (VPN, IP allow-list, authentication).
3. Use profile passwords for any profile that is managed interactively.
4. Rotate profile tokens if they may have been exposed.
5. Monitor the usage log for unexpected activity.

Example with Caddy (reverse proxy + TLS + local auth):

```caddy
api.example.com {
    reverse_proxy localhost:8080
    tls internal
    basicauth /* {
        bob $2a$14$...
    }
}
```

### Backing up

Back up the entire data directory (`AGOS_HOME`). The SQLite file is the only
persisted state. For portable, cross-key backup, use `atos-proxy config export`
with a passphrase — this produces a self-contained, sealed file that can be
restored on any machine.

### Rotating provider tokens

If an upstream provider token is compromised:

1. Update the provider in AGOS Proxy with the new token:
   `atos-proxy provider edit --profile <name>`
2. Revoke the old token at the provider (if the provider supports it).
3. Check the usage log for any suspicious requests made with the old token.

### Rotating profile tokens

If a profile bearer token is compromised:

```sh
atos-proxy profile token rotate <profile-name>
```

Update any clients that use the old token with the new one (printed to stdout).

### Deleting a compromised profile

If a profile is fully compromised and you cannot rotate safely:

```sh
atos-proxy profile delete --name <profile-name>
```

This removes the profile, all its providers, proxies, routes, and usage log.
Confirm carefully — it is irreversible.

## Security-related CLI commands

| Command                                          | Purpose                                   |
|--------------------------------------------------|-------------------------------------------|
| `atos-proxy profile create`                      | Create a profile (optionally password-protected). |
| `atos-proxy profile token rotate`                | Generate a new bearer token for a profile.|
| `atos-proxy profile edit`                        | Change name, description, password.       |
| `atos-proxy profile limit`                       | Set or view the RPM limit.                |
| `atos-proxy profile delete`                      | Remove a profile and all its data.        |
| `atos-proxy config export`                       | Seal a profile tree to a passphrase file. |
| `atos-proxy config import`                       | Restore from a sealed file.               |

## Reporting a vulnerability

See `CONTRIBUTING.md` for the security reporting process. Do not open a public
issue for security vulnerabilities.
