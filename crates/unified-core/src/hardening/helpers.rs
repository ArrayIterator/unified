// hardening — helpers.
// Request helpers: client IP, paths, responses, headers.
//
// Split from `crates/unified-core/src/hardening.rs`; moved verbatim, no behaviour changes.

use super::*;
use crate::error::UnifiedError;
use crate::hooks::HookBus;
use crate::json;
use crate::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::HttpResponse;
use actix_web::dev::ServiceRequest;
use actix_web::http::header::{self, HeaderValue};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

impl HardeningState {
    // -- Helpers for the middleware --------------------------------------

    /// True when the path is exempt from rate limiting and the
    /// blocklist (health probe and ACME challenge by default).
    pub fn is_exempt_path(&self, path: &str, settings: &SecuritySettings) -> bool {
        settings
            .rate_limit_exempt_paths
            .iter()
            .any(|prefix| path == prefix || path.starts_with(prefix.as_str()))
    }

    /// Body-size limit in bytes for a path: the upload limit when the
    /// path matches an upload prefix, otherwise the default limit.
    pub fn body_limit_for_path(&self, path: &str, settings: &SecuritySettings) -> (u64, &'static str) {
        if settings
            .upload_path_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
        {
            (settings.max_body_bytes_upload, "upload")
        } else {
            (settings.max_body_bytes_default, "default")
        }
    }
}

// ---------------------------------------------------------------------------
// Request helpers shared by the middleware
// ---------------------------------------------------------------------------

/// Client IP for a request: the first forwarded address reported by
/// Actix `ConnectionInfo` when present, otherwise the peer address.
///
/// WARNING: Actix trusts `X-Forwarded-For` from any client, so this
/// helper is spoofable by a direct client and must NOT be used for
/// enforcement (blocklist, rate limiting, brute-force keys). It is
/// kept for display / logging callers that already use it.
/// Enforcement paths must use `trusted_client_ip` instead.
pub fn client_ip(req: &ServiceRequest) -> Option<IpAddr> {
    if let Some(real_ip) = req.connection_info().realip_remote_addr() {
        if let Ok(ip) = real_ip.parse::<IpAddr>() {
            return Some(ip);
        }
    }
    req.peer_addr().map(|address| address.ip())
}

/// Client IP for enforcement, honouring `X-Forwarded-For` only when
/// the TCP peer is a configured trusted proxy
/// (`security.trusted_proxies`). With no trusted proxies configured
/// (the default) the peer address is always the client IP, so a
/// direct client can never spoof the IP the blocklist, rate limiter,
/// and brute-force engine enforce on. When the peer is trusted, the
/// first `X-Forwarded-For` entry is the client IP; an unparseable or
/// absent entry falls back to the peer address (fail closed: never
/// invent an address).
pub fn trusted_client_ip(
    peer: Option<std::net::SocketAddr>,
    headers: &actix_web::http::header::HeaderMap,
    settings: &SecuritySettings,
) -> Option<IpAddr> {
    let peer_ip = peer.map(|address| address.ip())?;
    if settings.trusted_proxies.is_empty() {
        return Some(peer_ip);
    }
    let trusted = settings.trusted_proxies.iter().any(|entry| {
        parse_cidr(entry).is_ok_and(|range| range.contains(peer_ip))
    });
    if !trusted {
        return Some(peer_ip);
    }
    let forwarded = headers
        .get(header::X_FORWARDED_FOR)
        .and_then(|value| value.to_str().ok())?;
    let first = forwarded.split(',').next().map(str::trim).unwrap_or("");
    match first.parse::<IpAddr>() {
        Ok(ip) => Some(ip),
        Err(_) => Some(peer_ip),
    }
}

/// `trusted_client_ip` for a middleware `ServiceRequest`.
pub fn trusted_client_ip_for_request(
    req: &ServiceRequest,
    settings: &SecuritySettings,
) -> Option<IpAddr> {
    trusted_client_ip(req.peer_addr(), req.headers(), settings)
}

/// `trusted_client_ip` for a handler `HttpRequest`.
pub fn trusted_client_ip_for_http_request(
    req: &actix_web::HttpRequest,
    settings: &SecuritySettings,
) -> Option<IpAddr> {
    trusted_client_ip(req.peer_addr(), req.headers(), settings)
}

/// True when the path is the ACME HTTP-01 challenge route.
pub fn is_acme_challenge_path(path: &str) -> bool {
    path.starts_with("/.well-known/acme-challenge/")
}

/// Normalize a request path for the per-endpoint rate-limit key:
/// numeric segments and UUID segments become `{id}` so one resource
/// with many ids does not create one window per id.
pub fn normalize_endpoint_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.is_empty() {
                segment.to_string()
            } else if segment.chars().all(|c| c.is_ascii_digit())
                || segment.parse::<uuid::Uuid>().is_ok()
            {
                "{id}".to_string()
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("/")
}

/// Per-user rate-limit key from the Authorization header. The raw
/// token is never stored: the key is a truncated base64url SHA-256 of
/// the header value, so one bearer token gets one window without the
/// token itself ever entering the rate-limit store or any log.
pub fn user_key(req: &ServiceRequest) -> Option<String> {
    let header = req.headers().get(header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    if value.trim().is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..8]);
    Some(format!("user:{encoded}"))
}

/// Build a standard-envelope 429 response with `Retry-After` and the
/// `X-RateLimit-*` headers.
pub(crate) fn rate_limited_response(
    limit: u32,
    remaining: u32,
    reset_in_secs: u64,
    retry_after_secs: u64,
    challenge_required: bool,
) -> HttpResponse {
    let mut error = UnifiedError::new("Rate limit exceeded", 429)
        .with_metadata("retry_after_secs", serde_json::json!(retry_after_secs))
        .with_metadata("limit", serde_json::json!(limit))
        .with_metadata("remaining", serde_json::json!(remaining))
        .with_metadata("reset_in_secs", serde_json::json!(reset_in_secs));
    if challenge_required {
        error = error.with_metadata("challenge_required", serde_json::json!(true));
    }
    HttpResponse::TooManyRequests()
        .insert_header(("Retry-After", retry_after_secs.to_string()))
        .insert_header(("X-RateLimit-Limit", limit.to_string()))
        .insert_header(("X-RateLimit-Remaining", remaining.to_string()))
        .insert_header((
            "X-RateLimit-Reset",
            (Utc::now().timestamp() + reset_in_secs as i64).to_string(),
        ))
        .json(json::error_json(&error, false))
}

pub(crate) fn set_header(
    headers: &mut actix_web::http::header::HeaderMap,
    name: header::HeaderName,
    value: &str,
) {
    if value.is_empty() {
        return;
    }
    if let Ok(header_value) = HeaderValue::from_str(value) {
        headers.insert(name, header_value);
    }
}
