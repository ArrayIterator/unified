//! Licensing - License key generation and checksum validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

/// HMAC-SHA256, the same construction the VCS module uses for its
/// GitHub webhook signatures and the store module uses for its signed
/// download links.
pub(crate) type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Key generation: XXXX-XXXX-XXXX-XXXX with a checksum character.
// ---------------------------------------------------------------------------

/// Alphabet without ambiguous characters (no 0/O, 1/I/L).
pub(crate) const KEY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Generate a new license key. The 16th character is a checksum: the
/// alphabet index of the sum of the indices of the first 15 characters
/// modulo the alphabet size.
///
/// Honest strength note: this is a plain index-sum modulo 32, NOT a
/// weighted checksum. Any single-character change that preserves the
/// index sum (for example raising one character by 1 and lowering
/// another by 1, modulo 32) still passes the checksum, and a random
/// 16-character string passes with probability 1 in 32. The checksum
/// only catches typos and malformed keys; it is NOT a forgery
/// defence. What stops a forged key is the database lookup in
/// validation: a checksum-valid key that was never issued fails with
/// `not_found`. Do not "fix" this by switching to a weighted
/// checksum: that would invalidate every key already issued.
pub fn generate_license_key() -> String {
    let mut rng = rand::rng();
    let mut indices: Vec<usize> = (0..15)
        .map(|_| rng.random_range(0..KEY_ALPHABET.len()))
        .collect();
    let checksum = indices.iter().sum::<usize>() % KEY_ALPHABET.len();
    indices.push(checksum);
    let chars: Vec<char> = indices
        .iter()
        .map(|index| KEY_ALPHABET[*index] as char)
        .collect();
    chars
        .chunks(4)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join("-")
}

/// True when the key has the right shape and a matching checksum.
///
/// See `generate_license_key` for the honest strength note: this is
/// a plain index-sum modulo 32, so a tampered key that preserves
/// the sum still passes. Validation additionally requires the key
/// to exist in the database, which is what stops forgeries.
pub fn key_checksum_valid(key: &str) -> bool {
    let compact: String = key.chars().filter(|c| *c != '-').collect();
    if compact.len() != 16 {
        return false;
    }
    let mut indices = Vec::with_capacity(16);
    for ch in compact.chars() {
        match KEY_ALPHABET.iter().position(|byte| *byte as char == ch) {
            Some(index) => indices.push(index),
            None => return false,
        }
    }
    indices[..15].iter().sum::<usize>() % KEY_ALPHABET.len() == indices[15]
}

pub(crate) fn normalize_key(key: &str) -> String {
    key.trim().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_has_checksum_shape() {
        let key = generate_license_key();
        assert_eq!(key.len(), 19);
        assert_eq!(&key[4..5], "-");
        assert_eq!(&key[9..10], "-");
        assert_eq!(&key[14..15], "-");
        for ch in key.chars().filter(|c| *c != '-') {
            assert!(KEY_ALPHABET.contains(&(ch as u8)), "unexpected char {ch}");
        }
        assert!(key_checksum_valid(&key));
    }

    #[test]
    fn generated_keys_vary() {
        let first = generate_license_key();
        let second = generate_license_key();
        assert_ne!(first, second);
    }

    #[test]
    fn checksum_valid_accepts_generated_keys() {
        for _ in 0..10 {
            assert!(key_checksum_valid(&generate_license_key()));
        }
    }

    #[test]
    fn checksum_valid_accepts_all_a_key() {
        // Sixteen 'A's: every index is 0, so the checksum (0) matches.
        assert!(key_checksum_valid("AAAAAAAAAAAAAAAA"));
        assert!(key_checksum_valid("AAAA-AAAA-AAAA-AAAA"));
    }

    #[test]
    fn checksum_valid_rejects_tampered_key() {
        let key = generate_license_key();
        let last = key.chars().last().unwrap();
        let replacement = KEY_ALPHABET
            .iter()
            .map(|byte| *byte as char)
            .find(|c| *c != last)
            .unwrap();
        let mut tampered = key.clone();
        tampered.pop();
        tampered.push(replacement);
        assert!(!key_checksum_valid(&tampered));
        assert!(!key_checksum_valid("AAAAAAAAAAAAAAAB"));
    }

    #[test]
    fn checksum_is_a_plain_sum_so_sum_preserving_tampering_passes() {
        // "B9AAAAAAAAAAAAAA": B is index 1, 9 is index 31, the rest
        // are 0, so the first-15 sum is 32 == 0 (mod 32) and the
        // checksum character A (index 0) still matches. The checksum
        // catches typos, not forgeries: only the database lookup in
        // validation stops a key that was never issued.
        assert!(key_checksum_valid("B9AAAAAAAAAAAAAA"));
        assert!(key_checksum_valid("B9AA-AAAA-AAAA-AAAA"));
    }

    #[test]
    fn checksum_valid_rejects_wrong_shape() {
        assert!(!key_checksum_valid(""));
        assert!(!key_checksum_valid("AAA"));
        assert!(!key_checksum_valid("AAAAAAAAAAAAAAAAA"));
        // Lowercase letters are not in the alphabet.
        assert!(!key_checksum_valid("aaaaaaaaaaaaaaaa"));
        // Ambiguous characters are not in the alphabet.
        assert!(!key_checksum_valid("0000-0000-0000-0000"));
        assert!(!key_checksum_valid("IIII-IIII-IIII-IIII"));
    }

    #[test]
    fn normalize_key_trims_and_uppercases() {
        assert_eq!(normalize_key("  abcd-efgh "), "ABCD-EFGH");
        assert_eq!(normalize_key("ABCD"), "ABCD");
        assert_eq!(normalize_key(""), "");
    }
}
