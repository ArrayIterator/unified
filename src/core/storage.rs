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

use crate::core::error::{UnifiedError, UnifiedResult};
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
// Storage kind.
// ---------------------------------------------------------------------------

/// The kind of a storage target. Serialized in snake case
/// (`local`, `s3`, `google_drive`, `r2`, `sftp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    Local,
    S3,
    GoogleDrive,
    R2,
    Sftp,
}

impl StorageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::S3 => "s3",
            Self::GoogleDrive => "google_drive",
            Self::R2 => "r2",
            Self::Sftp => "sftp",
        }
    }

    /// All supported kinds, for status responses and validation.
    pub fn all() -> &'static [StorageKind] {
        &[Self::Local, Self::S3, Self::GoogleDrive, Self::R2, Self::Sftp]
    }
}

impl fmt::Display for StorageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StorageKind {
    type Err = UnifiedError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "s3" => Ok(Self::S3),
            "google_drive" | "google-drive" | "googledrive" | "drive" => {
                Ok(Self::GoogleDrive)
            }
            "r2" => Ok(Self::R2),
            "sftp" => Ok(Self::Sftp),
            other => Err(UnifiedError::bad_request(format!(
                "Unknown storage kind \"{other}\" (expected local, s3, google_drive, r2 or sftp)"
            ))),
        }
    }
}

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

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

type HmacSha256 = Hmac<sha2::Sha256>;

fn hmac_sha256(key: &[u8], data: &[u8]) -> UnifiedResult<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| UnifiedError::internal(format!("Cannot create HMAC: {error}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Percent-encode one string per RFC 3986 (unreserved characters pass
/// through, everything else becomes `%XX`).
fn percent_encode(input: &str) -> String {
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
fn encode_key(key: &str) -> String {
    key.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// Mask a secret for display: only the last four characters are shown.
/// Short or empty values are fully masked.
pub fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "********".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("****{tail}")
}

/// Config field names that hold secrets and must never be returned.
pub const SECRET_CONFIG_FIELDS: [&str; 6] = [
    "secret_key",
    "access_key",
    "client_secret",
    "refresh_token",
    "access_token",
    "sftp_password",
];

// ---------------------------------------------------------------------------
// Host allowlist (URL safety).
// ---------------------------------------------------------------------------

/// Exact hosts a storage provider may be contacted on without any
/// extra configuration.
const KNOWN_STORAGE_HOSTS: [&str; 4] = [
    "oauth2.googleapis.com",
    "www.googleapis.com",
    "drive.googleapis.com",
    "upload.googleapis.com",
];

/// Host suffixes that are always allowed: any AWS S3 host, any
/// Cloudflare R2 host, and any Google API host.
const ALLOWED_HOST_SUFFIXES: [&str; 3] = [
    ".amazonaws.com",
    ".r2.cloudflarestorage.com",
    ".googleapis.com",
];

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host.parse::<IpAddr>().is_ok_and(|addr| {
            addr.is_loopback()
        })
}

/// True when `host` is allowed: a known provider host, a host under an
/// allowed provider suffix, or an exact match in `extra_hosts`
/// (config-provided hosts, for self-hosted S3-compatible endpoints).
pub fn is_allowed_storage_host(host: &str, extra_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    if KNOWN_STORAGE_HOSTS.contains(&host.as_str()) {
        return true;
    }
    if ALLOWED_HOST_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
    {
        return true;
    }
    extra_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host)
}

/// Validate a provider endpoint URL against the host allowlist.
/// Only `https` is accepted, except plain `http` for loopback hosts
/// (local development endpoints such as a local MinIO). Userinfo and
/// IP-literal hosts are rejected, except loopback IPs.
pub fn validate_endpoint_url(raw: &str, extra_hosts: &[String]) -> UnifiedResult<url::Url> {
    let url = url::Url::parse(raw.trim()).map_err(|_| {
        UnifiedError::bad_request("Storage endpoint is not a valid URL")
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Storage endpoint must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Storage endpoint has no host"))?
        .to_ascii_lowercase();
    let is_ip = host.parse::<IpAddr>().is_ok();
    if is_ip && !is_loopback_host(&host) {
        return Err(UnifiedError::forbidden(
            "Storage endpoints on IP-literal hosts are not allowed",
        ));
    }
    if !is_allowed_storage_host(&host, extra_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "Storage endpoint host \"{host}\" is not allowlisted"
        )));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback_host(&host) => Ok(url),
        _ => Err(UnifiedError::bad_request(
            "Storage endpoint must use https (http is allowed only for loopback hosts)",
        )),
    }
}

/// `Host` header value for a URL, including a non-default port.
fn host_header(url: &url::Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    }
}

/// Default User-Agent sent with storage requests.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

/// Build a reqwest client for storage traffic: fixed timeouts, no
/// proxy surprises, redirects disabled (provider endpoints must not
/// bounce requests elsewhere).
fn build_http_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Map a provider HTTP status to an error. 404/401/403 get their own
/// status; anything else is a 500 with the status in metadata. The
/// response body is never echoed: provider error bodies can contain
/// request ids and internal details.
fn map_provider_status(
    provider: &str,
    operation: &str,
    status: reqwest::StatusCode,
) -> UnifiedError {
    let message = format!("{provider} {operation} failed with HTTP {status}");
    match status.as_u16() {
        404 => UnifiedError::not_found(message),
        401 => UnifiedError::unauthorized(message),
        403 => UnifiedError::forbidden(message),
        _ => UnifiedError::internal(message)
            .with_metadata("status", Value::from(status.as_u16())),
    }
}

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
            .map(|parent| parent.join(temp_name))
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
            .map(|parent| parent.join(temp_name))
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

// ---------------------------------------------------------------------------
// S3 provider (AWS S3, SigV4).
// ---------------------------------------------------------------------------

/// Configuration for `S3Provider`.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Endpoint URL, for example `https://s3.us-east-1.amazonaws.com`.
    /// A custom S3-compatible endpoint must use `https` and its host
    /// must be listed in `storage.allowed_hosts`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    /// Never logged, never returned by any API response.
    pub secret_key: String,
}

impl S3Config {
    /// True when the endpoint is a standard AWS S3 endpoint, in which
    /// case virtual-host style URLs (`{bucket}.s3.{region}.amazonaws.com`)
    /// are used. Custom endpoints use path style
    /// (`{endpoint}/{bucket}/{key}`).
    fn is_aws_endpoint(host: &str, region: &str) -> bool {
        host == format!("s3.{region}.amazonaws.com") || host == "s3.amazonaws.com"
    }
}

