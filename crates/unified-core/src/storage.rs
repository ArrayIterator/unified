// Storage provider abstraction.
//
// This is the single storage abstraction used by backup, assets serving,
// and the downloader. Every provider implements the same contract
// (`StorageProvider`); callers pick a provider by name from the
// `StorageRegistry` and never touch provider-specific code.
//
// Providers:
//   - `LocalProvider` — files under a configured base directory.
//   - `S3Provider` — AWS S3, SigV4-signed requests over reqwest.
//   - `R2Provider` — Cloudflare R2, S3-compatible, reuses the S3
//     signing code with a custom endpoint.
//   - `GoogleDriveProvider` — Google Drive (`drive.file` scope) with
//     an OAuth2 access token and a refresh-token flow.
//
// Security rules applied by every provider:
//   - Storage keys are validated (`validate_key`): empty keys, absolute
//     paths, `..` / `.` segments, empty segments, backslashes, and
//     control characters are all rejected.
//   - Every HTTP call to a provider endpoint validates the host against
//     an allowlist: known provider hosts, suffix rules for provider
//     host families, and config-provided extra hosts. IP-literal hosts
//     are rejected (except loopback for local development endpoints),
//     credentials in URLs are rejected, and plain `http` is accepted
//     only for loopback hosts. Arbitrary user-supplied URLs are never
//     fetched.
//   - Credentials live only in configuration / settings, are never
//     logged, and are never returned by API responses (see
//     `StorageTargetConfig::masked_config`).

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

pub mod config;
pub mod drive;
pub mod events;
pub mod keys;
pub mod local;
pub mod s3;
pub mod sftp;
#[cfg(test)]
mod tests;
pub mod validation;

pub use self::config::*;
pub use self::drive::*;
pub use self::keys::*;
pub use self::local::*;
pub use self::s3::*;
pub use self::sftp::*;
pub use self::validation::*;

// ---------------------------------------------------------------------------
// Provider contract.
// ---------------------------------------------------------------------------

/// The single storage contract used by backup, assets serving, and the
/// downloader.
///
/// `upload` returns the storage key under which the object can later
/// be fetched with `download` / `delete` / `signed_url`. For most
/// providers that is the key that was passed in; Google Drive returns
/// the Drive file id instead (see `GoogleDriveProvider`).
#[async_trait]
pub trait StorageProvider: Send + Sync {
    fn kind(&self) -> StorageKind;
    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError>;
    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError>;
    async fn delete(&self, key: &str) -> Result<(), UnifiedError>;
    async fn signed_url(&self, key: &str, expires_secs: u64) -> Result<String, UnifiedError>;
    async fn test_connection(&self) -> Result<(), UnifiedError>;

    /// List object keys under `prefix` (a key-like prefix; an empty
    /// prefix lists from the provider root). The default
    /// implementation reports that listing is not supported: providers
    /// that can enumerate objects override it (local and SFTP do;
    /// S3 / R2 / Drive do not, because a list operation is outside
    /// the core object contract they were built against). Callers
    /// must treat a listing error as "enumeration unavailable", not
    /// as "the bucket is empty".
    async fn list(&self, prefix: &str) -> Result<Vec<String>, UnifiedError> {
        let _ = prefix;
        Err(UnifiedError::bad_request(
            "This storage provider does not support listing objects",
        ))
    }

    /// Download an object and verify it against an expected SHA-256
    /// hex digest. A mismatch is a 500 with
    /// `metadata.checksum_mismatch == true` and the bytes are never
    /// returned, so corrupt data is reported instead of silently
    /// served. The default implementation downloads the bytes and
    /// hashes them; providers that record a checksum at write time
    /// (local and SFTP sidecars) additionally verify on plain
    /// `download` against their own record.
    async fn download_verified(
        &self,
        key: &str,
        expected_sha256: &str,
    ) -> Result<Vec<u8>, UnifiedError> {
        let data = self.download(key).await?;
        let actual = sha256_hex(&data);
        if actual != expected_sha256.trim().to_ascii_lowercase() {
            return Err(UnifiedError::internal(format!(
                "Stored object \"{key}\" failed SHA-256 verification"
            ))
            .with_metadata("checksum_mismatch", Value::Bool(true))
            .with_metadata("key", Value::String(key.to_string())));
        }
        Ok(data)
    }

