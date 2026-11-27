//! ai module - Key encryption, HMAC, hashing and fingerprints.
//!
//! Follow-up (not done here): migrate the custom XOR stream cipher
//! (`encrypt_api_key` / `decrypt_api_key`, `v1.` format) to an AEAD
//! cipher (for example XChaCha20-Poly1305 or AES-GCM). The cipher
//! is deliberately unchanged here: switching it would break every
//! stored ciphertext, so the migration must bump the version
//! prefix and re-encrypt stored keys on next write.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// The encryption secret: `app.secret_key`. There is no fallback:
/// when no real secret is configured, storing a provider key is
/// refused and any call needing a stored key fails closed, instead
/// of encrypting with a hard-coded secret anyone holding the repo
/// could reproduce. Same fail-closed rule as the holidays module.
pub(crate) fn encryption_secret(ctx: &AppContext) -> UnifiedResult<String> {
    let config = ctx.config.read().expect("config lock poisoned");
    let secret = config.app.secret_key.trim().to_string();
    if secret.is_empty() {
        return Err(UnifiedError::internal(
            "app.secret_key is not configured; provider API keys cannot be stored or decrypted until a real secret is configured",
        ));
    }
    Ok(secret)
}

// Credential encryption (write-only storage).

pub(crate) type HmacSha256 = Hmac<Sha256>;

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

pub(crate) fn sha256_hex_of(data: &[u8]) -> String {
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = Sha256::digest(data);
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// Keystream for the XOR stream cipher: HMAC-SHA256 blocks keyed by
/// the SHA-256 of the secret, over `nonce || counter`.
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

/// Constant-time equality for authentication tags.
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

/// Encrypt an API key. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
pub(crate) fn encrypt_api_key(secret: &str, plaintext: &str) -> String {
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

/// Decrypt a value produced by `encrypt_api_key`. The error message
/// never includes any part of the stored value.
pub(crate) fn decrypt_api_key(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored provider key has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored provider key cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored provider key cannot be decrypted",
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
            "Stored provider key failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored provider key cannot be decrypted"))
}

/// Short fingerprint prefix of a key, for the masked read view. Only
/// the first 12 hex characters of the SHA-256 are exposed, never the
/// key itself.
pub(crate) fn key_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes())
        .chars()
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;
    use super::decrypt_api_key;
    use super::encrypt_api_key;
    use super::encryption_secret;
    use super::hmac_sha256;
    use super::key_fingerprint;
    use super::keystream;
    use super::sha256_hex_of;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use std::sync::Arc;

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
        // AppConfig::default() ships with an empty secret_key: no
        // hard-coded fallback may stand in for it.
        assert!(encryption_secret(&ctx).is_err());
        ctx.config
            .write()
            .expect("config lock poisoned")
            .app
            .secret_key = "a-real-configured-secret".to_string();
        assert_eq!(encryption_secret(&ctx).unwrap(), "a-real-configured-secret");
    }

    #[test]
    fn hmac_matches_rfc_vector() {
        let tag = hmac_sha256(&[0x0b; 20], b"Hi There");
        let hex: String = {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::with_capacity(tag.len() * 2);
            for &byte in tag.iter() {
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
            out
        };
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn keystream_is_deterministic_and_sized() {
        let first = keystream(&[1u8; 32], &[2u8; 16], 100);
        assert_eq!(first.len(), 100);
        assert_eq!(first, keystream(&[1u8; 32], &[2u8; 16], 100));
        assert_ne!(first, keystream(&[1u8; 32], &[3u8; 16], 100));
    }

    #[test]
    fn constant_time_eq_cases() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn key_fingerprint_is_12_hex_and_stable() {
        let first = key_fingerprint("sk-some-key");
        assert_eq!(first.len(), 12);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(first, key_fingerprint("sk-some-key"));
        assert_ne!(first, key_fingerprint("sk-other-key"));
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let stored = encrypt_api_key("test-secret", "sk-secret-value");
        assert!(stored.starts_with("v1."));
        assert_eq!(
            decrypt_api_key("test-secret", &stored).unwrap(),
            "sk-secret-value"
        );
    }

    #[test]
    fn decrypt_rejects_wrong_secret_and_garbage() {
        let stored = encrypt_api_key("test-secret", "sk-secret-value");
        assert!(decrypt_api_key("other-secret", &stored).is_err());
        assert!(decrypt_api_key("test-secret", "not-a-stored-value").is_err());
    }

    #[test]
    fn decrypt_rejects_tampered_value() {
        let stored = encrypt_api_key("test-secret", "sk-secret-value");
        let mut tampered = stored.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(decrypt_api_key("test-secret", &tampered).is_err());
    }

    #[test]
    fn encrypt_is_randomized() {
        let first = encrypt_api_key("test-secret", "same-value");
        let second = encrypt_api_key("test-secret", "same-value");
        assert_ne!(first, second);
    }
}