/// AWS S3 object storage with SigV4-signed requests.
///
/// Honest scope: core object operations only (PUT / GET / DELETE,
/// presigned GET URLs, and a bucket list probe for `test_connection`).
/// No bucket management, versioning, lifecycle, or multipart upload.
pub struct S3Provider {
    config: S3Config,
    client: reqwest::Client,
    endpoint_url: url::Url,
    virtual_host: bool,
}

impl S3Provider {
    pub fn new(config: S3Config, extra_hosts: &[String]) -> UnifiedResult<Self> {
        if config.region.trim().is_empty() {
            return Err(UnifiedError::bad_request("S3 region is required"));
        }
        if config.bucket.trim().is_empty() {
            return Err(UnifiedError::bad_request("S3 bucket is required"));
        }
        if config.access_key.trim().is_empty() || config.secret_key.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "S3 access key and secret key are required",
            ));
        }
        let endpoint_url = validate_endpoint_url(&config.endpoint, extra_hosts)?;
        let host = endpoint_url
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let virtual_host = S3Config::is_aws_endpoint(&host, config.region.trim());
        // Normalize the stored fields so signing never sees whitespace.
        let config = S3Config {
            endpoint: config.endpoint.trim().to_string(),
            region: config.region.trim().to_string(),
            bucket: config.bucket.trim().to_string(),
            access_key: config.access_key.trim().to_string(),
            secret_key: config.secret_key,
        };
        Ok(Self {
            config,
            client: build_http_client()?,
            endpoint_url,
            virtual_host,
        })
    }

    /// Base URL for one object key (no query string).
    fn object_url(&self, key: &str) -> UnifiedResult<url::Url> {
        let encoded = encode_key(key);
        let raw = if self.virtual_host {
            let host = self.endpoint_url.host_str().unwrap_or_default();
            format!(
                "https://{}.{host}/{encoded}",
                self.config.bucket,
                host = host
            )
        } else {
            let base = self.config.endpoint.trim_end_matches('/');
            format!("{base}/{}/{encoded}", self.config.bucket)
        };
        url::Url::parse(&raw).map_err(|_| {
            UnifiedError::bad_request("Cannot build the S3 object URL for the given key")
        })
    }

    /// Base URL for bucket-level operations (no key).
    fn bucket_url(&self) -> UnifiedResult<url::Url> {
        let raw = if self.virtual_host {
            let host = self.endpoint_url.host_str().unwrap_or_default();
            format!("https://{}.{host}/", self.config.bucket, host = host)
        } else {
            let base = self.config.endpoint.trim_end_matches('/');
            format!("{base}/{}/", self.config.bucket)
        };
        url::Url::parse(&raw)
            .map_err(|_| UnifiedError::internal("Cannot build the S3 bucket URL"))
    }

    /// Sign one request with SigV4. Returns the `Authorization` header
    /// value and the `x-amz-date` value; the caller also sends
    /// `x-amz-content-sha256` with `payload_hash`.
    fn sign(
        &self,
        method: &str,
        url: &url::Url,
        query: &[(String, String)],
        payload_hash: &str,
        now: DateTime<Utc>,
    ) -> UnifiedResult<(String, String)> {
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let mut sorted: Vec<(String, String)> = query.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let canonical_query = sorted
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical_uri = if url.path().is_empty() {
            "/".to_string()
        } else {
            url.path().to_string()
        };
        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n",
            host_header(url)
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let scope = format!(
            "{date_stamp}/{}/s3/aws4_request",
            self.config.region
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let date_key = hmac_sha256(format!("AWS4{}", self.config.secret_key).as_bytes(), date_stamp.as_bytes())?;
        let region_key = hmac_sha256(&date_key, self.config.region.as_bytes())?;
        let service_key = hmac_sha256(&region_key, b"s3")?;
        let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
        let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.config.access_key
        );
        Ok((authorization, amz_date))
    }

    /// Build a presigned GET URL for one key. The signature covers the
    /// canonical query string; the URL is valid for `expires_secs`.
    fn presigned_url(&self, key: &str, expires_secs: u64) -> UnifiedResult<String> {
        let url = self.object_url(key)?;
        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let scope = format!(
            "{date_stamp}/{}/s3/aws4_request",
            self.config.region
        );
        let credential = format!("{}/{}", self.config.access_key, scope);
        let mut query: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential".to_string(), credential),
            ("X-Amz-Date".to_string(), amz_date.clone()),
            (
                "X-Amz-Expires".to_string(),
                expires_secs.to_string(),
            ),
            ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
        ];
        query.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let canonical_query = query
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical_uri = url.path().to_string();
        let canonical_headers = format!("host:{}\n", host_header(&url));
        let canonical_request = format!(
            "GET\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\nhost\nUNSIGNED-PAYLOAD"
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let date_key = hmac_sha256(format!("AWS4{}", self.config.secret_key).as_bytes(), date_stamp.as_bytes())?;
        let region_key = hmac_sha256(&date_key, self.config.region.as_bytes())?;
        let service_key = hmac_sha256(&region_key, b"s3")?;
        let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
        let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        Ok(format!(
            "{}?{}&X-Amz-Signature={}",
            &url[..url::Position::BeforeQuery],
            canonical_query,
            signature
        ))
    }

    /// Send one signed request. `query` is included in both the URL
    /// and the signature; `body` is the request payload (empty for
    /// GET / DELETE).
    async fn signed_request(
        &self,
        method: reqwest::Method,
        url: url::Url,
        query: &[(String, String)],
        body: Vec<u8>,
    ) -> UnifiedResult<reqwest::Response> {
        let mut url = url;
        if !query.is_empty() {
            let query_string = query
                .iter()
                .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            url.set_query(Some(&query_string));
        }
        let payload_hash = sha256_hex(&body);
        let (authorization, amz_date) = self.sign(
            method.as_str(),
            &url,
            query,
            &payload_hash,
            Utc::now(),
        )?;
        let response = self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, authorization)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .body(body)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("S3 request failed: {error}"))
            })?;
        Ok(response)
    }
}

#[async_trait]
impl StorageProvider for S3Provider {
    fn kind(&self) -> StorageKind {
        StorageKind::S3
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::PUT, url, &[], data)
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(key.to_string())
        } else {
            Err(map_provider_status("S3", "upload", status))
        }
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::GET, url, &[], Vec::new())
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("S3", "download", status));
        }
        response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| {
            UnifiedError::internal(format!("Cannot read S3 response body: {error}"))
        })
    }

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::DELETE, url, &[], Vec::new())
            .await?;
        let status = response.status();
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(map_provider_status("S3", "delete", status))
        }
    }

    async fn signed_url(&self, key: &str, expires_secs: u64) -> Result<String, UnifiedError> {
        validate_key(key)?;
        let expires = expires_secs.clamp(1, 604_800);
        self.presigned_url(key, expires)
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        let url = self.bucket_url()?;
        let query = vec![
            ("list-type".to_string(), "2".to_string()),
            ("max-keys".to_string(), "0".to_string()),
        ];
        let response = self
            .signed_request(reqwest::Method::GET, url, &query, Vec::new())
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(map_provider_status("S3", "connection test", status))
        }
    }
}

