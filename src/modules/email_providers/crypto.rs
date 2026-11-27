//! email_providers - credential encryption.
//!
//! The OAuth secrets this module stores (SMTP password, OAuth client
//! secret, access / refresh tokens) are encrypted with the same `v1.`
//! format the holidays and AI modules use, so the three stay
//! interoperable: HMAC-SHA256 keystream XOR with an HMAC tag, NOT an
//! AEAD cipher. The key is `app.secret_key`, captured once by the
//! module `init` from the app config. There is no fallback: when no
//! real secret is configured, storing an OAuth secret is refused and
//! any adapter needing one fails closed, instead of encrypting with a
//! hard-coded secret anyone holding the repo could reproduce.
//!
//! Scope note: the pre-existing `api_key` / `access_key_id` /
//! `secret_access_key` fields of the 9 API providers are NOT
//! re-encrypted here - they keep their original write-only treatment
//! (never serialized, never logged). Only the secrets added by task
//! 350 go through this module. Re-encrypting the old fields would
//! change every API adapter's credential read path; that is a
//! separate migration, not this task.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::RwLock;
use unified_core::error::{UnifiedError, UnifiedResult};

type HmacSha256 = Hmac<Sha256>;

/// The encryption secret, set once by the module `init` from
/// `app.secret_key`. A plain `RwLock` (not `OnceLock`) so tests can
/// set and reset it; production sets it once at startup.
static ENCRYPTION_SECRET: RwLock<Option<String>> = RwLock::new(None);

/// Capture the encryption secret at module init. An empty secret is
/// stored as "not configured" so every later use fails closed with
/// a clear error instead of encrypting with an empty key.
pub(crate) fn set_encryption_secret(secret: &str) {
    let trimmed = secret.trim();
    let mut slot = ENCRYPTION_SECRET
        .write()
        .expect("email crypto secret lock poisoned");
    *slot = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
}

/// The configured secret, or a fail-closed error naming the missing
/// configuration. The secret value never appears in the error.
pub(crate) fn encryption_secret() -> UnifiedResult<String> {
    ENCRYPTION_SECRET
        .read()
        .expect("email crypto secret lock poisoned")
        .clone()
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| {
            UnifiedError::internal(
                "app.secret_key is not configured; OAuth secrets cannot be stored or decrypted until a real secret is configured",
            )
        })
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// Keystream for the XOR stream cipher: HMAC-SHA256 blocks keyed by
/// the SHA-256 of the secret, over `nonce || counter`.
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

/// True when a stored value is already in the encrypted `v1.` format.
pub(crate) fn is_encrypted(value: &str) -> bool {
    value.starts_with("v1.")
}

/// Encrypt a secret with the configured key. Output format:
/// `v1.<base64url(nonce || ciphertext || tag)>`. The tag covers
/// nonce + ciphertext. Refuses (fail closed) when no secret is
/// configured; the caller turns that into a 500 naming the missing
/// `app.secret_key`, never a plaintext store.
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

/// Decrypt a `v1.` value with the configured key. Any failure -
/// unknown format, undecodable blob, bad tag, non-UTF-8 plaintext -
/// is an internal error that names the failure, never the secret.
/// A value that is not in `v1.` format is refused outright (it was
/// never encrypted by this module).
pub(crate) fn decrypt_secret(stored: &str) -> UnifiedResult<String> {
    let secret = encryption_secret()?;
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored OAuth secret has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored OAuth secret cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored OAuth secret cannot be decrypted",
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
            "Stored OAuth secret failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored OAuth secret cannot be decrypted"))
}

/// Decrypt a stored OAuth secret, or `Ok(None)` when the connection
/// holds no value for the field. `Some("")` (an empty stored value)
/// is treated as no value, matching the write path's empty-means-clear.
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

    fn use_test_secret(secret: &str) {
        set_encryption_secret(secret);
    }

    #[test]
    fn roundtrip_keeps_the_plaintext_out_of_the_ciphertext() {
        let _guard = TEST_MUTEX.lock().unwrap();
        use_test_secret("test-secret-key-for-email-providers");
        let plaintext = "ya29.a0AfH6SMBx-secret-refresh-token-value";
        let stored = encrypt_secret(plaintext).expect("encrypt with a configured secret");
        assert!(stored.starts_with("v1."));
        assert!(!stored.contains(plaintext));
        assert_eq!(decrypt_secret(&stored).unwrap(), plaintext);
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let _guard = TEST_MUTEX.lock().unwrap();
        use_test_secret("test-secret-key-for-email-providers");
        let stored = encrypt_secret("client-secret-value").unwrap();
        let mut tampered = stored.clone();
        tampered.push('A');
        assert!(decrypt_secret(&tampered).is_err());
    }

    #[test]
    fn wrong_secret_fails_authentication() {
        let _guard = TEST_MUTEX.lock().unwrap();
        use_test_secret("first-secret");
        let stored = encrypt_secret("client-secret-value").unwrap();
        use_test_secret("different-secret");
        let err = decrypt_secret(&stored).unwrap_err();
        assert!(err.to_string().contains("authentication"));
    }

    #[test]
    fn unknown_format_is_refused() {
        let _guard = TEST_MUTEX.lock().unwrap();
        use_test_secret("test-secret-key-for-email-providers");
        let err = decrypt_secret("plain-text-not-encrypted").unwrap_err();
        assert!(err.to_string().contains("unknown format"));
    }

    #[test]
    fn missing_secret_fails_closed_on_both_sides() {
        let _guard = TEST_MUTEX.lock().unwrap();
        set_encryption_secret("");
        assert!(encrypt_secret("x").is_err());
        assert!(decrypt_secret("v1.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());
        assert!(encryption_secret().is_err());
    }

    #[test]
    fn decrypt_optional_handles_missing_and_empty() {
        let _guard = TEST_MUTEX.lock().unwrap();
        use_test_secret("test-secret-key-for-email-providers");
        assert_eq!(decrypt_optional(&None).unwrap(), None);
        assert_eq!(decrypt_optional(&Some(String::new())).unwrap(), None);
        let stored = encrypt_secret("token").unwrap();
        assert_eq!(
            decrypt_optional(&Some(stored)).unwrap(),
            Some("token".to_string())
        );
    }
}
