// storage — local.
// Local filesystem provider.
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
// Local provider.
// ---------------------------------------------------------------------------

/// Filesystem storage under a configured base directory.
///
/// Keys are relative paths under the base directory; any key that would
/// escape the base is rejected. Writes are atomic: data goes to a
/// temporary file in the same directory and is renamed into place, so a
/// crash never leaves a partial object at the final path. The SHA-256
/// of every uploaded object is recorded in a durable sidecar file
/// (`<key>.sha256`, written atomically next to the object) as well as
/// in memory (`checksum`), and verified on download whenever a record
/// exists — so corruption is detected across restarts, not only for
/// objects uploaded since this process started. Keys ending in
/// `.sha256` are reserved for sidecars and rejected as object keys.
/// Objects written before sidecars existed (or written outside this
/// provider) have no record and are served unverified; the
/// storage-manager verify endpoint backfills their checksum.
pub struct LocalProvider {
    base_dir: PathBuf,
    checksums: RwLock<HashMap<String, String>>,
}

impl LocalProvider {
    pub fn new(base_dir: PathBuf) -> UnifiedResult<Self> {
        std::fs::create_dir_all(&base_dir).map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot create local storage directory {}: {error}",
                base_dir.display()
            ))
        })?;
        let canonical = std::fs::canonicalize(&base_dir).map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot resolve local storage directory {}: {error}",
                base_dir.display()
            ))
        })?;
        Ok(Self {
            base_dir: canonical,
            checksums: RwLock::new(HashMap::new()),
        })
    }

    /// Base directory this provider stores under.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// SHA-256 recorded for `key` at upload time, if any.
    pub fn checksum(&self, key: &str) -> Option<String> {
        self.checksums
            .read()
            .expect("local storage lock poisoned")
            .get(key)
            .cloned()
    }

    /// Path of the durable checksum sidecar for an object path:
    /// the object path with `.sha256` appended.
    fn sidecar_path(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_os_string();
        name.push(".sha256");
        PathBuf::from(name)
    }

    /// SHA-256 recorded for `key`, preferring the durable sidecar
    /// over the in-memory record. `None` when no record exists
    /// (object predates sidecars or was written outside this
    /// provider).
    pub fn recorded_checksum(&self, key: &str) -> Option<String> {
        let path = self.resolve(key).ok()?;
        let sidecar = Self::sidecar_path(&path);
        if let Ok(raw) = std::fs::read_to_string(&sidecar) {
            let digest = raw.trim().to_ascii_lowercase();
            if digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(digest);
            }
        }
        self.checksum(key)
    }

    /// Write a checksum sidecar atomically (temporary file in the
    /// same directory, then rename into place).
    fn write_sidecar(&self, path: &Path, checksum: &str) -> UnifiedResult<()> {
        let sidecar = Self::sidecar_path(path);
        let temp_name = format!(
            ".{}.tmp-{}",
            sidecar
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "checksum".to_string()),
            uuid::Uuid::now_v7()
        );
        let temp_path = sidecar
            .parent()
            .map(|parent| parent.join(&temp_name))
            .unwrap_or_else(|| PathBuf::from(temp_name));
        let result = (|| -> UnifiedResult<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp_path).map_err(|error| {
                UnifiedError::internal(format!("Cannot create checksum sidecar: {error}"))
            })?;
            file.write_all(checksum.as_bytes()).map_err(|error| {
                UnifiedError::internal(format!("Cannot write checksum sidecar: {error}"))
            })?;
            file.sync_all().map_err(|error| {
                UnifiedError::internal(format!("Cannot flush checksum sidecar: {error}"))
            })?;
            drop(file);
            std::fs::rename(&temp_path, &sidecar).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot move checksum sidecar into place at {}: {error}",
                    sidecar.display()
                ))
            })?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    /// Resolve a validated key to a path, guaranteeing the result stays
    /// inside the base directory (lexical check plus canonicalization
    /// of any existing parent, so symlinks inside the base cannot be
    /// used to escape it).
    fn resolve(&self, key: &str) -> UnifiedResult<PathBuf> {
        validate_key(key)?;
        if key.ends_with(".sha256") {
            return Err(UnifiedError::bad_request(
                "Storage keys ending in \".sha256\" are reserved for integrity sidecars",
            ));
        }
        let mut path = self.base_dir.clone();
        for segment in key.split('/') {
            path.push(segment);
        }
        if !path.starts_with(&self.base_dir) {
            return Err(UnifiedError::forbidden(
                "Storage key escapes the local storage directory",
            ));
        }
        if let Some(parent) = path.parent() {
            if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
                if !canonical_parent.starts_with(&self.base_dir) {
                    return Err(UnifiedError::forbidden(
                        "Storage key escapes the local storage directory",
                    ));
                }
            }
        }
        Ok(path)
    }
}