// ---------------------------------------------------------------------------
// R2 provider (Cloudflare R2, S3-compatible).
// ---------------------------------------------------------------------------

/// Configuration for `R2Provider`. The endpoint is the account endpoint,
/// for example `https://<account-id>.r2.cloudflarestorage.com`. The
/// region is `auto` for R2 unless a jurisdiction requires otherwise.
#[derive(Debug, Clone)]
pub struct R2Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    /// Never logged, never returned by any API response.
    pub secret_key: String,
}

/// Cloudflare R2. R2 speaks the S3 API, so this provider delegates
/// every operation to an inner `S3Provider` built with the R2 endpoint;
/// only the reported kind differs.
pub struct R2Provider {
    inner: S3Provider,
}

impl R2Provider {
    pub fn new(config: R2Config, extra_hosts: &[String]) -> UnifiedResult<Self> {
        let region = if config.region.trim().is_empty() {
            "auto".to_string()
        } else {
            config.region.clone()
        };
        let inner = S3Provider::new(
            S3Config {
                endpoint: config.endpoint,
                region,
                bucket: config.bucket,
                access_key: config.access_key,
                secret_key: config.secret_key,
            },
            extra_hosts,
        )?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl StorageProvider for R2Provider {
    fn kind(&self) -> StorageKind {
        StorageKind::R2
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        self.inner.upload(key, data).await
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        self.inner.download(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        self.inner.delete(key).await
    }

    async fn signed_url(&self, key: &str, expires_secs: u64) -> Result<String, UnifiedError> {
        self.inner.signed_url(key, expires_secs).await
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        self.inner.test_connection().await
    }
}

// ---------------------------------------------------------------------------
// Google Drive provider.
// ---------------------------------------------------------------------------

/// Configuration for `GoogleDriveProvider`.
#[derive(Debug, Clone)]
pub struct DriveConfig {
    pub client_id: String,
    /// Never logged, never returned by any API response.
    pub client_secret: String,
    /// Never logged, never returned by any API response.
    pub refresh_token: String,
    /// Optional pre-obtained access token. When present it is used
    /// until it expires; when absent (or expired) the refresh token
    /// is exchanged for a new one. Never logged.
    pub access_token: String,
    /// OAuth2 token endpoint. Defaults to
    /// `https://oauth2.googleapis.com/token`; a custom value must
    /// pass the host allowlist.
    pub token_url: String,
}

impl DriveConfig {
    fn token_url(&self) -> String {
        if self.token_url.trim().is_empty() {
            "https://oauth2.googleapis.com/token".to_string()
        } else {
            self.token_url.clone()
        }
    }
}

struct CachedToken {
    access_token: String,
    expires_at: DateTime<Utc>,
}

/// Google Drive storage (`drive.file` scope).
///
/// Keys are Drive file ids after upload: `upload` takes a display name
/// as the key, creates the file, and returns its Drive file id, which
/// is then the key for `download` / `delete` / `signed_url`. As a
/// convenience, `download` / `delete` / `signed_url` also accept a
/// plain file name: when the key is not a file id, the provider looks
/// the file up by name first.
///
/// `signed_url` returns a Drive share-style download URL
/// (`https://drive.google.com/uc?export=download&id={id}`). Honest
/// limitation: Google Drive has no time-limited presigned URLs, so
/// `expires_secs` cannot be enforced by Drive — the returned link
/// works only while the file is shared with link access, and it does
/// not expire on its own. Callers that need a time limit must serve
/// the bytes through the storage API instead.
pub struct GoogleDriveProvider {
    config: DriveConfig,
    client: reqwest::Client,
    token: tokio::sync::RwLock<Option<CachedToken>>,
}

impl GoogleDriveProvider {
    pub fn new(config: DriveConfig, extra_hosts: &[String]) -> UnifiedResult<Self> {
        if config.client_id.trim().is_empty() || config.client_secret.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Google Drive client id and client secret are required",
            ));
        }
        if config.refresh_token.trim().is_empty() && config.access_token.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Google Drive needs a refresh token or an access token",
            ));
        }
        let token_url = config.token_url();
        validate_endpoint_url(&token_url, extra_hosts)?;
        Ok(Self {
            config,
            client: build_http_client()?,
            token: tokio::sync::RwLock::new(None),
        })
    }

    /// Return a valid access token, refreshing it first when the
    /// cached one is missing or about to expire. Tokens are kept in
    /// memory only and never logged.
    async fn access_token(&self) -> UnifiedResult<String> {
        {
            let cached = self.token.read().await;
            if let Some(token) = cached.as_ref() {
                if token.expires_at > Utc::now() + chrono::Duration::seconds(60) {
                    return Ok(token.access_token.clone());
                }
            }
        }
        if self.config.refresh_token.trim().is_empty() {
            // No refresh token: the configured access token is all there
            // is. Cache it briefly so repeated calls do not re-read
            // config, and let the API report it when it expires.
            let access_token = self.config.access_token.clone();
            let mut cached = self.token.write().await;
            *cached = Some(CachedToken {
                access_token: access_token.clone(),
                expires_at: Utc::now() + chrono::Duration::seconds(3600),
            });
            return Ok(access_token);
        }
        let mut cached = self.token.write().await;
        if let Some(token) = cached.as_ref() {
            if token.expires_at > Utc::now() + chrono::Duration::seconds(60) {
                return Ok(token.access_token.clone());
            }
        }
        let response = self
            .client
            .post(self.config.token_url())
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("refresh_token", self.config.refresh_token.as_str()),
            ])
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Google Drive token refresh failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status(
                "Google Drive",
                "token refresh",
                status,
            ));
        }
        let body: Value = response.json().await.map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot parse Google Drive token response: {error}"
            ))
        })?;
        let access_token = body
            .get("access_token")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal("Google Drive token response has no access token")
            })?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(|value| value.as_u64())
            .unwrap_or(3600);
        let lifetime = expires_in.saturating_sub(60).max(60);
        *cached = Some(CachedToken {
            access_token: access_token.clone(),
            expires_at: Utc::now() + chrono::Duration::seconds(lifetime as i64),
        });
        Ok(access_token)
    }

    fn api_url(path: &str) -> UnifiedResult<url::Url> {
        url::Url::parse(&format!("https://www.googleapis.com{path}")).map_err(|_| {
            UnifiedError::internal("Cannot build the Google Drive API URL")
        })
    }

    /// Resolve a key to a Drive file id. The key itself is tried as a
    /// file id first; when that returns 404, the file is looked up by
    /// name and the first match wins.
    async fn resolve_file_id(&self, key: &str) -> UnifiedResult<String> {
        let token = self.access_token().await?;
        let url = Self::api_url(&format!(
            "/drive/v3/files/{}?fields=id",
            percent_encode(key)
        ))?;
        let response = self
            .client
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Google Drive lookup failed: {error}"))
            })?;
        if response.status().is_success() {
            return Ok(key.to_string());
        }
        if response.status().as_u16() != 404 {
            return Err(map_provider_status(
                "Google Drive",
                "file lookup",
                response.status(),
            ));
        }
        let mut url = Self::api_url("/drive/v3/files")?;
        let query = format!("name = '{}'", key.replace('\'', "\\'"));
        url.query_pairs_mut()
            .append_pair("q", &query)
            .append_pair("fields", "files(id)")
            .append_pair("pageSize", "1");
        let response = self
            .client
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Google Drive search failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("Google Drive", "file search", status));
        }
        let body: Value = response.json().await.map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot parse Google Drive search response: {error}"
            ))
        })?;
        body.get("files")
            .and_then(|files| files.as_array())
            .and_then(|files| files.first())
            .and_then(|file| file.get("id"))
            .and_then(|id| id.as_str())
            .map(|id| id.to_string())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Google Drive file \"{key}\" not found"))
            })
    }
}

