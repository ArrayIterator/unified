// storage — config.
// Storage target configuration and provider construction.
//
// Split from `crates/unified-core/src/storage.rs`; moved verbatim, no behaviour changes.

use super::*;
use crate::error::{UnifiedError, UnifiedResult};
use crate::provider_registry::{ProviderDefinition, provider_registry, valid_provider_id};
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
use std::sync::{Arc, OnceLock, RwLock};
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

    /// The registry definition for this kind, if one is registered
    /// (C12, task 269). Every built-in kind has one; `None` only when
    /// the registry was built without seeds (tests).
    pub fn definition(&self) -> Option<Arc<ProviderDefinition>> {
        provider_registry().get("storage", self.as_str())
    }

    /// Human-readable display name, from the provider registry.
    pub fn display_name(&self) -> String {
        self.definition()
            .map(|def| def.display_name.clone())
            .unwrap_or_else(|| self.as_str().to_string())
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
    /// Registry definition id (C12, task 269) that overrides `kind`
    /// when set: the target is built by the factory registered for
    /// that id instead of the built-in constructor for `kind`. This
    /// is how a provider an admin registered through the provider
    /// registry gets a storage target with no code change. The id
    /// must match a registered definition and factory; unknown ids
    /// fail at build time, never silently fall back to `kind`.
    pub kind_id: Option<String>,
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
            kind_id: None,
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
        // A registry-defined target validates its own configuration
        // in its factory; only the name and a well-formed id are
        // checked here.
        if let Some(kind_id) = self.kind_id.as_deref().filter(|id| !id.trim().is_empty()) {
            if !valid_provider_id(kind_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid storage provider id '{kind_id}' (expected lowercase letters, digits, '-' or '_')"
                )));
            }
            return Ok(());
        }
        match self.kind {
            StorageKind::Local => Ok::<(), UnifiedError>(()),
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
    // A registry-defined target is built by its factory. An unknown
    // id is an honest error naming the id, never a silent fallback
    // to the `kind` constructor.
    if let Some(kind_id) = target.kind_id.as_deref().filter(|id| !id.trim().is_empty()) {
        let factory = storage_kind_factory(kind_id).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown storage provider '{kind_id}': no factory is registered for this id"
            ))
        })?;
        return factory(target, allowed_hosts);
    }
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

// ---------------------------------------------------------------------------
// Registry-defined kinds (C12, task 269).
// ---------------------------------------------------------------------------

/// A factory that builds a storage provider from a target
/// configuration plus the configured extra allowed hosts.
pub type StorageKindFactory = Arc<
    dyn Fn(&StorageTargetConfig, &[String]) -> UnifiedResult<Arc<dyn StorageProvider>>
        + Send
        + Sync,
>;

static STORAGE_KIND_FACTORIES: OnceLock<RwLock<HashMap<String, StorageKindFactory>>> =
    OnceLock::new();

fn storage_kind_factories() -> &'static RwLock<HashMap<String, StorageKindFactory>> {
    STORAGE_KIND_FACTORIES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the factory for a registry definition id.
/// This is how a provider added through the provider registry — no
/// code change, no enum variant — gets a storage target: register
/// its definition, then register its factory under the same id, once
/// at startup or from an extension.
pub fn register_storage_kind_factory(id: &str, factory: StorageKindFactory) -> UnifiedResult<()> {
    if !valid_provider_id(id) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid storage provider id '{id}' (expected lowercase letters, digits, '-' or '_')"
        )));
    }
    storage_kind_factories()
        .write()
        .expect("storage kind factory registry lock poisoned")
        .insert(id.to_string(), factory);
    Ok(())
}

/// The factory registered for a registry definition id, if any.
pub fn storage_kind_factory(id: &str) -> Option<StorageKindFactory> {
    storage_kind_factories()
        .read()
        .expect("storage kind factory registry lock poisoned")
        .get(id)
        .cloned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_storage_kinds_come_from_registry_data() {
        assert_eq!(StorageKind::S3.display_name(), "Amazon S3");
        assert_eq!(StorageKind::Local.display_name(), "Local Filesystem");
        assert_eq!(StorageKind::GoogleDrive.display_name(), "Google Drive");
        let s3 = StorageKind::S3.definition().unwrap();
        assert!(s3.capability("upload"));
        assert!(!s3.capability("list"));
        assert!(StorageKind::Local.definition().unwrap().capability("list"));
        assert!(StorageKind::Sftp.definition().unwrap().capability("list"));
        assert!(
            !StorageKind::Sftp
                .definition()
                .unwrap()
                .capability("signed_url")
        );
    }

    #[test]
    fn add_one_storage_provider_without_code_change() {
        // Register a definition + a factory, then build a target that
        // references it by kind_id only. The factory below builds a
        // LocalProvider stand-in: the point is the dispatch, not a
        // new wire protocol. No enum variant exists for the new id
        // and no match arm was added.
        let mut def = ProviderDefinition::new("storage", "acme_store", "Acme Store", "local");
        def.capabilities = serde_json::json!({
            "upload": true, "download": true, "delete": true,
            "signed_url": false, "list": true, "test_connection": true
        });
        provider_registry().register(def).unwrap();
        register_storage_kind_factory(
            "acme_store",
            Arc::new(|target, _allowed| {
                Ok(Arc::new(LocalProvider::new(PathBuf::from(
                    target.base_dir.clone(),
                ))?))
            }),
        )
        .unwrap();
        let dir = std::env::temp_dir().join("unified-c12-acme-store");
        let mut target = StorageTargetConfig::default();
        target.name = "acme".to_string();
        target.kind_id = Some("acme_store".to_string());
        target.base_dir = dir.to_string_lossy().into_owned();
        let provider = provider_from_config(&target, &[]).unwrap();
        assert_eq!(provider.kind(), StorageKind::Local);
        // An unknown id fails loudly, never falls back to `kind`.
        target.kind_id = Some("no_such_store".to_string());
        assert!(provider_from_config(&target, &[]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
