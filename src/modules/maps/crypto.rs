//! Map & Tagging — encryption for tile-provider browser API keys.
//!
//! The provider API key a tenant stores (Google Maps browser
//! key) is encrypted with the same `v1.` format the email
//! providers module uses for OAuth secrets, so the two stay
//! interoperable: HMAC-SHA256 keystream XOR with an HMAC tag,
//! NOT an AEAD cipher. The key is `app.secret_key`, captured
//! once by the module `init` from the app config. There is no
//! fallback: when no real secret is configured, storing a key
//! is refused and resolving a provider that needs one fails
//! closed, instead of encrypting with a hard-coded secret
//! anyone holding the repo could reproduce.
//!
//! The stored value is ciphertext only (`v1.<base64url(nonce ||
//! ciphertext || tag)>`). The plaintext key is a
//! referrer-restricted browser key by design: it is only ever
//! substituted into a tile URL served to authenticated tenant
//! users with `maps.view`, and is never logged or returned by
//! any settings endpoint. Store only keys restricted by HTTP
//! referrer to this deployment.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::RwLock;
use unified_core::error::{UnifiedError, UnifiedResult};

type HmacSha256 = Hmac<Sha256>;

/// The encryption secret, set once by the module `init` from
/// `app.secret_key`. A plain `RwLock` (not `OnceLock`) so tests
/// can set and reset it; production sets it once at startup.
static ENCRYPTION_SECRET: RwLock<Option<String>> = RwLock::new(None);

/// Capture the encryption secret at module init. An empty secret
/// is stored as "not configured" so every later use fails closed
/// with a clear error instead of encrypting with an empty key.
pub(crate) fn set_encryption_secret(secret: &str) {
    let trimmed = secret.trim();
    let mut slot = ENCRYPTION_SECRET
        .write()
        .expect("maps crypto secret lock poisoned");
    *slot = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
}

/// The configured secret, or a fail-closed error naming the
/// missing configuration. The secret value never appears in the
/// error.
fn encryption_secret() -> UnifiedResult<String> {
    ENCRYPTION_SECRET
        .read()
        .expect("maps crypto secret lock poisoned")
        .clone()
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| {
            UnifiedError::internal(
                "app.secret_key is not configured; provider API keys cannot be stored or decrypted until a real secret is configured",
            )
        })
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// Keystream for the XOR stream cipher: HMAC-SHA256 blocks keyed
/// by the SHA-256 of the secret, over `nonce || counter`.
fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut message = nonce.to_vec();
        message.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hmac_sha256(key, &message));
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Constant-time equality for authentication tags.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt a provider API key with the configured key. Output
/// format: `v1.<base64url(nonce || ciphertext || tag)>`. The tag
/// covers nonce + ciphertext. Refuses (fail closed) when no
/// secret is configured.
pub(crate) fn encrypt_secret(plaintext: &str) -> UnifiedResult<String> {
    let secret = encryption_secret()?;
    let key = Sha256::digest(secret.as_bytes());
    let nonce: [u8; 16] = rand::random();
    let stream = keystream(&key, &nonce, plaintext.len());
    let ciphertext: Vec<u8> = plaintext
        .as_bytes()
        .iter()
        .zip(stream.iter())
        .map(|(plain, mask)| plain ^ mask)
        .collect();
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&key, &tag_message);
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    Ok(format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    ))
}

/// Decrypt a `v1.` value with the configured key. Any failure —
/// unknown format, undecodable blob, bad tag, non-UTF-8
/// plaintext — is an internal error that names the failure,
/// never the secret. A value that is not in `v1.` format is
/// refused outright (it was never encrypted by this module).
pub(crate) fn decrypt_secret(stored: &str) -> UnifiedResult<String> {
    let secret = encryption_secret()?;
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored provider API key has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored provider API key cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored provider API key cannot be decrypted",
        ));
    }
    let (nonce, rest) = blob.split_at(16);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let key = Sha256::digest(secret.as_bytes());
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(ciphertext);
    let expected = hmac_sha256(&key, &tag_message);
    if !constant_time_eq(&expected, tag) {
        return Err(UnifiedError::internal(
            "Stored provider API key failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored provider API key cannot be decrypted"))
}

/// Decrypt a stored provider API key, or `Ok(None)` when the
/// tenant holds no value. `Some("")` (an empty stored value) is
/// treated as no value, matching the write path's
/// empty-means-clear.
pub(crate) fn decrypt_optional(stored: &Option<String>) -> UnifiedResult<Option<String>> {
    match stored.as_deref().filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => decrypt_secret(value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encryption secret is process-global: tests that set it
    /// must not run concurrently, or one test's secret would break
    /// another's roundtrip mid-flight.
    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn roundtrip_keeps_the_plaintext_out_of_the_ciphertext() {
        let _guard = TEST_MUTEX.lock().unwrap();
        set_encryption_secret("test-secret-key-for-maps");
        let plaintext = "test-google-browser-key";
        let stored = encrypt_secret(plaintext).expect("encrypt with a configured secret");
        assert!(stored.starts_with("v1."));
        assert!(!stored.contains(plaintext));
        assert_eq!(decrypt_secret(&stored).unwrap(), plaintext);
    }

    #[test]
    fn encrypt_refuses_without_a_configured_secret() {
        let _guard = TEST_MUTEX.lock().unwrap();
        set_encryption_secret("");
        assert!(encrypt_secret("key").is_err());
        set_encryption_secret("test-secret-key-for-maps");
    }
}