#[async_trait]
impl StorageProvider for GoogleDriveProvider {
    fn kind(&self) -> StorageKind {
        StorageKind::GoogleDrive
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        validate_key(key)?;
        let token = self.access_token().await?;
        let name = key.rsplit('/').next().unwrap_or(key).to_string();
        let metadata = serde_json::json!({
            "name": name,
            "mimeType": "application/octet-stream",
        });
        let form = reqwest::multipart::Form::new()
            .part(
                "metadata",
                reqwest::multipart::Part::text(metadata.to_string())
                    .mime_str("application/json; charset=UTF-8")
                    .map_err(|error| {
                        UnifiedError::internal(format!(
                            "Cannot build Drive upload metadata part: {error}"
                        ))
                    })?,
            )
            .part(
                "media",
                reqwest::multipart::Part::bytes(data)
                    .file_name(name)
                    .mime_str("application/octet-stream")
                    .map_err(|error| {
                        UnifiedError::internal(format!(
                            "Cannot build Drive upload media part: {error}"
                        ))
                    })?,
            );
        let mut url = Self::api_url("/upload/drive/v3/files")?;
        url.query_pairs_mut().append_pair("uploadType", "multipart");
        let response = self
            .client
            .post(url)
            .bearer_auth(&token)
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Google Drive upload failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("Google Drive", "upload", status));
        }
        let body: Value = response.json().await.map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot parse Google Drive upload response: {error}"
            ))
        })?;
        body.get("id")
            .and_then(|id| id.as_str())
            .map(|id| id.to_string())
            .ok_or_else(|| {
                UnifiedError::internal("Google Drive upload response has no file id")
            })
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        validate_key(key)?;
        let file_id = self.resolve_file_id(key).await?;
        let token = self.access_token().await?;
        let mut url = Self::api_url(&format!(
            "/drive/v3/files/{}",
            percent_encode(&file_id)
        ))?;
        url.query_pairs_mut().append_pair("alt", "media");
        let response = self
            .client
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Google Drive download failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("Google Drive", "download", status));
        }
        response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot read Google Drive response body: {error}"
            ))
        })
    }

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        validate_key(key)?;
        let file_id = self.resolve_file_id(key).await?;
        let token = self.access_token().await?;
        let url = Self::api_url(&format!(
            "/drive/v3/files/{}",
            percent_encode(&file_id)
        ))?;
        let response = self
            .client
            .delete(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Google Drive delete failed: {error}"))
            })?;
        let status = response.status();
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(map_provider_status("Google Drive", "delete", status))
        }
    }

    async fn signed_url(&self, key: &str, _expires_secs: u64) -> Result<String, UnifiedError> {
        validate_key(key)?;
        // See the struct documentation: Drive has no time-limited
        // presigned URLs, so `expires_secs` cannot be enforced. The
        // returned link downloads the file only while it is shared
        // with link access.
        let file_id = self.resolve_file_id(key).await?;
        Ok(format!(
            "https://drive.google.com/uc?export=download&id={}",
            percent_encode(&file_id)
        ))
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        let token = self.access_token().await?;
        let mut url = Self::api_url("/drive/v3/about")?;
        url.query_pairs_mut().append_pair("fields", "user");
        let response = self
            .client
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!(
                    "Google Drive connection test failed: {error}"
                ))
            })?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(map_provider_status(
                "Google Drive",
                "connection test",
                status,
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// SFTP provider (system `sftp` client).
// ---------------------------------------------------------------------------
//
// Added by the storage-manager module (2026-11-22). Delimited section:
// everything between this header and the "Target configuration"
// header below belongs to the SFTP provider.
//
// There is no SSH/SFTP client library in the dependency set, so this
// provider runs the system OpenSSH `sftp` binary in batch mode — the
// same approach the service monitor uses for remote checks over the
// system `ssh` binary, and the backup module uses for `pg_dump`.
// Blocking process work runs inside `tokio::task::spawn_blocking`,
// following the mail module's delivery pattern.
//
// Authentication:
//   - Keyfile: the private-key *path* is stored (never the key
//     material). `sftp` is invoked with `-i <keyfile>` and
//     `BatchMode=yes`, so no interactive prompt is ever possible.
//   - Password: the password is passed to `sshpass` through the
//     `SSHPASS` environment variable only — never on a command line,
//     where it would show in a process listing — following the
//     service monitor's pattern. When `sshpass` is not installed,
//     password authentication is refused with a clear error and
//     keyfile authentication is the way in. The password itself is
//     held encrypted by the caller (the storage-manager module
//     encrypts bucket credentials); this provider only ever sees the
//     decrypted value in memory for the duration of one call.
//
// Host validation (SSRF safety): the host must be an exact match in
// the operator-configured allowlist (`allowed_hosts`, the same list
// the S3 provider uses for extra hosts). There is no suffix rule
// for SFTP — there is no known provider host family to match
// against. IP-literal hosts that are loopback, private, link-local,
// unspecified, or multicast are rejected outright, even when
// allowlisted. The host is never taken from a request: callers pass
// it from stored bucket configuration only, and `StrictHostKeyChecking`
// stays enabled so a man-in-the-middle cannot be silently accepted.
//
// `signed_url` matches the local provider's documented behaviour:
// SFTP has no signing mechanism, so it returns an error telling the
// caller to download through the storage API instead (the
// storage-manager module mints revocable hub download tokens for
// exactly that path).
//
// Honest limits, stated plainly:
//   - No overall transfer timeout: `ConnectTimeout=10` bounds the
//     connect only. A stalled transfer blocks one `spawn_blocking`
//     worker until the remote side closes or the operator kills the
//     process. Do not use this provider for multi-gigabyte objects.
//   - No multipart / resume support: uploads and downloads are
//     single-shot through a temporary file.
//   - No server-side encryption flag: transport encryption is SSH;
//     at-rest encryption depends on the remote server's filesystem.
//   - Directory creation is best-effort (`-mkdir`, errors ignored):
//     a missing parent is created when possible, and the following
//     `put` still fails loudly when it is not.

/// True when `host` is an exact match in the operator-configured
/// allowlist (case-insensitive). No suffix matching: SFTP has no
/// known provider host family.
pub fn is_allowed_sftp_host(host: &str, allowed_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowed_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host)
}

/// Validate an SFTP host against the allowlist. Rejects empty hosts,
/// hosts starting with `-` (option injection into the `sftp`
/// command line), malformed hosts, and IP literals that are
/// loopback, private, link-local, unspecified, or multicast — even
/// when allowlisted. A hostname that is not allowlisted is refused.
pub fn validate_sftp_host(host: &str, allowed_hosts: &[String]) -> UnifiedResult<()> {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return Err(UnifiedError::bad_request(
            "SFTP host must be 1..=253 characters",
        ));
    }
    if host.starts_with('-') {
        return Err(UnifiedError::bad_request(
            "SFTP host must not start with '-'",
        ));
    }
    if let Ok(addr) = host.parse::<IpAddr>() {
        let forbidden = match addr {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_multicast()
                    || v4.is_broadcast()
            }
            IpAddr::V6(v6) => {
                // Segment checks instead of the newer `is_unique_local`
                // / `is_unicast_link_local` helpers: loopback, the
                // unspecified address, multicast (ff00::/8), unique
                // local (fc00::/7), and link local (fe80::/10).
                let segments = v6.segments();
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (segments[0] & 0xfe00) == 0xfc00
                    || (segments[0] & 0xffc0) == 0xfe80
            }
        };
        if forbidden {
            return Err(UnifiedError::forbidden(
                "SFTP hosts on loopback, private, link-local, or multicast addresses are not allowed",
            ));
        }
    } else {
        // A hostname: labels of letters, digits, and `-`, separated
        // by `.`; a `:` means the caller passed something that looks
        // like an address but does not parse as one.
        if host.contains(':') {
            return Err(UnifiedError::bad_request(
                "SFTP host is not a valid hostname or IP address",
            ));
        }
        let valid = host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
        if !valid {
            return Err(UnifiedError::bad_request(
                "SFTP host is not a valid hostname",
            ));
        }
    }
    if !is_allowed_sftp_host(host, allowed_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "SFTP host \"{host}\" is not allowlisted"
        )));
    }
    Ok(())
}

