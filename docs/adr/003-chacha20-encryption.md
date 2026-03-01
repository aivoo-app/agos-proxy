# ADR 003: Encryption Algorithm Choices

> Status: Accepted

## Context

AGOS Proxy stores provider API keys (secrets that unlock real spending) in a local SQLite file. These must be encrypted at rest so that an attacker who gains access to the file cannot read the keys.

We need:
- **Encryption at rest** for provider tokens
- **Password hashing** for profile passwords
- **Key derivation** for sealing portable config files

## Decision

We use:
- **ChaCha20-Poly1305** (AEAD) for encrypting provider tokens at rest
- **Argon2id** for hashing profile passwords and deriving keys from passphrases

## Consequences

### ChaCha20-Poly1305 for Token Encryption

**Why not AES-GCM?**
- ChaCha20-Poly1305 is faster on platforms without AES hardware acceleration (e.g., ARM, some VMs).
- It's immune to timing attacks that can affect AES implementations.
- The `chacha20poly1305` crate is well-audited and part of the RustCrypto ecosystem.

**How it works:**
1. A 256-bit master key is randomly generated on first store creation.
2. For each token encryption, a random 96-bit nonce is generated.
3. The token is encrypted with ChaCha20-Poly1305 using the master key and nonce.
4. The result is stored as `nonce || ciphertext` in the `providers.auth_token` column.
5. The master key is stored in the `meta` table (itself not exposed through any CLI command).

**Nonce management:** Each encryption uses a randomly generated nonce. With a 96-bit nonce and 256-bit key, the probability of collision is negligible for any realistic store size.

### Argon2id for Password Hashing

**Why Argon2id?**
- Argon2 is the winner of the Password Hashing Competition (PHC).
- Argon2id is a hybrid of Argon2i (side-channel resistant) and Argon2d (GPU-resistant).
- It's the recommended choice by OWASP for password hashing.

**Parameters:**
- Memory: 64 MB
- Iterations: 3
- Parallelism: 1
- Output: 32 bytes
- Salt: 16 bytes, randomly generated per password

**Stored format:** `argon2id:<salt-hex>:<derived-key-hex>`

**Backward compatibility:** Older stores may contain `sha256:`-prefixed hashes (a marker format used before the Argon2id milestone). These are still verified for backward compatibility.

## Alternatives Considered

| Algorithm | Use Case | Why Not Chosen |
|-----------|----------|----------------|
| AES-256-GCM | Token encryption | Slower on ARM; timing attack surface |
| bcrypt | Password hashing | Lower memory hardness than Argon2 |
| scrypt | Password hashing | Less flexible than Argon2 |
| PBKDF2 | Password hashing | Not memory-hard; vulnerable to GPU attacks |
| SHA-256 | Password hashing | Fast to brute-force; no memory hardness |

## Security Boundaries

- **The host is trusted.** If an attacker has root, they can read the master key from memory.
- **The network is not trusted.** Use a TLS-terminating reverse proxy.
- **The store file is encrypted at rest.** Provider tokens are never stored in plaintext.
- **The master key is local to a store.** Copying the SQLite file to another machine does not transfer the ability to decrypt tokens.
