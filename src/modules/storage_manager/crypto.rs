// Credential encryption (write-only storage).

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::RwLock;

use super::*;

// ---------------------------------------------------------------------------
// Credential encryption (write-only storage).
//
// Same construction as the AI module (`encrypt_api_key`): a versioned
// `v1.` HMAC-SHA256 keystream (XOR stream cipher) with an HMAC tag
// over nonce + ciphertext. Kept as a local copy on purpose — see the
// module header note about a shared core helper.
// ---------------------------------------------------------------------------

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

/// The decryption secret, from a bare config handle, so hook
/// handlers (which have no `AppContext`) can decrypt bucket
/// credentials too. The handle is captured at `init` time.
///
/// Decrypt-only legacy path: existing rows encrypted under the
/// development fallback must stay readable, so decryption keeps
/// falling back. Encryption never uses this function — see
/// `encryption_secret_for_encrypt`, which fails closed.
pub(crate) fn encryption_secret_from(config: &RwLock<AppConfig>) -> String {
    let config = config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        log::warn!(
            "Storage Manager: decrypting credentials with the development fallback secret because app.secret_key is not configured; set a real app.secret_key"
        );
        DEV_ENCRYPTION_SECRET.to_string()
    } else {
        config.app.secret_key.clone()
    }
}

/// The secret to use when ENCRYPTING credentials. Fail-closed: when
/// no real `app.secret_key` is configured, encryption is refused
/// outside explicit development mode (`app.mode == "development"`),
/// where the public development fallback is used with a loud
/// warning. The fallback constant is in the source tree and visible
/// to anyone with repo access, so it must never silently protect
/// real credentials in production.
pub(crate) fn encryption_secret_for_encrypt(
    config: &RwLock<AppConfig>,
) -> UnifiedResult<String> {
    let config = config.read().expect("config lock poisoned");
    if !config.app.secret_key.trim().is_empty() {
        return Ok(config.app.secret_key.clone());
    }
    if config.app.mode == "development" {
        log::warn!(
            "Storage Manager: app.secret_key is not configured; encrypting credentials with the DEVELOPMENT fallback secret \"{DEV_ENCRYPTION_SECRET}\" (development mode only — never use this in production)"
        );
        return Ok(DEV_ENCRYPTION_SECRET.to_string());
    }
    Err(UnifiedError::internal(
        "Cannot encrypt storage credentials: app.secret_key is not configured (set app.secret_key, or run in development mode to use the development fallback)",
    ))
}

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

/// Encrypt a credential value. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
pub(crate) fn encrypt_secret_value(secret: &str, plaintext: &str) -> String {
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

/// Decrypt a value produced by `encrypt_secret_value`. The error
/// message never includes any part of the stored value.
pub(crate) fn decrypt_secret_value(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored credential has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored credential cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored credential cannot be decrypted",
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
            "Stored credential failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored credential cannot be decrypted"))
}

/// Short fingerprint prefix of a credential value, for masked views.
/// Only the first 12 hex characters of the SHA-256 are exposed.
/// Kept for backwards-compatible display only; new fingerprints
/// must use `ciphertext_fingerprint` (see below), because a hash
/// of the plaintext is brute-forceable for low-entropy secrets.
pub(crate) fn secret_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes()).chars().take(12).collect()
}