/// Validate an SFTP username: 1..=64 characters, letters, digits,
/// `_`, `-`, `.`, never starting with `-`.
fn validate_sftp_username(username: &str) -> UnifiedResult<()> {
    if username.is_empty()
        || username.len() > 64
        || username.starts_with('-')
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(UnifiedError::bad_request(
            "SFTP username must be 1..=64 characters of letters, digits, '_', '-' or '.'",
        ));
    }
    Ok(())
}

/// Validate an SFTP keyfile path: an absolute path, no NUL bytes.
fn validate_sftp_keyfile(path: &str) -> UnifiedResult<()> {
    if !path.starts_with('/') || path.contains('\0') || path.len() > 4096 {
        return Err(UnifiedError::bad_request(
            "SFTP keyfile path must be an absolute path",
        ));
    }
    Ok(())
}

/// Validate an SFTP base path: empty (the remote home directory) or
/// an absolute remote path whose segments are never `.` or `..`.
fn validate_sftp_base_path(base_path: &str) -> UnifiedResult<()> {
    if base_path.is_empty() {
        return Ok(());
    }
    if !base_path.starts_with('/') {
        return Err(UnifiedError::bad_request(
            "SFTP base path must be absolute (start with '/') or empty",
        ));
    }
    if base_path.chars().any(|c| c.is_control()) || base_path.contains('\\') {
        return Err(UnifiedError::bad_request(
            "SFTP base path must not contain control characters or backslashes",
        ));
    }
    for segment in base_path.split('/') {
        if segment == "." || segment == ".." {
            return Err(UnifiedError::bad_request(
                "SFTP base path must not contain \".\" or \"..\" segments",
            ));
        }
    }
    Ok(())
}

/// Validate an SFTP list prefix: empty (the base path) or a
/// key-like prefix; a trailing `/` (directory form) is allowed.
fn validate_sftp_prefix(prefix: &str) -> UnifiedResult<()> {
    let clean = prefix.trim_end_matches('/');
    if clean.is_empty() {
        return Ok(());
    }
    validate_key(clean)
}

/// Configuration for `SftpProvider`.
#[derive(Debug, Clone)]
pub struct SftpConfig {
    /// Remote host. Must be allowlisted (see `validate_sftp_host`).
    pub host: String,
    /// Remote port. `0` means the default SSH port 22.
    pub port: u16,
    pub username: String,
    /// `"keyfile"` or `"password"`.
    pub auth_kind: String,
    /// Absolute path to the private key file (keyfile auth only).
    /// Only the path is stored, never the key material.
    pub keyfile: Option<String>,
    /// Password (password auth only). Never logged, never returned
    /// by any API response; passed to `sshpass` via the `SSHPASS`
    /// environment variable only.
    pub password: String,
    /// Remote base directory. Empty means the remote home directory.
    pub base_path: String,
    /// Operator-configured allowlist of exact hostnames. The host
    /// must match one entry; there is no suffix rule for SFTP.
    pub allowed_hosts: Vec<String>,
}

impl SftpConfig {
    /// Effective port: the configured port, or 22 when it is 0.
    pub fn effective_port(&self) -> u16 {
        if self.port == 0 {
            22
        } else {
            self.port
        }
    }
}