    /// True when the object at `key` matches `expected_sha256`.
    /// A checksum mismatch answers `Ok(false)`; any other failure
    /// (missing object, provider error) is still an error.
    async fn verify_checksum(
        &self,
        key: &str,
        expected_sha256: &str,
    ) -> Result<bool, UnifiedError> {
        match self.download_verified(key, expected_sha256).await {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .metadata
                    .get("checksum_mismatch")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Download only the byte range `[start, end]` (both inclusive)
    /// of an object, for HTTP `Range` serving (`206 Partial
    /// Content`). The default implementation downloads the whole
    /// object and slices it: correct for every provider, but it
    /// holds the whole object in RAM. Providers that can fetch a
    /// range natively (local: seek + read; S3/R2: GetObject with a
    /// `Range` header) override this so a range read never loads
    /// the whole object. A range outside the object answers 416
    /// (`Range Not Satisfiable`). A range read is NOT
    /// checksum-verified: the recorded SHA-256 covers the whole
    /// object, and a slice cannot be verified against it (the full
    /// `download` still verifies).
    async fn download_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, UnifiedError> {
        let data = self.download(key).await?;
        let len = data.len() as u64;
        if start >= len || end < start || end >= len {
            return Err(UnifiedError::new(
                format!("Range {start}-{end} is outside the {len} byte object \"{key}\""),
                416,
            ));
        }
        Ok(data[start as usize..=end as usize].to_vec())
    }

    /// Byte size of a stored object, without downloading it. The
    /// default implementation downloads the object and measures
    /// it: correct, but expensive; providers with a cheap stat
    /// (local: filesystem metadata; S3/R2: HeadObject) override
    /// it. The `/files/public/` route needs this: it serves
    /// without a DB read, so the size cannot come from the
    /// document row.
    async fn object_size(&self, key: &str) -> Result<u64, UnifiedError> {
        Ok(self.download(key).await?.len() as u64)
    }

    /// Move an object from one key to another within the same
    /// provider (the visibility flip: public <-> private area).
    /// The default implementation copies (download + upload) and
    /// then deletes the source; when the delete fails the copy is
    /// left behind and the error is returned, so the caller can
    /// retry the delete or let the orphan sweeper reclaim it.
    /// Providers with an atomic move (local: rename) override this.
    async fn move_object(&self, from_key: &str, to_key: &str) -> Result<(), UnifiedError> {
        if from_key == to_key {
            return Ok(());
        }
        let data = self.download(from_key).await?;
        self.upload(to_key, data).await?;
        self.delete(from_key).await
    }
}

// ---------------------------------------------------------------------------
// Registry.
// ---------------------------------------------------------------------------

/// Named providers, shared through `AppContext`. Registration is
/// interior-mutable so modules can add targets at runtime; re-registering
/// a name replaces the previous provider.
pub struct StorageRegistry {
    providers: RwLock<HashMap<String, Arc<dyn StorageProvider>>>,
}

impl StorageRegistry {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, name: String, provider: Arc<dyn StorageProvider>) {
        self.providers
            .write()
            .expect("storage registry lock poisoned")
            .insert(name, provider);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn StorageProvider>> {
        self.providers
            .read()
            .expect("storage registry lock poisoned")
            .get(name)
            .cloned()
    }

    /// Registered target names, sorted.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .read()
            .expect("storage registry lock poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    pub fn remove(&self, name: &str) -> bool {
        self.providers
            .write()
            .expect("storage registry lock poisoned")
            .remove(name)
            .is_some()
    }
}

impl Default for StorageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
