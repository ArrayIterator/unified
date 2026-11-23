// Third-party VCS integrations — Provider abstraction, provider API types, and release parsing.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Provider abstraction.
// ---------------------------------------------------------------------------

/// A third-party version-control hosting provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    GitHub,
    GitLab,
    Bitbucket,
}

impl Provider {
    /// Parse a provider name from a path segment or filter value.
    pub fn parse(value: &str) -> UnifiedResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "github" => Ok(Self::GitHub),
            "gitlab" => Ok(Self::GitLab),
            "bitbucket" => Ok(Self::Bitbucket),
            _ => Err(UnifiedError::bad_request(format!(
                "Unknown VCS provider \"{value}\" (expected github, gitlab or bitbucket)"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
            Self::Bitbucket => "bitbucket",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Bitbucket => "Bitbucket",
        }
    }

    /// Default API base URL for the provider.
    pub fn default_api_base(&self) -> &'static str {
        match self {
            Self::GitHub => "https://api.github.com",
            Self::GitLab => "https://gitlab.com/api/v4",
            Self::Bitbucket => "https://api.bitbucket.org/2.0",
        }
    }

    /// Header name carrying the credential for this provider.
    /// GitLab personal access tokens use `Private-Token`; GitHub and
    /// Bitbucket use a Bearer token.
    pub fn auth_header_name(&self) -> header::HeaderName {
        match self {
            Self::GitLab => header::HeaderName::from_static("private-token"),
            _ => header::AUTHORIZATION,
        }
    }

    /// Header value for a credential. The token itself is never logged.
    pub fn auth_header_value(&self, token: &str) -> String {
        match self {
            Self::GitLab => token.to_string(),
            _ => format!("Bearer {token}"),
        }
    }

    /// Header carrying the webhook signature for this provider, if any.
    pub fn webhook_signature_header(&self) -> &'static str {
        match self {
            Self::GitHub => "x-hub-signature-256",
            Self::GitLab => "x-gitlab-token",
            // Bitbucket provides no signature scheme; the module checks
            // the configured shared secret in this custom header.
            Self::Bitbucket => "x-webhook-secret",
        }
    }
}

/// Behaviour every provider must expose: where its API lives, how it
/// authenticates, and where its releases and tags are listed.
pub trait VcsProvider {
    /// API base URL, honouring a settings override and, for GitLab, a
    /// self-hosted instance host.
    fn api_base(&self, settings: Option<&ProviderSettings>, host: Option<&str>) -> String;
    /// Short description of the auth scheme, for API responses.
    fn auth_scheme(&self) -> &'static str;
    /// Path (relative to the API base) listing releases.
    fn releases_path(&self, owner: &str, repo: &str) -> String;
    /// Path (relative to the API base) listing tags.
    fn tags_path(&self, owner: &str, repo: &str) -> String;
}

impl VcsProvider for Provider {
    fn api_base(&self, settings: Option<&ProviderSettings>, host: Option<&str>) -> String {
        if let Some(settings) = settings {
            if let Some(base) = &settings.api_base_override {
                if !base.trim().is_empty() {
                    return base.trim().trim_end_matches('/').to_string();
                }
            }
        }
        if let Self::GitLab = self {
            if let Some(host) = host.filter(|host| !host.trim().is_empty()) {
                return format!("https://{}/api/v4", host.trim());
            }
        }
        self.default_api_base().to_string()
    }

    fn auth_scheme(&self) -> &'static str {
        match self {
            Self::GitLab => "private-token",
            _ => "bearer",
        }
    }

    fn releases_path(&self, owner: &str, repo: &str) -> String {
        match self {
            Self::GitHub => format!("/repos/{owner}/{repo}/releases"),
            Self::GitLab => format!(
                "/projects/{}/releases",
                encode_path_segment(&format!("{owner}/{repo}"))
            ),
            // Bitbucket has no releases API; tags are the release list.
            Self::Bitbucket => self.tags_path(owner, repo),
        }
    }

    fn tags_path(&self, owner: &str, repo: &str) -> String {
        match self {
            Self::GitHub => format!("/repos/{owner}/{repo}/tags"),
            Self::GitLab => format!(
                "/projects/{}/repository/tags",
                encode_path_segment(&format!("{owner}/{repo}"))
            ),
            Self::Bitbucket => format!("/repositories/{owner}/{repo}/refs/tags"),
        }
    }
}