/// One blocking `sftp` invocation, run inside `spawn_blocking`.
/// `batch` is the `-b -` script fed on stdin. Returns the process
/// output; the caller decides whether a non-zero exit is an error
/// (bulk operations use `-`-prefixed commands whose failures are
/// ignored by `sftp` itself, so the exit status stays 0 for them).
fn run_sftp_blocking(
    host: String,
    port: u16,
    username: String,
    auth_kind: String,
    keyfile: Option<String>,
    password: String,
    batch: String,
) -> UnifiedResult<std::process::Output> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let target = format!("{username}@{host}");
    let mut command = if auth_kind == "password" {
        let sshpass_ok = Command::new("sshpass")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok();
        if !sshpass_ok {
            return Err(UnifiedError::internal(
                "SFTP password authentication needs the sshpass helper, which is not installed; use keyfile authentication instead",
            ));
        }
        let mut wrapped = Command::new("sshpass");
        // The password travels in the environment only, never on a
        // command line (where it would show in a process listing).
        wrapped.arg("-e").env("SSHPASS", password).arg("sftp");
        wrapped
    } else {
        Command::new("sftp")
    };
    command
        .arg("-P")
        .arg(port.to_string())
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-b")
        .arg("-");
    if let Some(keyfile) = &keyfile {
        command.arg("-i").arg(keyfile);
    }
    command.arg(&target);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        UnifiedError::internal(format!("Cannot start the sftp client: {error}"))
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(batch.as_bytes());
        // Dropping stdin closes it, so `sftp -b -` sees end of input.
    }
    child.wait_with_output().map_err(|error| {
        UnifiedError::internal(format!("Cannot run the sftp client: {error}"))
    })
}

/// SFTP storage over the system OpenSSH `sftp` client (see the
/// section header for the honest limits).
pub struct SftpProvider {
    config: SftpConfig,
}

impl SftpProvider {
    pub fn new(config: SftpConfig) -> UnifiedResult<Self> {
        validate_sftp_host(&config.host, &config.allowed_hosts)?;
        validate_sftp_username(&config.username)?;
        validate_sftp_base_path(&config.base_path)?;
        match config.auth_kind.as_str() {
            "keyfile" => {
                let Some(keyfile) = &config.keyfile else {
                    return Err(UnifiedError::bad_request(
                        "SFTP keyfile authentication requires a keyfile path",
                    ));
                };
                validate_sftp_keyfile(keyfile)?;
            }
            "password" => {
                if config.keyfile.is_some() {
                    return Err(UnifiedError::bad_request(
                        "SFTP password authentication does not take a keyfile path",
                    ));
                }
                if config.password.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "SFTP password authentication requires a password",
                    ));
                }
            }
            _ => {
                return Err(UnifiedError::bad_request(
                    "SFTP auth_kind must be \"keyfile\" or \"password\"",
                ));
            }
        }
        let config = SftpConfig {
            host: config.host.trim().to_ascii_lowercase(),
            port: config.effective_port(),
            username: config.username.trim().to_string(),
            auth_kind: config.auth_kind,
            keyfile: config.keyfile,
            password: config.password,
            base_path: config.base_path.trim().to_string(),
            allowed_hosts: config.allowed_hosts,
        };
        Ok(Self { config })
    }

    /// Remote path for a validated storage key under the base path.
    fn remote_path(&self, key: &str) -> UnifiedResult<String> {
        validate_key(key)?;
        if key.ends_with(".sha256") {
            return Err(UnifiedError::bad_request(
                "Storage keys ending in \".sha256\" are reserved for integrity sidecars",
            ));
        }
        if self.config.base_path.is_empty() {
            Ok(key.to_string())
        } else {
            Ok(format!(
                "{}/{}",
                self.config.base_path.trim_end_matches('/'),
                key
            ))
        }
    }

    /// Remote directory that holds `key` (the base path when the key
    /// has no directory part).
    fn remote_dir(&self, key: &str) -> UnifiedResult<String> {
        let remote = self.remote_path(key)?;
        match remote.rfind('/') {
            Some(index) if index > 0 => Ok(remote[..index].to_string()),
            _ => Ok(self.config.base_path.clone()),
        }
    }

    /// Run one `sftp` batch script on a blocking worker thread.
    async fn run_batch(&self, batch: String) -> UnifiedResult<std::process::Output> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            run_sftp_blocking(
                config.host,
                config.port,
                config.username,
                config.auth_kind,
                config.keyfile,
                config.password,
                batch,
            )
        })
        .await
        .map_err(|_| UnifiedError::internal("SFTP worker task failed"))?
    }

    /// Fail unless the `sftp` invocation exited successfully. The
    /// remote error text is never echoed: it can contain server
    /// paths and internal details.
    fn require_success(output: &std::process::Output, operation: &str) -> UnifiedResult<()> {
        if output.status.success() {
            Ok(())
        } else {
            Err(UnifiedError::internal(format!(
                "SFTP {operation} failed with exit status {}",
                output.status
            )))
        }
    }

    /// Temporary local path for one transfer. The caller removes it
    /// when done; the name carries a UUID so concurrent transfers
    /// never collide.
    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(".unified-sftp-{}", uuid::Uuid::now_v7()))
    }
}

#[async_trait]
impl StorageProvider for SftpProvider {
    fn kind(&self) -> StorageKind {
        StorageKind::Sftp
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        let remote = self.remote_path(key)?;
        let remote_dir = self.remote_dir(key)?;
        let checksum = sha256_hex(&data);
        // Best-effort parent creation: `-mkdir` ignores the error
        // when a directory already exists, so the batch does not
        // abort before the `put`.
        let mut batch = String::new();
        if !remote_dir.is_empty() {
            let mut current = String::new();
            for segment in remote_dir.split('/').filter(|part| !part.is_empty()) {
                current.push('/');
                current.push_str(segment);
                batch.push_str(&format!("-mkdir {current}\n"));
            }
        }
        let temp_path = Self::temp_path();
        std::fs::write(&temp_path, &data).map_err(|error| {
            UnifiedError::internal(format!("Cannot stage SFTP upload: {error}"))
        })?;
        // The checksum travels as a sidecar file (`<key>.sha256`)
        // next to the object, so `download` can verify the bytes
        // even when the caller holds no checksum of its own.
        let checksum_path = Self::temp_path();
        std::fs::write(&checksum_path, checksum.as_bytes()).map_err(|error| {
            let _ = std::fs::remove_file(&temp_path);
            UnifiedError::internal(format!("Cannot stage SFTP checksum sidecar: {error}"))
        })?;
        batch.push_str(&format!(
            "put {} {}\n",
            temp_path.display(),
            remote
        ));
        batch.push_str(&format!(
            "put {} {}.sha256\n",
            checksum_path.display(),
            remote
        ));
        let output = self.run_batch(batch).await;
        let _ = std::fs::remove_file(&temp_path);
        let _ = std::fs::remove_file(&checksum_path);
        let output = output?;
        if let Err(error) = Self::require_success(&output, "upload") {
            // Do not leave an object without its sidecar behind.
            let _ = self
                .run_batch(format!("-rm {remote}\n-rm {remote}.sha256\n"))
                .await;
            return Err(error);
        }
        Ok(key.to_string())
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        let remote = self.remote_path(key)?;
        let temp_path = Self::temp_path();
        let checksum_path = Self::temp_path();
        // `-get` ignores the error when no sidecar exists yet
        // (objects written before sidecars), so legacy objects
        // still download — unverified, like the local provider.
        let batch = format!(
            "-get {remote}.sha256 {}\nget {} {}\n",
            checksum_path.display(),
            remote,
            temp_path.display()
        );
        let output = self.run_batch(batch).await?;
        Self::require_success(&output, "download")?;
        let expected = std::fs::read_to_string(&checksum_path)
            .ok()
            .map(|raw| raw.trim().to_ascii_lowercase())
            .filter(|digest| digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()));
        let _ = std::fs::remove_file(&checksum_path);
        let data = std::fs::read(&temp_path).map_err(|error| {
            let _ = std::fs::remove_file(&temp_path);
            UnifiedError::internal(format!("Cannot read staged SFTP download: {error}"))
        })?;
        let _ = std::fs::remove_file(&temp_path);
        if let Some(expected) = expected {
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

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        let remote = self.remote_path(key)?;
        // Idempotent, matching S3 delete semantics: `-rm` ignores
        // the error when the object is already gone. The sidecar
        // goes with the object so a later object under the same key
        // is never verified against a stale checksum.
        let batch = format!("-rm {remote}\n-rm {remote}.sha256\n");
        let output = self.run_batch(batch).await?;
        Self::require_success(&output, "delete")?;
        Ok(())
    }

