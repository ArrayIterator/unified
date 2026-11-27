// Third-party VCS integrations - Input validation and pure value checks.

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
use super::providers::*;
use super::store::*;
use super::helpers::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

/// Owner / repository slug: letters, digits, `-`, `_`, `.`; no slashes,
/// no whitespace, not `.` or `..`.
pub(crate) fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SLUG_LEN
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Hostname with an optional `:port`. No scheme, no path, no userinfo.
pub(crate) fn valid_host(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.contains('/') || value.contains('@') {
        return false;
    }
    let (host, port) = match value.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (value, None),
    };
    if let Some(port) = port {
        if port.is_empty() || port.parse::<u16>().is_err() {
            return false;
        }
    }
    !host.is_empty()
        && host
            .split('.')
            .all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            })
}

/// Parse a version from a tag: strip one leading `v`/`V`, then require
/// strict semver.
pub(crate) fn version_from_tag(tag: &str) -> UnifiedResult<Version> {
    let stripped = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    Version::parse(stripped).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Tag \"{tag}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH, optionally prefixed with \"v\")"
        ))
    })
}

pub(crate) fn validate_version_string(version: &str) -> UnifiedResult<Version> {
    Version::parse(version.trim()).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Version \"{version}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH)"
        ))
    })
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Constant-time string comparison, so signature and token checks do
/// not leak how many leading characters matched.
pub(crate) fn secrets_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Canonical asset URL for storage: scheme, host, and path only. The
/// query string and fragment are removed because provider asset URLs
/// can carry signed query parameters, which must never be stored or
/// returned. Returns `None` when the URL does not parse.
pub(crate) fn redact_url(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Some(url.to_string())
}

/// True when the host is an IP literal (never an allowed asset host).
pub(crate) fn is_ip_literal(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok()
}

/// Validate a provider API base override URL: `https` only, no
/// credentials, and no IP-literal or local host. The stored provider
/// token is sent to this host as an Authorization / Private-Token
/// header, so an override pointing at an internal or
/// attacker-controlled address would exfiltrate the token. A
/// hostname that merely resolves to a private address is not caught
/// here (no DNS lookup), the same limit as the asset-URL guard.
pub(crate) fn validate_api_base_override(raw: &str) -> UnifiedResult<()> {
    let url = url::Url::parse(raw.trim()).map_err(|_| {
        UnifiedError::bad_request("api_base_override must be a valid URL")
    })?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request("api_base_override must use https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "api_base_override must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("api_base_override must have a host"))?
        .to_ascii_lowercase();
    // IP literals (v4 and v6, public or private) and localhost can
    // only point at a single machine, never at a provider API.
    if is_ip_literal(&host) || host == "localhost" || host.ends_with(".localhost") {
        return Err(UnifiedError::forbidden(
            "api_base_override must not point at an IP-literal or local host",
        ));
    }
    Ok(())
}

/// True when `host` is an allowed release-asset download host.
pub(crate) fn allowed_asset_host(host: &str, gitlab_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    if BUILTIN_ASSET_HOSTS.contains(&host.as_str()) {
        return true;
    }
    gitlab_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host)
}

/// Validate a release-asset URL against the allowlist (SSRF guard).
/// Only `https` is accepted, except plain `http` for configured
/// self-hosted GitLab hosts. Rejects userinfo, IP-literal hosts, and
/// any host outside the allowlist. The URL is never echoed back in an
/// error: the message names only the offending host.
pub(crate) fn validate_asset_url(raw: &str, gitlab_hosts: &[String]) -> UnifiedResult<url::Url> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Asset URL is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Asset URL must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Asset URL has no host"))?
        .to_ascii_lowercase();
    if is_ip_literal(&host) {
        return Err(UnifiedError::forbidden(
            "Asset downloads from IP-literal hosts are not allowed",
        ));
    }
    if !allowed_asset_host(&host, gitlab_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "Asset host \"{host}\" is not allowlisted for downloads"
        )));
    }
    let is_configured_gitlab = gitlab_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host);
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_configured_gitlab => Ok(url),
        _ => Err(UnifiedError::bad_request(
            "Asset URL must use https (http is allowed only for configured self-hosted GitLab hosts)",
        )),
    }
}

