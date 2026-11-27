// storage - validation.
// Endpoint and host allowlist validation for provider URLs.
//

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

// Host allowlist (URL safety).

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
pub(crate) fn host_header(url: &url::Url) -> String {
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
pub(crate) fn build_http_client() -> UnifiedResult<reqwest::Client> {
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
pub(crate) fn map_provider_status(
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
