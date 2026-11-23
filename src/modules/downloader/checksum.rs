//! Checksum hashing and verification.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Checksums.
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Constant-time digest comparison, so a checksum check does not leak
/// how many leading characters matched.
fn digests_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Streaming hashers for the requested checksum algorithm. Only the
/// requested algorithm is computed.
pub(crate) struct ChecksumHashers {
    sha256: Option<sha2::Sha256>,
    sha512: Option<sha2::Sha512>,
    sha1: Option<sha1::Sha1>,
}

impl ChecksumHashers {
    pub(crate) fn for_algorithm(algorithm: Option<&str>) -> Self {
        match algorithm {
            Some("sha256") => {
                use sha2::Digest;
                Self {
                    sha256: Some(sha2::Sha256::new()),
                    sha512: None,
                    sha1: None,
                }
            }
            Some("sha512") => {
                use sha2::Digest;
                Self {
                    sha256: None,
                    sha512: Some(sha2::Sha512::new()),
                    sha1: None,
                }
            }
            Some("sha1") => {
                use sha1::Digest;
                Self {
                    sha256: None,
                    sha512: None,
                    sha1: Some(sha1::Sha1::new()),
                }
            }
            _ => Self {
                sha256: None,
                sha512: None,
                sha1: None,
            },
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.sha256.is_some() || self.sha512.is_some() || self.sha1.is_some()
    }

    pub(crate) fn update(&mut self, chunk: &[u8]) {
        if let Some(hasher) = self.sha256.as_mut() {
            use sha2::Digest;
            hasher.update(chunk);
        }
        if let Some(hasher) = self.sha512.as_mut() {
            use sha2::Digest;
            hasher.update(chunk);
        }
        if let Some(hasher) = self.sha1.as_mut() {
            use sha1::Digest;
            hasher.update(chunk);
        }
    }

    /// Finalize the hash and compare it with the expected digest. On a
    /// mismatch the partial file is removed (its bytes are wrong, so a
    /// resume would only re-download the same wrong content) and a
    /// retryable error is returned.
    pub(crate) fn finish(self, job: &DownloadJobState) -> Result<(), AttemptError> {
        let expected = job
            .checksum
            .as_ref()
            .map(|value| value.trim().to_lowercase());
        let Some(expected) = expected else {
            return Ok(());
        };
        let digest = if let Some(hasher) = self.sha256 {
            use sha2::Digest;
            hex_encode(&hasher.finalize())
        } else if let Some(hasher) = self.sha512 {
            use sha2::Digest;
            hex_encode(&hasher.finalize())
        } else if let Some(hasher) = self.sha1 {
            use sha1::Digest;
            hex_encode(&hasher.finalize())
        } else {
            return Ok(());
        };
        let matches = digests_equal(&digest, &expected);
        *job.checksum_ok.write().expect("job lock poisoned") = Some(matches);
        if !matches {
            let _ = std::fs::remove_file(&job.part_path);
            return Err(AttemptError::retryable(format!(
                "Checksum mismatch for {}: expected {expected}, got {digest}",
                job.destination.display()
            )));
        }
        Ok(())
    }
}

/// Stream a file through the hashers in 64 KiB chunks, never loading the
/// whole file into memory. `limit` hashes only the first `limit` bytes
/// (the already-downloaded prefix when resuming); `None` hashes the
/// whole file.
pub(crate) fn hash_file(
    path: &Path,
    limit: Option<u64>,
    hashers: &mut ChecksumHashers,
) -> Result<(), AttemptError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        AttemptError::terminal(format!("Cannot read partial file for checksum: {error}"))
    })?;
    let mut remaining = limit.unwrap_or(u64::MAX);
    let mut buffer = [0u8; 65_536];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..want]).map_err(|error| {
            AttemptError::terminal(format!("Cannot read partial file for checksum: {error}"))
        })?;
        if read == 0 {
            break;
        }
        hashers.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(())
}

/// Verify the checksum of a file that is already fully on disk (used
/// when the server reports the requested range as already satisfied).
pub(crate) fn verify_checksum_file(job: &DownloadJobState) -> Result<(), AttemptError> {
    if job.checksum.is_none() {
        return Ok(());
    }
    let mut hashers = ChecksumHashers::for_algorithm(job.checksum_algorithm.as_deref());
    hash_file(&job.part_path, None, &mut hashers)?;
    hashers.finish(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_cases() {
        assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x01, 0x02]), "0102");
    }

    #[test]
    fn digests_equal_cases() {
        assert!(digests_equal("abc123", "abc123"));
        assert!(!digests_equal("abc123", "abc124"));
        assert!(!digests_equal("abc", "abcd"));
        assert!(digests_equal("", ""));
    }
    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

}
