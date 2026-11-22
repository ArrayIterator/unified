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
/// (`local`, `s3`, `google_drive`, `r2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    Local,
    S3,
    GoogleDrive,
    R2,
}

impl StorageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::S3 => "s3",
            Self::GoogleDrive => "google_drive",
            Self::R2 => "r2",
        }
    }

    /// All supported kinds, for status responses and validation.
    pub fn all() -> &'static [StorageKind] {
        &[Self::Local, Self::S3, Self::GoogleDrive, Self::R2]
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
            other => Err(UnifiedError::bad_request(format!(
                "Unknown storage kind \"{other}\" (expected local, s3, google_drive or r2)"
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
pub const SECRET_CONFIG_FIELDS: [&str; 5] = [
    "secret_key",
    "access_key",
    "client_secret",
    "refresh_token",
    "access_token",
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
/// of every uploaded object is recorded in memory (`checksum`) and
/// verified on download when present.
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

    /// Resolve a validated key to a path, guaranteeing the result stays
    /// inside the base directory (lexical check plus canonicalization
    /// of any existing parent, so symlinks inside the base cannot be
    /// used to escape it).
    fn resolve(&self, key: &str) -> UnifiedResult<PathBuf> {
        validate_key(key)?;
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
        if let Some(expected) = self.checksum(key) {
            let actual = sha256_hex(&data);
            if actual != expected {
                return Err(UnifiedError::internal(format!(
                    "Stored object \"{key}\" failed its recorded SHA-256 checksum"
                )));
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
        }
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
}
