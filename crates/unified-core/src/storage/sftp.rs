// storage — sftp.
// SFTP provider.
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