/// Percent-encode one path segment (everything except the unreserved
/// set). Used for GitLab project paths, where `owner/repo` becomes a
/// single URL-encoded segment.
pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Provider settings (write-only credentials).
// ---------------------------------------------------------------------------

/// Per-provider settings. This struct deliberately does NOT implement
/// `Serialize`: the token and webhook secret must never be serialized
/// into a response, a log line, or a hook payload.
#[derive(Debug, Clone, Default)]
pub struct ProviderSettings {
    pub token: Option<String>,
    pub webhook_secret: Option<String>,
    pub api_base_override: Option<String>,
    pub gitlab_hosts: Vec<String>,
}

/// Public view of a provider's settings: configuration flags only.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderSettingsView {
    pub provider: String,
    pub configured: bool,
    pub webhook_configured: bool,
    pub api_base: String,
    pub gitlab_hosts: Vec<String>,
}

/// Public view of a provider.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub provider: String,
    pub display_name: String,
    pub api_base: String,
    pub auth_scheme: String,
    pub webhook_signature_header: String,
    pub configured: bool,
    pub webhook_configured: bool,
}

// ---------------------------------------------------------------------------
// Provider API: releases and tags.
// ---------------------------------------------------------------------------

/// One release or tag as returned by a provider.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRelease {
    pub tag: String,
    /// Semver parsed from the tag, when the tag is a valid version.
    pub version: Option<String>,
    pub name: Option<String>,
    pub notes: String,
    pub published_at: Option<DateTime<Utc>>,
    pub prerelease: bool,
    pub assets: Vec<ProviderAsset>,
}

/// One downloadable file attached to a provider release.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderAsset {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
}

/// Releases fetched from a provider, with the rate-limit state the
/// provider reported (surfaced in response metadata, never as an
/// error unless the provider actually failed).
pub struct ProviderReleases {
    pub items: Vec<ProviderRelease>,
    pub rate_limit: RateLimitInfo,
}

pub(crate) fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .map(str::to_string)
}