    async fn signed_url(&self, key: &str, _expires_secs: u64) -> Result<String, UnifiedError> {
        validate_key(key)?;
        // SFTP has no signing mechanism; serve objects through the
        // storage API instead (the storage-manager module mints
        // revocable hub download tokens for exactly that path).
        Err(UnifiedError::bad_request(
            "SFTP storage does not issue signed URLs; download the object through the storage API instead",
        ))
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        let dir = if self.config.base_path.is_empty() {
            ".".to_string()
        } else {
            self.config.base_path.clone()
        };
        let batch = format!("ls -1 {dir}\n");
        let output = self.run_batch(batch).await?;
        Self::require_success(&output, "connection test")?;
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, UnifiedError> {
        validate_sftp_prefix(prefix)?;
        let clean = prefix.trim_end_matches('/');
        let dir = if clean.is_empty() {
            if self.config.base_path.is_empty() {
                ".".to_string()
            } else {
                self.config.base_path.clone()
            }
        } else if self.config.base_path.is_empty() {
            clean.to_string()
        } else {
            format!("{}/{}", self.config.base_path.trim_end_matches('/'), clean)
        };
        let batch = format!("ls -1 {dir}\n");
        let output = self.run_batch(batch).await?;
        Self::require_success(&output, "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut keys: Vec<String> = Vec::new();
        for line in stdout.lines() {
            let name = line.trim();
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }
            // Integrity sidecars (`<key>.sha256`) are not objects.
            if name.ends_with(".sha256") {
                continue;
            }
            // `ls -1` prints one name per line; a name is a single
            // path segment, so it cannot escape the listed directory.
            let key = if clean.is_empty() {
                name.to_string()
            } else {
                format!("{clean}/{name}")
            };
            keys.push(key);
        }
        keys.sort();
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// Target configuration.
// ---------------------------------------------------------------------------

/// One storage target, as declared in configuration or created through
/// the storage API. Secrets are stored in this struct only in memory;
/// they are never logged and never serialized into API responses —
/// use `masked_config` for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageTargetConfig {
    pub name: String,
    pub kind: StorageKind,
    /// Endpoint URL (S3 / R2). Ignored for local and Drive targets.
    pub endpoint: String,
    /// Region (S3 / R2). `auto` for R2.
    pub region: String,
    /// Bucket (S3 / R2).
    pub bucket: String,
    /// Base directory (local provider). Defaults to `data/storage`.
    pub base_dir: String,
    /// Access key (S3 / R2). Masked in API responses.
    pub access_key: String,
    /// Secret key (S3 / R2). Masked in API responses, never logged.
    pub secret_key: String,
    /// OAuth2 client id (Google Drive).
    pub client_id: String,
    /// OAuth2 client secret (Google Drive). Masked, never logged.
    pub client_secret: String,
    /// OAuth2 refresh token (Google Drive). Masked, never logged.
    pub refresh_token: String,
    /// OAuth2 access token (Google Drive). Masked, never logged.
    pub access_token: String,
    /// OAuth2 token endpoint (Google Drive). Empty means the default
    /// Google endpoint.
    pub token_url: String,
    /// SFTP host. Must be allowlisted (see `validate_sftp_host`).
    pub sftp_host: String,
    /// SFTP port. `0` means the default SSH port 22.
    pub sftp_port: u16,
    /// SFTP username.
    pub sftp_username: String,
    /// SFTP auth kind: `"keyfile"` or `"password"`.
    pub sftp_auth_kind: String,
    /// SFTP private-key file path (keyfile auth only). Only the path
    /// is stored, never the key material.
    pub sftp_keyfile: String,
    /// SFTP password (password auth only). Masked in API responses,
    /// never logged.
    pub sftp_password: String,
    /// SFTP remote base directory. Empty means the remote home
    /// directory.
    pub sftp_base_path: String,
    /// Optional hard quota for this target, in bytes. `None` means
    /// no target-level limit (the storage-manager quotas and plan
    /// quotas still apply on the hub upload path). Enforced by the
    /// storage module on every upload through the target.
    pub quota_bytes: Option<i64>,
    /// Optional hard quota for this target, in objects. `None`
    /// means no target-level limit. `Some(0)` means no objects may
    /// be stored through the target.
    pub max_objects: Option<i64>,
}

impl Default for StorageTargetConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: StorageKind::Local,
            endpoint: String::new(),
            region: String::new(),
            bucket: String::new(),
            base_dir: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            refresh_token: String::new(),
            access_token: String::new(),
            token_url: String::new(),
            sftp_host: String::new(),
            sftp_port: 0,
            sftp_username: String::new(),
            sftp_auth_kind: String::new(),
            sftp_keyfile: String::new(),
            sftp_password: String::new(),
            sftp_base_path: String::new(),
            quota_bytes: None,
            max_objects: None,
        }
    }
}