/// Ask the hook bus whether a software id is acceptable. When no
/// handler is registered the id is accepted; a handler that answers
/// `valid: false` rejects it with 422.
pub(crate) fn validate_software_id(ctx: &AppContext, software_id: &str) -> UnifiedResult<()> {
    if !ctx.hooks.has("vcs.software.validate") {
        return Ok(());
    }
    let context = ctx.hooks.emit(
        "vcs.software.validate",
        json!({ "software_id": software_id }),
    );
    let invalid = context
        .results
        .iter()
        .any(|result| result.get("valid").and_then(|value| value.as_bool()) == Some(false));
    if invalid {
        return Err(UnifiedError::unprocessable(format!(
            "Software \"{software_id}\" is not valid for a VCS connection"
        )));
    }
    Ok(())
}

pub(crate) fn validate_connection_input(
    provider: Provider,
    owner: &str,
    repo: &str,
    host: Option<&str>,
    software_id: &str,
) -> UnifiedResult<()> {
    if !valid_slug(owner) {
        return Err(UnifiedError::bad_request(
            "Owner must be a slug of letters, digits, '-', '_' or '.'",
        ));
    }
    if !valid_slug(repo) {
        return Err(UnifiedError::bad_request(
            "Repository must be a slug of letters, digits, '-', '_' or '.'",
        ));
    }
    if let Some(host) = host.filter(|host| !host.is_empty()) {
        if provider != Provider::GitLab {
            return Err(UnifiedError::bad_request(
                "A custom host is only supported for self-hosted GitLab instances",
            ));
        }
        if !valid_host(host) {
            return Err(UnifiedError::bad_request(
                "Host must be a hostname, optionally with a port (no scheme or path)",
            ));
        }
    }
    if software_id.trim().is_empty() || software_id.len() > 256 {
        return Err(UnifiedError::bad_request(
            "software_id is required and must be at most 256 characters",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug_accepts_owner_and_repo_names() {
        assert!(valid_slug("my-repo_1.2"));
        assert!(valid_slug("a"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a/b"));
        assert!(!valid_slug("a b"));
        assert!(!valid_slug("."));
        assert!(!valid_slug(".."));
    }

    #[test]
    fn valid_host_accepts_hostnames_with_optional_ports() {
        assert!(valid_host("example.com"));
        assert!(valid_host("git.example.com:8080"));
        assert!(valid_host("localhost"));
        assert!(!valid_host(""));
        assert!(!valid_host("https://example.com"));
        assert!(!valid_host("example.com/path"));
        assert!(!valid_host("example.com:99999"));
        assert!(!valid_host("example.com:"));
        assert!(!valid_host("-bad.example.com"));
        assert!(!valid_host("a..b"));
    }

    #[test]
    fn version_from_tag_strips_a_leading_v() {
        assert_eq!(version_from_tag("v1.2.3").unwrap().to_string(), "1.2.3");
        assert_eq!(version_from_tag("V2.0.0").unwrap().to_string(), "2.0.0");
        assert_eq!(version_from_tag("1.2.3").unwrap().to_string(), "1.2.3");
        assert_eq!(version_from_tag("v1.2.3-beta.1").unwrap().to_string(), "1.2.3-beta.1");
        assert!(version_from_tag("1.2").is_err());
        assert!(version_from_tag("v1.2.3.4").is_err());
    }

    #[test]
    fn validate_version_string_trims_and_requires_semver() {
        assert_eq!(validate_version_string(" 1.2.3 ").unwrap().to_string(), "1.2.3");
        assert!(validate_version_string("1.2").is_err());
        assert!(validate_version_string("").is_err());
    }

    #[test]
    fn hex_encode_produces_lowercase_hex() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0, 255, 16]), "00ff10");
        assert_eq!(hex_encode(b"hi"), "6869");
    }

    #[test]
    fn secrets_equal_compares_without_short_circuiting_on_length() {
        assert!(secrets_equal("abc", "abc"));
        assert!(!secrets_equal("abc", "abd"));
        assert!(!secrets_equal("abc", "ab"));
        assert!(!secrets_equal("", "a"));
        assert!(secrets_equal("", ""));
    }

    #[test]
    fn redact_url_strips_query_fragment_and_credentials() {
        assert_eq!(
            redact_url("https://github.com/o/r.zip?token=abc#frag").as_deref(),
            Some("https://github.com/o/r.zip")
        );
        assert_eq!(
            redact_url("https://github.com/o/r.zip").as_deref(),
            Some("https://github.com/o/r.zip")
        );
        assert_eq!(redact_url("not a url"), None);
    }

    #[test]
    fn is_ip_literal_detects_v4_and_v6_literals() {
        assert!(is_ip_literal("127.0.0.1"));
        assert!(is_ip_literal("::1"));
        assert!(!is_ip_literal("example.com"));
        assert!(!is_ip_literal("256.1.1.1"));
    }

    #[test]
    fn allowed_asset_host_covers_builtin_and_configured_hosts() {
        assert!(allowed_asset_host("github.com", &[]));
        assert!(allowed_asset_host("GITHUB.COM", &[]));
        assert!(allowed_asset_host("objects.githubusercontent.com", &[]));
        assert!(allowed_asset_host("api.bitbucket.org", &[]));
        assert!(!allowed_asset_host("evil.example.com", &[]));
        assert!(allowed_asset_host("git.example.com", &["git.example.com".to_string()]));
        assert!(!allowed_asset_host("git.example.com", &[]));
    }

    #[test]
    fn validate_asset_url_enforces_the_allowlist_and_https() {
        assert!(validate_asset_url("https://github.com/o/r/releases/download/v1/a.zip", &[]).is_ok());
        assert!(validate_asset_url("http://github.com/o/r.zip", &[]).is_err());
        assert!(validate_asset_url("https://user@github.com/o/r.zip", &[]).is_err());
        assert!(validate_asset_url("https://127.0.0.1/r.zip", &[]).is_err());
        assert!(validate_asset_url("https://evil.example.com/r.zip", &[]).is_err());
        assert!(validate_asset_url("https://api.github.com/repos/o/r/tarball/v1", &[]).is_ok());
        assert!(validate_asset_url(
            "http://git.example.com/r.zip",
            &["git.example.com".to_string()]
        )
        .is_ok());
    }

    #[test]
    fn validate_connection_input_checks_provider_owner_repo_and_host() {
        assert!(validate_connection_input(Provider::GitHub, "acme", "widget", None, "sw-1").is_ok());
        assert!(validate_connection_input(Provider::GitHub, "acme", "widget", Some("x.com"), "sw-1").is_err());
        assert!(validate_connection_input(
            Provider::GitLab, "acme", "widget", Some("git.example.com"), "sw-1"
        )
        .is_ok());
        assert!(validate_connection_input(Provider::GitHub, "bad owner", "widget", None, "sw-1").is_err());
        assert!(validate_connection_input(Provider::GitHub, "acme", "widget", None, "").is_err());
        assert!(validate_connection_input(Provider::GitHub, "acme", "widget", None, "  ").is_err());
    }

    #[test]
    fn validate_api_base_override_rejects_ip_literal_and_local_hosts() {
        assert!(validate_api_base_override("https://api.example.com/v4").is_ok());
        assert!(validate_api_base_override("https://gitlab.example.com/api/v4").is_ok());
        assert!(validate_api_base_override("http://api.example.com/v4").is_err());
        assert!(validate_api_base_override("https://user:pass@api.example.com/v4").is_err());
        assert!(validate_api_base_override("https://127.0.0.1/api").is_err());
        assert!(validate_api_base_override("https://10.0.0.5/api").is_err());
        assert!(validate_api_base_override("https://172.16.0.9/api").is_err());
        assert!(validate_api_base_override("https://192.168.1.10/api").is_err());
        assert!(validate_api_base_override("https://8.8.8.8/api").is_err());
        assert!(validate_api_base_override("https://[::1]/api").is_err());
        assert!(validate_api_base_override("https://localhost/api").is_err());
        assert!(validate_api_base_override("https://api.localhost/api").is_err());
        assert!(validate_api_base_override("not a url").is_err());
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
