//! Symmetric encryption helpers for stored provider
//! credentials and webhook secrets.
//!
//! The same `v1.` format as the holidays module (HMAC
//! keystream with an HMAC tag, NOT an AEAD cipher), copied
//! so the two modules stay interoperable: `v1.` plus
//! base64url of `nonce || ciphertext || tag`, where the tag
//! covers nonce + ciphertext. The secret is
//! `app.secret_key` from the application config; there is no
//! fallback, so storing a credential with no configured
//! secret is refused instead of encrypting with a
//! hard-coded secret anyone holding the repo could
//! reproduce.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// SHA-256 hex of `data`, used for webhook-secret hashes
/// and credential fingerprints.
pub(crate) fn sha256_hex_of(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(digest.len() * 2);
    for &byte in digest.iter() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// The encryption secret: `app.secret_key`. There is no
/// fallback: when no real secret is configured, storing a
/// provider credential is refused and any send needing a
/// stored credential fails closed.
pub(crate) fn encryption_secret(ctx: &AppContext) -> UnifiedResult<String> {
    let config = ctx.config.read().expect("config lock poisoned");
    let secret = config.app.secret_key.trim().to_string();
    if secret.is_empty() {
        return Err(UnifiedError::internal(
            "app.secret_key is not configured; provider credentials cannot be stored or decrypted until a real secret is configured",
        ));
    }
    Ok(secret)
}

/// Keystream for the XOR stream cipher: HMAC-SHA256 blocks
/// keyed by the SHA-256 of the secret, over
/// `nonce || counter`.
pub(crate) fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
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

/// Constant-time equality for authentication tags and
/// webhook-secret hashes.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt a provider credential. Output format:
/// `v1.<base64url(nonce || ciphertext || tag)>`. The tag
/// covers nonce + ciphertext.
pub(crate) fn encrypt_credential(secret: &str, plaintext: &str) -> String {
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
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    )
}

/// Decrypt a value produced by `encrypt_credential`. The
/// error message never includes any part of the stored
/// value.
pub(crate) fn decrypt_credential(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored.strip_prefix("v1.").ok_or_else(|| {
        UnifiedError::internal("Stored provider credential has an unknown format")
    })?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored provider credential cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored provider credential cannot be decrypted",
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
            "Stored provider credential failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored provider credential cannot be decrypted"))
}

/// Short fingerprint prefix of a credential, for the masked
/// read view. Only the first 12 hex characters of the
/// SHA-256 are exposed, never the credential itself.
pub(crate) fn credential_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes())
        .chars()
        .take(12)
        .collect()
}

/// Hash a webhook secret for storage: SHA-256 hex. The raw
/// secret is returned exactly once at creation / rotation
/// and never stored.
pub(crate) fn hash_webhook_secret(secret: &str) -> String {
    sha256_hex_of(secret.as_bytes())
}

/// True when a presented webhook secret matches the stored
/// hash. Comparison is constant-time; a missing stored hash
/// never matches.
pub(crate) fn webhook_secret_matches(stored_hash: Option<&str>, presented: &str) -> bool {
    match stored_hash {
        Some(hash) => constant_time_eq(
            hash.as_bytes(),
            sha256_hex_of(presented.as_bytes()).as_bytes(),
        ),
        None => false,
    }
}

/// Generate a fresh webhook secret: 48 random alphanumeric
/// characters, the same shape as the referral module's
/// token pattern. The caller returns it once and stores
/// only its hash.
pub(crate) fn generate_webhook_secret() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let bytes: Vec<u8> = (0..48)
        .map(|_| {
            let index: usize = rand::random::<u8>() as usize % ALPHABET.len();
            ALPHABET[index]
        })
        .collect();
    String::from_utf8(bytes).expect("alphabet is ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn encryption_secret_refuses_an_unconfigured_secret() {
        let ctx = test_context();
        // AppConfig::default() ships with an empty secret_key:
        // no hard-coded fallback may stand in for it.
        assert!(encryption_secret(&ctx).is_err());
        ctx.config
            .write()
            .expect("config lock poisoned")
            .app
            .secret_key = "a-real-configured-secret".to_string();
        assert_eq!(encryption_secret(&ctx).unwrap(), "a-real-configured-secret");
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let stored = encrypt_credential("test-secret-key", "sk-secret-123");
        assert_eq!(
            decrypt_credential("test-secret-key", &stored).unwrap(),
            "sk-secret-123"
        );
    }

    #[test]
    fn decrypt_rejects_unknown_format_and_wrong_secret() {
        assert!(decrypt_credential("secret", "garbage-without-prefix").is_err());
        assert!(decrypt_credential("secret", "").is_err());
        let stored = encrypt_credential("right-secret", "plaintext");
        match decrypt_credential("wrong-secret", &stored) {
            Ok(text) => assert_ne!(text, "plaintext"),
            Err(_) => {}
        }
    }

    #[test]
    fn encrypt_output_has_v1_prefix_and_differs_from_plaintext() {
        let first = encrypt_credential("secret", "plaintext");
        let second = encrypt_credential("secret", "plaintext");
        assert!(first.starts_with("v1."));
        assert_ne!(first, "plaintext");
        // Random nonce: two encryptions of the same value differ.
        assert_ne!(first, second);
    }

    #[test]
    fn credential_fingerprint_is_12_hex_chars_and_deterministic() {
        let first = credential_fingerprint("some-api-key");
        let second = credential_fingerprint("some-api-key");
        assert_eq!(first, second);
        assert_eq!(first.len(), 12);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, credential_fingerprint("another-api-key"));
    }

    #[test]
    fn webhook_secret_hash_and_match() {
        let secret = generate_webhook_secret();
        assert_eq!(secret.len(), 48);
        assert!(secret.bytes().all(|b| b.is_ascii_alphanumeric()));
        let hash = hash_webhook_secret(&secret);
        assert_eq!(hash.len(), 64);
        assert!(webhook_secret_matches(Some(&hash), &secret));
        assert!(!webhook_secret_matches(Some(&hash), "wrong-secret"));
        assert!(!webhook_secret_matches(None, &secret));
        assert_ne!(generate_webhook_secret(), secret);
    }

    #[test]
    fn constant_time_eq_cases() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
