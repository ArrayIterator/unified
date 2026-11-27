// storage - drive.
// Google Drive provider.
//

use super::keys::percent_encode;
use super::validation::build_http_client;
use super::validation::map_provider_status;
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

// Google Drive provider.

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
/// `expires_secs` cannot be enforced by Drive - the returned link
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
