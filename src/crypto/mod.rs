//! Handling of secrets at rest.
//!
//! Provider tokens are encrypted before they reach disk, keyed off a local
//! master key that lives only in memory while the store is open. The master
//! key is randomly generated on first use and stashed in the store's `meta`
//! table so restarts don't lose it. Keeping this in its own module means
//! token handling stays in one place and is easy to audit and test in isolation.

use aead::generic_array::GenericArray;
use aead::{Aead, KeyInit};
use anyhow::Result;
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;

/// Size of the master key in bytes (256 bits).
pub const MASTER_KEY_LEN: usize = 32;

/// Size of the nonce in bytes (96 bits for ChaCha20-Poly1305).
const NONCE_LEN: usize = 12;

/// A 256-bit symmetric key, held only in memory.
#[derive(Clone)]
pub struct MasterKey {
    bytes: [u8; MASTER_KEY_LEN],
}

impl MasterKey {
    /// Generate a fresh random master key.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; MASTER_KEY_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        Ok(Self { bytes })
    }

    /// Reconstruct a master key from raw bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; MASTER_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("master key must be {MASTER_KEY_LEN} bytes"))?;
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.bytes
    }
}

/// Encrypt `plaintext` with `key`, returning `nonce || ciphertext`.
pub fn encrypt(key: &MasterKey, plaintext: &str) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key.as_bytes()));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt `nonce || ciphertext` with `key`, returning the original plaintext.
pub fn decrypt(key: &MasterKey, ciphertext: &[u8]) -> Result<String> {
    if ciphertext.len() < NONCE_LEN {
        anyhow::bail!("ciphertext too short");
    }
    let (nonce_bytes, ct) = ciphertext.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key.as_bytes()));
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ct)
        .map_err(|_| anyhow::anyhow!("decryption failed (wrong key or corrupted data)"))?;
    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("invalid utf-8: {e}"))
}

/// Derive a 256-bit key from a password using Argon2id.
pub fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Version};
    let mut out = [0u8; 32];
    let argon2 = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        argon2::Params::new(64 * 1024, 3, 1, Some(32)).unwrap(),
    );
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow::anyhow!("argon2 key derivation failed: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = MasterKey::generate().unwrap();
        let token = "sk-supersecret-12345";
        let ct = encrypt(&key, token).unwrap();
        assert_ne!(ct, token.as_bytes());
        let pt = decrypt(&key, &ct).unwrap();
        assert_eq!(pt, token);
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key1 = MasterKey::generate().unwrap();
        let key2 = MasterKey::generate().unwrap();
        let ct = encrypt(&key1, "secret").unwrap();
        assert!(decrypt(&key2, &ct).is_err());
    }

    #[test]
    fn derive_key_is_deterministic_with_same_salt() {
        let salt = b"fixedsalt99"; // 12 bytes (>= 8 required by argon2)
        let k1 = derive_key("password", salt).unwrap();
        let k2 = derive_key("password", salt).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn derive_key_differs_with_different_salt() {
        let k1 = derive_key("password", b"saltsaltsalt1").unwrap();
        let k2 = derive_key("password", b"saltsaltsalt2").unwrap();
        assert_ne!(k1, k2);
    }
}
