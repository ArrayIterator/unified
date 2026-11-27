// Webhooks - Signing.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// Signing.

/// HMAC-SHA256 signature over `{timestamp}.{body}`, hex encoded.
/// Returns the header value `sha256=<hex>`.
pub fn sign_payload(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(format!("{timestamp}.").as_bytes());
    mac.update(body);
    let hex: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    format!("sha256={hex}")
}

/// Verify a received signature with a constant-time comparison.
pub fn verify_signature(secret: &str, timestamp: i64, body: &[u8], signature: &str) -> bool {
    let expected = sign_payload(secret, timestamp, body);
    if expected.len() != signature.len() {
        return false;
    }
    expected
        .bytes()
        .zip(signature.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

pub(crate) fn payload_hash(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_payload_is_deterministic() {
        let first = sign_payload("secret", 1_700_000_000, b"hello");
        let second = sign_payload("secret", 1_700_000_000, b"hello");
        assert_eq!(first, second);
    }

    #[test]
    fn sign_payload_has_header_format() {
        let signature = sign_payload("secret", 1_700_000_000, b"hello");
        assert!(signature.starts_with("sha256="));
        assert_eq!(signature.len(), 7 + 64);
    }

    #[test]
    fn sign_payload_changes_with_secret() {
        let a = sign_payload("secret-a", 1_700_000_000, b"hello");
        let b = sign_payload("secret-b", 1_700_000_000, b"hello");
        assert_ne!(a, b);
    }

    #[test]
    fn sign_payload_changes_with_body() {
        let a = sign_payload("secret", 1_700_000_000, b"hello");
        let b = sign_payload("secret", 1_700_000_000, b"goodbye");
        assert_ne!(a, b);
    }

    #[test]
    fn sign_payload_changes_with_timestamp() {
        let a = sign_payload("secret", 1_700_000_000, b"hello");
        let b = sign_payload("secret", 1_700_000_001, b"hello");
        assert_ne!(a, b);
    }

    #[test]
    fn verify_signature_accepts_correct_signature() {
        let signature = sign_payload("secret", 1_700_000_000, b"hello");
        assert!(verify_signature("secret", 1_700_000_000, b"hello", &signature));
    }

    #[test]
    fn verify_signature_rejects_wrong_secret() {
        let signature = sign_payload("secret", 1_700_000_000, b"hello");
        assert!(!verify_signature("other", 1_700_000_000, b"hello", &signature));
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let signature = sign_payload("secret", 1_700_000_000, b"hello");
        assert!(!verify_signature("secret", 1_700_000_000, b"hellx", &signature));
    }
    #[test]
    fn payload_hash_matches_known_vectors() {
        assert_eq!(
            payload_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            payload_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

}
