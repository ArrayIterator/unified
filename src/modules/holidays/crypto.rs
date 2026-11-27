//! Symmetric encryption helpers for stored secrets.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// Credential encryption (write-only storage) - same `v1.` format as
// ai.rs, copied so the two modules stay interoperable. See the module
// header: HMAC keystream with an HMAC tag, NOT an AEAD cipher.

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex_of(data: &[u8]) -> String {
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

/// The encryption secret: `app.secret_key`. There is no fallback:
/// when no real secret is configured, storing a provider key is
/// refused and any sync needing a stored key fails closed, instead
/// of encrypting with a hard-coded secret anyone holding the repo
/// could reproduce.
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
    sha256_hex_of(plaintext.as_bytes()).chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // AppConfig::default() ships with an empty secret_key: no
        // hard-coded fallback may stand in for it.
        assert!(encryption_secret(&ctx).is_err());
        ctx.config
            .write()
            .expect("config lock poisoned")
            .app
            .secret_key = "a-real-configured-secret".to_string();
        assert_eq!(
            encryption_secret(&ctx).unwrap(),
            "a-real-configured-secret"
        );
    }

    #[test]
    fn sha256_hex_of_known_vectors() {
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
    fn sha256_hex_of_is_64_lowercase_hex() {
        let hex = sha256_hex_of(b"hello");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hex, hex.to_lowercase());
    }

    #[test]
    fn constant_time_eq_equal_and_unequal() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn key_fingerprint_is_12_hex_chars_and_deterministic() {
        let first = key_fingerprint("some-api-key");
        let second = key_fingerprint("some-api-key");
        assert_eq!(first, second);
        assert_eq!(first.len(), 12);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, key_fingerprint("another-api-key"));
        assert_eq!(first, &sha256_hex_of(b"some-api-key")[..12]);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let stored = encrypt_api_key("test-secret-key", "sk-secret-123");
        assert_eq!(decrypt_api_key("test-secret-key", &stored).unwrap(), "sk-secret-123");
    }

    #[test]
    fn decrypt_rejects_unknown_format() {
        assert!(decrypt_api_key("secret", "garbage-without-prefix").is_err());
        assert!(decrypt_api_key("secret", "").is_err());
    }

    #[test]
    fn encrypt_output_has_v1_prefix_and_differs_from_plaintext() {
        let first = encrypt_api_key("secret", "plaintext");
        let second = encrypt_api_key("secret", "plaintext");
        assert!(first.starts_with("v1."));
        assert_ne!(first, "plaintext");
        // Random nonce: two encryptions of the same value differ.
        assert_ne!(first, second);
        // A wrong secret never yields the original plaintext back.
        match decrypt_api_key("wrong-secret", &first) {
            Ok(text) => assert_ne!(text, "plaintext"),
            Err(_) => {}
        }
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