pub(crate) fn parse_release_list(provider: Provider, body: &Value) -> Vec<ProviderRelease> {
    let values: Vec<&Value> = match provider {
        Provider::Bitbucket => body
            .get("values")
            .and_then(|values| values.as_array())
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
        _ => body
            .as_array()
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        match provider {
            Provider::GitHub => {
                // Skip provider-side drafts: only real releases are
                // importable.
                if value
                    .get("draft")
                    .and_then(|field| field.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }
                let Some(tag) = string_field(value, "tag_name") else {
                    continue;
                };
                let assets = value
                    .get("assets")
                    .and_then(|assets| assets.as_array())
                    .map(|assets| {
                        assets
                            .iter()
                            .filter_map(|asset| {
                                Some(ProviderAsset {
                                    name: string_field(asset, "name")?,
                                    url: string_field(asset, "browser_download_url")
                                        .or_else(|| string_field(asset, "url"))?,
                                    size: asset.get("size").and_then(|size| size.as_u64()),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(value, "name"),
                    notes: string_field(value, "body").unwrap_or_default(),
                    published_at: parse_datetime(
                        value.get("published_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: value
                        .get("prerelease")
                        .and_then(|field| field.as_bool())
                        .unwrap_or(false),
                    assets,
                });
            }
            Provider::GitLab => {
                let Some(tag) = string_field(value, "tag_name") else {
                    continue;
                };
                let assets = value
                    .get("assets")
                    .and_then(|assets| assets.get("links"))
                    .and_then(|links| links.as_array())
                    .map(|links| {
                        links
                            .iter()
                            .filter_map(|link| {
                                Some(ProviderAsset {
                                    name: string_field(link, "name")?,
                                    url: string_field(link, "url")?,
                                    size: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(value, "name"),
                    notes: string_field(value, "description").unwrap_or_default(),
                    published_at: parse_datetime(
                        value.get("released_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: false,
                    assets,
                });
            }
            Provider::Bitbucket => {
                let Some(tag) = string_field(value, "name") else {
                    continue;
                };
                let published_at = value
                    .get("target")
                    .and_then(|target| target.get("date"))
                    .and_then(|date| date.as_str());
                out.push(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: None,
                    notes: String::new(),
                    published_at: parse_datetime(published_at),
                    prerelease: false,
                    assets: Vec::new(),
                });
            }
        }
    }
    out
}

pub(crate) fn parse_tag_list(provider: Provider, body: &Value) -> Vec<ProviderRelease> {
    let values: Vec<&Value> = match provider {
        Provider::Bitbucket => body
            .get("values")
            .and_then(|values| values.as_array())
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
        _ => body
            .as_array()
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let (tag, published_at) = match provider {
            Provider::GitHub => (
                string_field(value, "name"),
                value
                    .get("commit")
                    .and_then(|commit| commit.get("created_at"))
                    .and_then(|date| date.as_str())
                    .map(str::to_string),
            ),
            Provider::GitLab => (
                string_field(value, "name"),
                value
                    .get("commit")
                    .and_then(|commit| commit.get("created_at"))
                    .and_then(|date| date.as_str())
                    .map(str::to_string),
            ),
            Provider::Bitbucket => (
                string_field(value, "name"),
                value
                    .get("target")
                    .and_then(|target| target.get("date"))
                    .and_then(|date| date.as_str())
                    .map(str::to_string),
            ),
        };
        let Some(tag) = tag else { continue };
        out.push(ProviderRelease {
            version: version_from_tag(&tag).ok().map(|version| version.to_string()),
            tag,
            name: None,
            notes: String::new(),
            published_at: parse_datetime(published_at.as_deref()),
            prerelease: false,
            assets: Vec::new(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_release_list_github_skips_drafts_and_maps_assets() {
        let body = json!([
            {"tag_name": "v0.9.0", "draft": true, "name": "Draft", "body": "wip"},
            {"tag_name": "v1.2.3", "draft": false, "name": "Release 1.2.3", "body": "Notes here",
             "published_at": "2026-01-01T00:00:00Z", "prerelease": false,
             "assets": [{"name": "a.zip", "browser_download_url": "https://github.com/o/r/a.zip", "size": 42}]},
            {"name": "no tag here"}
        ]);
        let releases = parse_release_list(Provider::GitHub, &body);
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].tag, "v1.2.3");
        assert_eq!(releases[0].version.as_deref(), Some("1.2.3"));
        assert_eq!(releases[0].name.as_deref(), Some("Release 1.2.3"));
        assert_eq!(releases[0].notes, "Notes here");
        assert!(!releases[0].prerelease);
        assert_eq!(releases[0].assets.len(), 1);
        assert_eq!(releases[0].assets[0].name, "a.zip");
        assert_eq!(releases[0].assets[0].url, "https://github.com/o/r/a.zip");
        assert_eq!(releases[0].assets[0].size, Some(42));
    }

    #[test]
    fn parse_release_list_gitlab_reads_tag_links() {
        let body = json!([
            {"tag_name": "v2.0.0", "name": "Two", "description": "Desc",
             "released_at": "2026-02-01T00:00:00Z",
             "assets": {"links": [{"name": "b.tar.gz", "url": "https://gitlab.com/o/r/b.tar.gz"}]}}
        ]);
        let releases = parse_release_list(Provider::GitLab, &body);
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].tag, "v2.0.0");
        assert_eq!(releases[0].version.as_deref(), Some("2.0.0"));
        assert_eq!(releases[0].notes, "Desc");
        assert_eq!(releases[0].assets.len(), 1);
        assert_eq!(releases[0].assets[0].url, "https://gitlab.com/o/r/b.tar.gz");
    }

    #[test]
    fn parse_release_list_bitbucket_reads_values_and_tag_names() {
        let body = json!({"values": [
            {"name": "v3.0.0", "target": {"date": "2026-03-01T00:00:00Z"}}
        ]});
        let releases = parse_release_list(Provider::Bitbucket, &body);
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].tag, "v3.0.0");
        assert_eq!(releases[0].version.as_deref(), Some("3.0.0"));
        assert!(releases[0].published_at.is_some());
        assert!(parse_release_list(Provider::GitHub, &json!([])).is_empty());
    }

    #[test]
    fn parse_tag_list_github_uses_tag_names() {
        let body = json!([
            {"name": "v1.0.0", "commit": {"created_at": "2026-01-01T00:00:00Z"}},
            {"name": "not-a-version", "commit": {}}
        ]);
        let releases = parse_tag_list(Provider::GitHub, &body);
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].tag, "v1.0.0");
        assert_eq!(releases[0].version.as_deref(), Some("1.0.0"));
        assert!(releases[0].published_at.is_some());
        assert_eq!(releases[1].tag, "not-a-version");
        assert_eq!(releases[1].version, None);
    }
}
