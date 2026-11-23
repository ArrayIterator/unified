// storage — keys.
// Storage key validation and key helpers (hashing, encoding).
//
// Split from `crates/unified-core/src/storage.rs`; moved verbatim, no behaviour changes.

use super::*;
use crate::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Key validation (shared by all providers).
// ---------------------------------------------------------------------------

/// Maximum storage key length in bytes.
pub const MAX_KEY_LEN: usize = 1024;

/// Validate a storage key. Keys are slash-separated relative paths:
/// empty keys, absolute paths, `..` / `.` segments, empty segments,
/// backslashes, and control characters are rejected.
pub fn validate_key(key: &str) -> UnifiedResult<()> {
    if key.is_empty() {
        return Err(UnifiedError::bad_request("Storage key must not be empty"));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Storage key exceeds the maximum length of {MAX_KEY_LEN} bytes"
        )));
    }
    if key.starts_with('/') || key.starts_with('\\') {
        return Err(UnifiedError::bad_request(
            "Storage key must be a relative path (absolute paths are not allowed)",
        ));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(
            "Storage key must not contain control characters",
        ));
    }
    for segment in key.split('/') {
        if segment.is_empty() {
            return Err(UnifiedError::bad_request(
                "Storage key must not contain empty segments",
            ));
        }
        if segment == "." || segment == ".." {
            return Err(UnifiedError::bad_request(
                "Storage key must not contain \".\" or \"..\" segments",
            ));
        }
        if segment.contains('\\') {
            return Err(UnifiedError::bad_request(
                "Storage key must not contain backslashes",
            ));
        }
    }
    Ok(())
}

/// Validate a shared-media reference: the single form every module
/// uses to point at a file in the shared documents store, so media
/// is never re-uploaded per module. Two forms are accepted:
///
/// * a documents document id (a UUID), resolved to a `/files/` URL
///   by the caller through the documents module; or
/// * a `/files/public/{key}` or `/files/private/{key}` URL — the
///   exact URL the media picker returns (a query string is allowed,
///   e.g. a signed `?token=` or `?download=1`).
///
/// Returns the trimmed reference unchanged. Anything else — an
/// absolute URL to another host, a bare storage key, a path with
/// `..` segments — is rejected, so a media field can never point
/// outside the shared store.
pub fn validate_media_reference(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(
            "Media reference must not be empty",
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("/files/") {
        let (area_and_key, _query) = match rest.split_once('?') {
            Some((head, tail)) => (head, Some(tail)),
            None => (rest, None),
        };
        let (area, key) = match area_and_key.split_once('/') {
            Some((area, key)) => (area, key),
            None => {
                return Err(UnifiedError::bad_request(
                    "Media reference must be /files/public/{key} or /files/private/{key}",
                ));
            }
        };
        if area != "public" && area != "private" {
            return Err(UnifiedError::bad_request(
                "Media reference must be /files/public/{key} or /files/private/{key}",
            ));
        }
        validate_key(key)?;
        return Ok(trimmed.to_string());
    }
    if uuid::Uuid::parse_str(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }
    Err(UnifiedError::bad_request(
        "Media reference must be a documents document id or a /files/public/{key} or /files/private/{key} URL",
    ))
}

/// Validate a target name: non-empty, at most 128 characters, letters,
/// digits, `-` and `_` only.
pub fn validate_target_name(name: &str) -> UnifiedResult<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Storage target name must be 1..=128 characters",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "Storage target name must contain only letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

type HmacSha256 = Hmac<sha2::Sha256>;

pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> UnifiedResult<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| UnifiedError::internal(format!("Cannot create HMAC: {error}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Percent-encode one string per RFC 3986 (unreserved characters pass
/// through, everything else becomes `%XX`).
pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Percent-encode a storage key, segment by segment, keeping `/` as
/// the separator.
pub(crate) fn encode_key(key: &str) -> String {
    key.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_reference_accepts_document_id_and_files_urls() {
        assert_eq!(
            validate_media_reference("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(
            validate_media_reference(
                "/files/public/image/2026/11/23/019a0000-0000-7000-8000-000000000000.jpg"
            )
            .unwrap(),
            "/files/public/image/2026/11/23/019a0000-0000-7000-8000-000000000000.jpg"
        );
        assert_eq!(
            validate_media_reference(
                "/files/private/image/2026/11/23/019a0000-0000-7000-8000-000000000000.jpg?token=abc"
            )
            .unwrap(),
            "/files/private/image/2026/11/23/019a0000-0000-7000-8000-000000000000.jpg?token=abc"
        );
        // Whitespace is trimmed.
        assert_eq!(
            validate_media_reference("  /files/public/image/2026/11/23/x.jpg  ").unwrap(),
            "/files/public/image/2026/11/23/x.jpg"
        );
    }

    #[test]
    fn media_reference_rejects_everything_outside_the_shared_store() {
        assert!(validate_media_reference("").is_err());
        assert!(validate_media_reference("https://cdn.example.com/cover.jpg").is_err());
        assert!(validate_media_reference("/files/draft/image/2026/11/23/x.jpg").is_err());
        assert!(validate_media_reference("/files/public/../secret.txt").is_err());
        assert!(validate_media_reference("/files/public/").is_err());
        assert!(validate_media_reference("image/2026/11/23/x.jpg").is_err());
        assert!(validate_media_reference("not-a-uuid").is_err());
    }
}