/// Fingerprint of an ENCRYPTED credential blob (`v1.…` ciphertext),
/// for masked views: the first 12 hex characters of the SHA-256 of
/// the stored ciphertext. Fingerprinting the ciphertext (which
/// carries a random nonce, so equal plaintexts give different
/// fingerprints) reveals nothing about the plaintext and cannot be
/// brute-forced against low-entropy secrets the way a plaintext
/// hash can. Use this for every new `key_fingerprint`.
pub(crate) fn ciphertext_fingerprint(stored_ciphertext: &str) -> String {
    sha256_hex_of(stored_ciphertext.as_bytes())
        .chars()
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_of_matches_known_vectors() {
        assert_eq!(
            sha256_hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_hex_of_is_64_lowercase_hex_chars() {
        let hex = sha256_hex_of(b"hello");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn constant_time_eq_compares_bytes_and_lengths() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn keystream_is_deterministic_and_exact_length() {
        let key = b"test-key";
        let nonce = b"0123456789abcdef";
        assert_eq!(keystream(key, nonce, 0).len(), 0);
        assert_eq!(keystream(key, nonce, 32).len(), 32);
        assert_eq!(keystream(key, nonce, 100), keystream(key, nonce, 100));
        assert_ne!(keystream(key, nonce, 32), keystream(key, b"fedcba9876543210", 32));
        assert_ne!(keystream(key, nonce, 32), keystream(b"other-key", nonce, 32));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        for plaintext in ["", "s3cr3t-value", "unicode: pässwörd ✓"] {
            let stored = encrypt_secret_value("my-secret", plaintext);
            assert!(stored.starts_with("v1."));
            assert_eq!(decrypt_secret_value("my-secret", &stored).unwrap(), plaintext);
        }
        // Random nonce: two encryptions of the same value differ.
        let a = encrypt_secret_value("my-secret", "same");
        let b = encrypt_secret_value("my-secret", "same");
        assert_ne!(a, b);
        assert_eq!(decrypt_secret_value("my-secret", &a).unwrap(), "same");
        assert_eq!(decrypt_secret_value("my-secret", &b).unwrap(), "same");
    }

    #[test]
    fn decrypt_rejects_wrong_secret_bad_format_and_tampering() {
        let stored = encrypt_secret_value("right-secret", "value");
        assert!(decrypt_secret_value("wrong-secret", &stored).is_err());
        assert!(decrypt_secret_value("right-secret", "no-prefix-here").is_err());
        assert!(decrypt_secret_value("right-secret", "v1.!!!").is_err());
        assert!(decrypt_secret_value("right-secret", "v1.QUJD").is_err());
        // Flip one base64 character: the tag no longer matches.
        let mut tampered = stored.clone().into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        assert!(decrypt_secret_value("right-secret", &String::from_utf8(tampered).unwrap()).is_err());
    }

    #[test]
    fn secret_fingerprint_is_12_hex_prefix_of_sha256() {
        let fp = secret_fingerprint("credential");
        assert_eq!(fp.len(), 12);
        assert_eq!(fp, &sha256_hex_of(b"credential")[..12]);
        assert_eq!(secret_fingerprint("credential"), fp);
        assert_ne!(secret_fingerprint("credential"), secret_fingerprint("other"));
    }

    #[test]
    fn encrypt_secret_refused_without_real_secret_outside_dev_mode() {
        // Default config: production mode, empty secret_key.
        let config = RwLock::new(AppConfig::default());
        assert_eq!(config.read().unwrap().app.mode, "production");
        assert!(encryption_secret_for_encrypt(&config).is_err());
        // A configured secret is returned as-is.
        let mut configured = AppConfig::default();
        configured.app.secret_key = "real-secret-key".to_string();
        let config = RwLock::new(configured);
        assert_eq!(
            encryption_secret_for_encrypt(&config).unwrap(),
            "real-secret-key"
        );
        // Explicit development mode may use the fallback (loudly).
        let mut dev = AppConfig::default();
        dev.app.mode = "development".to_string();
        let config = RwLock::new(dev);
        assert_eq!(
            encryption_secret_for_encrypt(&config).unwrap(),
            DEV_ENCRYPTION_SECRET
        );
    }

    #[test]
    fn ciphertext_fingerprint_hashes_the_stored_blob_not_the_plaintext() {
        let a = encrypt_secret_value("my-secret", "same-value");
        let b = encrypt_secret_value("my-secret", "same-value");
        // Same plaintext, different ciphertexts (random nonce).
        assert_ne!(a, b);
        let fp_a = ciphertext_fingerprint(&a);
        let fp_b = ciphertext_fingerprint(&b);
        assert_eq!(fp_a.len(), 12);
        assert_ne!(fp_a, fp_b);
        assert_eq!(fp_a, &sha256_hex_of(a.as_bytes())[..12]);
        // A plaintext hash would be identical for both — that is
        // exactly the brute-forceable leak this replaces.
        assert_eq!(secret_fingerprint("same-value"), secret_fingerprint("same-value"));
    }
    #[test]
    fn sha256_hex_of_matches_known_vectors() {
        assert_eq!(
            sha256_hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

}