#[async_trait]
impl StorageProvider for LocalProvider {
    fn kind(&self) -> StorageKind {
        StorageKind::Local
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot create storage directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let checksum = sha256_hex(&data);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "object".to_string());
        let temp_name = format!(".{file_name}.tmp-{}", uuid::Uuid::now_v7());
        let temp_path = path
            .parent()
            .map(|parent| parent.join(&temp_name))
            .unwrap_or_else(|| PathBuf::from(temp_name));
        let write_result = (|| -> Result<(), UnifiedError> {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp_path).map_err(|error| {
                UnifiedError::internal(format!("Cannot create temporary file: {error}"))
            })?;
            file.write_all(&data).map_err(|error| {
                UnifiedError::internal(format!("Cannot write temporary file: {error}"))
            })?;
            file.sync_all().map_err(|error| {
                UnifiedError::internal(format!("Cannot flush temporary file: {error}"))
            })?;
            drop(file);
            std::fs::rename(&temp_path, &path).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot move uploaded object into place at {}: {error}",
                    path.display()
                ))
            })?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        // The sidecar is part of the upload: if it cannot be written
        // the object is removed again rather than left behind
        // without an integrity record.
        if let Err(error) = self.write_sidecar(&path, &checksum) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        self.checksums
            .write()
            .expect("local storage lock poisoned")
            .insert(key.to_string(), checksum);
        Ok(key.to_string())
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        let path = self.resolve(key)?;
        if !path.is_file() {
            return Err(UnifiedError::not_found(format!(
                "Object \"{key}\" not found in local storage"
            )));
        }
        let data = std::fs::read(&path).map_err(|error| {
            UnifiedError::internal(format!("Cannot read stored object \"{key}\": {error}"))
        })?;
        if let Some(expected) = self.recorded_checksum(key) {
            let actual = sha256_hex(&data);
            if actual != expected {
                return Err(UnifiedError::internal(format!(
                    "Stored object \"{key}\" failed its recorded SHA-256 checksum"
                ))
                .with_metadata("checksum_mismatch", Value::Bool(true))
                .with_metadata("key", Value::String(key.to_string())));
            }
        }
        Ok(data)
    }

    async fn download_range(&self, key: &str, start: u64, end: u64) -> Result<Vec<u8>, UnifiedError> {
        use std::io::{Read, Seek, SeekFrom};
        let path = self.resolve(key)?;
        let mut file = std::fs::File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                UnifiedError::not_found(format!("Object \"{key}\" not found in local storage"))
            } else {
                UnifiedError::internal(format!("Cannot read stored object \"{key}\": {error}"))
            }
        })?;
        let len = file.metadata().map_err(|error| {
            UnifiedError::internal(format!("Cannot stat stored object \"{key}\": {error}"))
        })?.len();
        if start >= len || end < start || end >= len {
            return Err(UnifiedError::new(
                format!("Range {start}-{end} is outside the {len} byte object \"{key}\""),
                416,
            ));
        }
        file.seek(SeekFrom::Start(start)).map_err(|error| {
            UnifiedError::internal(format!("Cannot seek stored object \"{key}\": {error}"))
        })?;
        let mut data = vec![0u8; (end - start + 1) as usize];
        file.read_exact(&mut data).map_err(|error| {
            UnifiedError::internal(format!("Cannot read stored object \"{key}\": {error}"))
        })?;
        Ok(data)
    }

    async fn object_size(&self, key: &str) -> Result<u64, UnifiedError> {
        let path = self.resolve(key)?;
        std::fs::metadata(&path).map(|metadata| metadata.len()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                UnifiedError::not_found(format!("Object \"{key}\" not found in local storage"))
            } else {
                UnifiedError::internal(format!("Cannot stat stored object \"{key}\": {error}"))
            }
        })
    }

    async fn move_object(&self, from_key: &str, to_key: &str) -> Result<(), UnifiedError> {
        if from_key == to_key {
            return Ok(());
        }
        let from_path = self.resolve(from_key)?;
        let to_path = self.resolve(to_key)?;
        if !from_path.is_file() {
            return Err(UnifiedError::not_found(format!(
                "Object \"{from_key}\" not found in local storage"
            )));
        }
        if let Some(parent) = to_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot create storage directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        // Atomic rename on the same filesystem: the flip costs no
        // copy and no transient double. When the rename fails (the
        // target is on another device, which cannot happen under
        // one base directory but is guarded anyway), fall back to
        // copy + delete.
        match std::fs::rename(&from_path, &to_path) {
            Ok(()) => {}
            Err(_) => {
                std::fs::copy(&from_path, &to_path).map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot copy stored object \"{from_key}\" to \"{to_key}\": {error}"
                    ))
                })?;
                std::fs::remove_file(&from_path).map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot delete moved object \"{from_key}\": {error}"
                    ))
                })?;
            }
        }
        // The sidecar moves with the object so the target keeps its
        // integrity record and the source leaves none behind.
        let from_sidecar = Self::sidecar_path(&from_path);
        if from_sidecar.is_file() {
            let to_sidecar = Self::sidecar_path(&to_path);
            if std::fs::rename(&from_sidecar, &to_sidecar).is_err() {
                let _ = std::fs::copy(&from_sidecar, &to_sidecar);
                let _ = std::fs::remove_file(&from_sidecar);
            }
        }
        if let Some(checksum) = self
            .checksums
            .write()
            .expect("local storage lock poisoned")
            .remove(from_key)
        {
            self.checksums
                .write()
                .expect("local storage lock poisoned")
                .insert(to_key.to_string(), checksum);
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        let path = self.resolve(key)?;
        // Idempotent, matching S3 delete semantics: deleting a missing
        // object is not an error.
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot delete stored object \"{key}\": {error}"
                ))
            })?;
        }
        // The sidecar goes with the object so a later object under
        // the same key is never verified against a stale checksum.
        let _ = std::fs::remove_file(Self::sidecar_path(&path));
        self.checksums
            .write()
            .expect("local storage lock poisoned")
            .remove(key);
        Ok(())
    }

    async fn signed_url(&self, key: &str, _expires_secs: u64) -> Result<String, UnifiedError> {
        validate_key(key)?;
        // Local files have no signing mechanism; serve them through the
        // storage module's download endpoint instead.
        Err(UnifiedError::bad_request(
            "Local storage does not issue signed URLs; download the object through the storage API instead",
        ))
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        let probe_name = format!(".healthcheck-{}", uuid::Uuid::now_v7());
        let probe_path = self.base_dir.join(probe_name);
        std::fs::write(&probe_path, b"ok").map_err(|error| {
            UnifiedError::internal(format!(
                "Local storage directory {} is not writable: {error}",
                self.base_dir.display()
            ))
        })?;
        let _ = std::fs::remove_file(&probe_path);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, UnifiedError> {
        // A prefix is a directory-like path: validate it like a key,
        // except a trailing `/` (directory form) is allowed and an
        // empty prefix lists from the base directory.
        let clean_prefix = prefix.trim_end_matches('/');
        if !clean_prefix.is_empty() {
            validate_key(clean_prefix)?;
        }
        let root = if clean_prefix.is_empty() {
            self.base_dir.clone()
        } else {
            let mut path = self.base_dir.clone();
            for segment in clean_prefix.split('/') {
                path.push(segment);
            }
            if !path.starts_with(&self.base_dir) {
                return Err(UnifiedError::forbidden(
                    "Storage prefix escapes the local storage directory",
                ));
            }
            path
        };
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut keys: Vec<String> = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(&self.base_dir) else {
                continue;
            };
            let key = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if key.is_empty() {
                continue;
            }
            // Integrity sidecars (`<key>.sha256`) are not objects.
            if key.ends_with(".sha256") {
                continue;
            }
            // Skip the atomic-upload temporary files.
            if relative
                .file_name()
                .map(|name| name.to_string_lossy().starts_with('.'))
                .unwrap_or(false)
                && relative
                    .file_name()
                    .map(|name| name.to_string_lossy().contains(".tmp-"))
                    .unwrap_or(false)
            {
                continue;
            }
            keys.push(key);
        }
        keys.sort();
        Ok(keys)
    }
}