impl StorageTargetConfig {
    /// Validate the target: name, kind, and the fields its kind
    /// requires. Secret values are checked for presence only — their
    /// values never appear in any error message.
    pub fn validate(&self) -> UnifiedResult<()> {
        validate_target_name(&self.name)?;
        match self.kind {
            StorageKind::Local => Ok(()),
            StorageKind::S3 => {
                if self.endpoint.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "S3 target requires an endpoint",
                    ));
                }
                if self.region.trim().is_empty() {
                    return Err(UnifiedError::bad_request("S3 target requires a region"));
                }
                if self.bucket.trim().is_empty() {
                    return Err(UnifiedError::bad_request("S3 target requires a bucket"));
                }
                if self.access_key.trim().is_empty() || self.secret_key.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "S3 target requires an access key and a secret key",
                    ));
                }
                Ok(())
            }
            StorageKind::R2 => {
                if self.endpoint.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "R2 target requires an endpoint",
                    ));
                }
                if self.bucket.trim().is_empty() {
                    return Err(UnifiedError::bad_request("R2 target requires a bucket"));
                }
                if self.access_key.trim().is_empty() || self.secret_key.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "R2 target requires an access key and a secret key",
                    ));
                }
                Ok(())
            }
            StorageKind::GoogleDrive => {
                if self.client_id.trim().is_empty()
                    || self.client_secret.trim().is_empty()
                {
                    return Err(UnifiedError::bad_request(
                        "Google Drive target requires a client id and a client secret",
                    ));
                }
                if self.refresh_token.trim().is_empty()
                    && self.access_token.trim().is_empty()
                {
                    return Err(UnifiedError::bad_request(
                        "Google Drive target requires a refresh token or an access token",
                    ));
                }
                Ok(())
            }
            StorageKind::Sftp => {
                if self.sftp_host.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "SFTP target requires a host",
                    ));
                }
                if self.sftp_username.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "SFTP target requires a username",
                    ));
                }
                match self.sftp_auth_kind.as_str() {
                    "keyfile" => {
                        if self.sftp_keyfile.trim().is_empty() {
                            return Err(UnifiedError::bad_request(
                                "SFTP keyfile authentication requires a keyfile path",
                            ));
                        }
                    }
                    "password" => {
                        if self.sftp_password.trim().is_empty() {
                            return Err(UnifiedError::bad_request(
                                "SFTP password authentication requires a password",
                            ));
                        }
                    }
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "SFTP target requires auth_kind \"keyfile\" or \"password\"",
                        ));
                    }
                }
                Ok(())
            }
        }?;
        if let Some(quota) = self.quota_bytes {
            if quota < 0 {
                return Err(UnifiedError::bad_request(
                    "Storage target quota_bytes must be zero or greater",
                ));
            }
        }
        if let Some(limit) = self.max_objects {
            if limit < 0 {
                return Err(UnifiedError::bad_request(
                    "Storage target max_objects must be zero or greater",
                ));
            }
        }
        Ok(())
    }

    /// The target as a display value: every secret field is replaced
    /// with its masked form. This is the only shape of a target
    /// configuration that may leave the server.
    pub fn masked_config(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            for field in SECRET_CONFIG_FIELDS {
                if let Some(Value::String(secret)) = map.get(field) {
                    let masked = mask_secret(secret);
                    map.insert(field.to_string(), Value::String(masked));
                }
            }
        }
        value
    }
}

/// Storage section of the application configuration. The integrator
/// adds this as `pub storage: StorageSection` on `AppConfig`
/// (see the integration notes in the build report).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSection {
    /// Name of the default target, when one is configured.
    pub default_target: Option<String>,
    /// Extra allowed endpoint hosts, for self-hosted S3-compatible
    /// endpoints (exact hostnames, for example `minio.example.com`).
    pub allowed_hosts: Vec<String>,
    /// Targets registered at startup.
    pub targets: Vec<StorageTargetConfig>,
}

/// Build one provider from a target configuration.
pub fn provider_from_config(
    target: &StorageTargetConfig,
    allowed_hosts: &[String],
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    target.validate()?;
    match target.kind {
        StorageKind::Local => {
            let base_dir = if target.base_dir.trim().is_empty() {
                PathBuf::from("data/storage")
            } else {
                PathBuf::from(target.base_dir.clone())
            };
            Ok(Arc::new(LocalProvider::new(base_dir)?))
        }
        StorageKind::S3 => Ok(Arc::new(S3Provider::new(
            S3Config {
                endpoint: target.endpoint.clone(),
                region: target.region.clone(),
                bucket: target.bucket.clone(),
                access_key: target.access_key.clone(),
                secret_key: target.secret_key.clone(),
            },
            allowed_hosts,
        )?)),
        StorageKind::R2 => Ok(Arc::new(R2Provider::new(
            R2Config {
                endpoint: target.endpoint.clone(),
                region: target.region.clone(),
                bucket: target.bucket.clone(),
                access_key: target.access_key.clone(),
                secret_key: target.secret_key.clone(),
            },
            allowed_hosts,
        )?)),
        StorageKind::GoogleDrive => Ok(Arc::new(GoogleDriveProvider::new(
            DriveConfig {
                client_id: target.client_id.clone(),
                client_secret: target.client_secret.clone(),
                refresh_token: target.refresh_token.clone(),
                access_token: target.access_token.clone(),
                token_url: target.token_url.clone(),
            },
            allowed_hosts,
        )?)),
        StorageKind::Sftp => Ok(Arc::new(SftpProvider::new(
            SftpConfig {
                host: target.sftp_host.clone(),
                port: target.sftp_port,
                username: target.sftp_username.clone(),
                auth_kind: target.sftp_auth_kind.clone(),
                keyfile: if target.sftp_keyfile.trim().is_empty() {
                    None
                } else {
                    Some(target.sftp_keyfile.clone())
                },
                password: target.sftp_password.clone(),
                base_path: target.sftp_base_path.clone(),
                allowed_hosts: allowed_hosts.to_vec(),
            },
        )?)),
    }
}

/// Build a registry from the storage configuration section, following
/// the `build_cache` pattern: a target that fails validation is
/// skipped with a warning, so one bad target never prevents startup.
pub fn build_storage_registry(config: &StorageSection) -> Arc<StorageRegistry> {
    let registry = Arc::new(StorageRegistry::new());
    for target in &config.targets {
        match provider_from_config(target, &config.allowed_hosts) {
            Ok(provider) => registry.register(target.name.clone(), provider),
            Err(error) => log::warn!(
                "Skipping storage target \"{}\": {}",
                target.name,
                error.message
            ),
        }
    }
    registry
}

// ---------------------------------------------------------------------------
// Hook event names (emitted by the storage module).
// ---------------------------------------------------------------------------

pub mod events {
    pub const TARGET_CREATED: &str = "storage.target.created";
    pub const TARGET_DELETED: &str = "storage.target.deleted";
    pub const OBJECT_UPLOADED: &str = "storage.object.uploaded";
    pub const OBJECT_DELETED: &str = "storage.object.deleted";
    /// Emitted when a download or verification finds bytes whose
    /// SHA-256 does not match the recorded checksum. The corrupt
    /// bytes are never served.
    pub const OBJECT_CHECKSUM_MISMATCH: &str = "storage.object.checksum_mismatch";
}
