// Tax module - credential encryption (HMAC keystream, tags, fingerprints).
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Credential encryption (write-only storage)

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

/// The encryption secret: `app.secret_key`, or the development
/// fallback when none is configured.
pub(crate) fn encryption_secret(ctx: &AppContext) -> String {
    let config = ctx.config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        DEV_ENCRYPTION_SECRET.to_string()
    } else {
        config.app.secret_key.clone()
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

/// Encrypt a connector credential. Output format:
/// `v1.<base64url(nonce || ciphertext || tag)>`. The tag covers
/// nonce + ciphertext.
pub(crate) fn encrypt_tax_credential(secret: &str, plaintext: &str) -> String {
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

/// Decrypt a value produced by `encrypt_tax_credential`. The error
/// message never includes any part of the stored value.
pub(crate) fn decrypt_tax_credential(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored tax credential has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored tax credential cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored tax credential cannot be decrypted",
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
            "Stored tax credential failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored tax credential cannot be decrypted"))
}

/// Short fingerprint prefix of a credential, for the masked read
/// view. Only the first 12 hex characters of the SHA-256 are
/// exposed, never the credential itself.
pub(crate) fn credential_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes())
        .chars()
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let stored = encrypt_tax_credential("test-secret", "sk-live-123");
        assert!(stored.starts_with("v1."));
        assert_eq!(
            decrypt_tax_credential("test-secret", &stored).unwrap(),
            "sk-live-123"
        );
    }

    #[test]
    fn encrypt_uses_a_random_nonce() {
        let first = encrypt_tax_credential("secret", "same-value");
        let second = encrypt_tax_credential("secret", "same-value");
        assert_ne!(first, second);
        assert_eq!(
            decrypt_tax_credential("secret", &first).unwrap(),
            "same-value"
        );
        assert_eq!(
            decrypt_tax_credential("secret", &second).unwrap(),
            "same-value"
        );
    }

    #[test]
    fn decrypt_rejects_wrong_secret_and_bad_input() {
        let stored = encrypt_tax_credential("right-secret", "value");
        assert!(decrypt_tax_credential("wrong-secret", &stored).is_err());
        assert!(decrypt_tax_credential("right-secret", "plain-text").is_err());
        assert!(decrypt_tax_credential("right-secret", "v1.!!!not-base64!!!").is_err());
        assert!(decrypt_tax_credential("right-secret", "v1.AAAA").is_err());
    }

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn credential_fingerprint_is_a_stable_12_hex_prefix() {
        let fingerprint = credential_fingerprint("my-api-key");
        assert_eq!(fingerprint.len(), 12);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(fingerprint, credential_fingerprint("my-api-key"));
        assert_ne!(fingerprint, credential_fingerprint("another-key"));
        assert_eq!(fingerprint, &sha256_hex_of(b"my-api-key")[..12]);
    }

    #[test]
    fn keystream_is_deterministic_and_sized() {
        let first = keystream(b"key", b"nonce-1234567890", 64);
        let second = keystream(b"key", b"nonce-1234567890", 64);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert_ne!(first, keystream(b"other-key", b"nonce-1234567890", 64));
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
