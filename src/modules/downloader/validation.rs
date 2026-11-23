//! Input validation and URL safety checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// URL safety (SSRF guard, same rules as the webhooks module).
//
// A download URL is attacker-influenced input: without these checks
// the server could be made to fetch cloud metadata endpoints
// (169.254.169.254), loopback admin interfaces, or any host on the
// private network, and write the response to disk. Loopback targets
// are refused like any other non-public address; only an explicit
// `DOWNLOADER_ALLOW_LOOPBACK` opt-in (development only) re-allows
// them. The checks run at job creation and again on every redirect
// target (see `build_client`).
// ---------------------------------------------------------------------------

/// The IPv4 address behind an IPv4-mapped (`::ffff:a.b.c.d`) or
/// IPv4-compatible (`::a.b.c.d`) IPv6 address, if any. Such an
/// address is really an IPv4 address and must be judged by the
/// IPv4 rules, so it cannot smuggle a loopback or private target
/// past the guard in its IPv6 spelling.
fn mapped_ipv4(addr: &std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    addr.to_ipv4_mapped().or_else(|| addr.to_ipv4())
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => {
            addr.is_loopback()
                || mapped_ipv4(&addr).is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// True when the address is private, link-local, or otherwise
/// reserved: never a valid download target. Loopback is forbidden
/// too, by default — see `allow_loopback_downloads` for the
/// explicit opt-in development exception.
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || unified_core::net::ipv4_is_benchmarking(&addr)
                || unified_core::net::ipv4_is_reserved(&addr)
        }
        IpAddr::V6(addr) => {
            // Judge a mapped address by the IPv4 rules: without
            // this, ::ffff:10.0.0.1 would pass as a harmless public
            // IPv6 address while fetching a private IPv4 host.
            if let Some(v4) = mapped_ipv4(&addr) {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            addr.is_unspecified()
                || addr.is_multicast()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || addr.is_unique_local()
        }
    }
}

/// Development-only loopback exception: loopback targets are
/// forbidden unless `DOWNLOADER_ALLOW_LOOPBACK` is explicitly set
/// to `1` or `true`. The flag is off by default and must never be
/// enabled in production — the SSRF guard exists to stop the
/// server fetching loopback admin interfaces.
pub(crate) fn allow_loopback_downloads() -> bool {
    std::env::var("DOWNLOADER_ALLOW_LOOPBACK")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Resolve a hostname and require EVERY resolved address to be safe.
/// A mixed answer (one public, one private address) is refused: the
/// HTTP client picks among the resolved addresses, so any private
/// address in the answer could be the one fetched. Loopback
/// addresses are refused like any other non-public address, unless
/// the development exception above is explicitly enabled.
fn resolved_addresses_are_safe(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_loopback(ip) {
            return allow_loopback_downloads();
        }
        return !is_forbidden_ip(ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    for socket in addresses {
        any = true;
        let ip = socket.ip();
        if is_loopback(ip) {
            if !allow_loopback_downloads() {
                return false;
            }
        } else if is_forbidden_ip(ip) {
            return false;
        }
    }
    any
}

/// Validate a download URL: http(s) only, no embedded credentials,
/// no loopback or forbidden IP-literal host, and no hostname that
/// resolves to a forbidden address. Hostnames are resolved again by the redirect
/// policy and must be re-checked at fetch time by the caller; a
/// DNS change between creation and fetch (TOCTOU rebinding) is still
/// possible and is recorded as an open limitation, not a closed gap.
pub(crate) fn validate_download_url(raw: &str) -> UnifiedResult<()> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid download URL \"{raw}\"")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Download URL must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Download URL must not contain embedded credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request(
            "Download URL must have a host",
        ));
    };
    if host.eq_ignore_ascii_case("localhost") {
        return Err(UnifiedError::bad_request(
            "Download URL must not target localhost or a loopback address",
        ));
    }
    if !resolved_addresses_are_safe(&url) {
        return Err(UnifiedError::bad_request(
            "Download URL host resolves to a private, loopback, link-local, or reserved address; download refused",
        ));
    }
    Ok(())
}

pub(crate) fn parse_checksum(input: &str) -> UnifiedResult<(String, String)> {
    let input = input.trim().to_lowercase();
    let (algorithm, hex) = match input.split_once(':') {
        Some((algorithm, hex)) => (algorithm.to_string(), hex.to_string()),
        None => ("sha256".to_string(), input),
    };
    if !matches!(algorithm.as_str(), "sha256" | "sha512" | "sha1") {
        return Err(UnifiedError::bad_request(
            "Unsupported checksum algorithm (expected sha256, sha512 or sha1)",
        ));
    }
    let expected_len = match algorithm.as_str() {
        "sha256" => 64,
        "sha512" => 128,
        _ => 40,
    };
    if hex.len() != expected_len || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid {algorithm} checksum: expected {expected_len} hex characters"
        )));
    }
    Ok((algorithm, hex))
}

pub(crate) fn build_client(request: &CreateJobRequest) -> UnifiedResult<reqwest::Client> {
    let max_redirects = request.max_redirects.min(MAX_MAX_REDIRECTS);
    let redirect = if request.follow_redirect && max_redirects > 0 {
        let max = max_redirects;
        // Every redirect target is re-validated with the same SSRF
        // rules as the original URL: a redirect to a private,
        // loopback, or reserved address (or to a non-http(s) scheme,
        // or with embedded credentials) stops the chain instead of
        // following it into the internal network.
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() as u32 >= max {
                return attempt.stop();
            }
            match validate_download_url(attempt.url().as_str()) {
                Ok(()) => attempt.follow(),
                Err(_) => attempt.stop(),
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(
            request.timeout_secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS),
        ))
        .connect_timeout(Duration::from_secs(
            request
                .connect_timeout_secs
                .clamp(MIN_CONNECT_TIMEOUT_SECS, MAX_CONNECT_TIMEOUT_SECS),
        ))
        .redirect(redirect)
        .tcp_nodelay(true)
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_checksum_bare_sha256() {
        let hex = "a".repeat(64);
        assert_eq!(parse_checksum(&hex).unwrap(), ("sha256".to_string(), hex));
    }

    #[test]
    fn parse_checksum_prefixed_algorithms() {
        assert_eq!(
            parse_checksum(&format!("sha512:{}", "b".repeat(128))).unwrap().0,
            "sha512"
        );
        assert_eq!(
            parse_checksum(&format!("sha1:{}", "c".repeat(40))).unwrap().0,
            "sha1"
        );
    }

    #[test]
    fn parse_checksum_rejects_unknown_algorithm_and_bad_length() {
        assert!(parse_checksum(&format!("md5:{}", "a".repeat(32))).is_err());
        assert!(parse_checksum("abc").is_err());
        assert!(parse_checksum(&"a".repeat(63)).is_err());
        assert!(parse_checksum(&"z".repeat(64)).is_err());
    }

    #[test]
    fn parse_checksum_normalizes_case() {
        let (algorithm, hex) = parse_checksum(&format!("SHA256:{}", "A".repeat(64))).unwrap();
        assert_eq!(algorithm, "sha256");
        assert_eq!(hex, "a".repeat(64));
    }

    #[test]
    fn validate_download_url_rejects_non_http() {
        assert!(validate_download_url("ftp://example.com/file.zip").is_err());
    }

    #[test]
    fn validate_download_url_rejects_credentials() {
        assert!(validate_download_url("https://user:pass@example.com/file.zip").is_err());
    }

    #[test]
    fn validate_download_url_rejects_localhost_and_garbage() {
        assert!(validate_download_url("https://localhost/file.zip").is_err());
        assert!(validate_download_url("not a url").is_err());
        assert!(validate_download_url("https:///file.zip").is_err());
    }

    #[test]
    fn validate_download_url_rejects_loopback_addresses() {
        // Loopback is forbidden like any other non-public address,
        // by IP literal (v4 and v6) as well as by name.
        assert!(validate_download_url("http://127.0.0.1/file.zip").is_err());
        assert!(validate_download_url("http://127.0.0.2:8080/file.zip").is_err());
        assert!(validate_download_url("http://[::1]/file.zip").is_err());
        assert!(validate_download_url("https://localhost:8443/file.zip").is_err());
    }

    #[test]
    fn validate_download_url_rejects_ipv4_mapped_addresses() {
        // ::ffff:a.b.c.d is really an IPv4 address: a mapped
        // loopback or private address must not slip past the guard
        // in its IPv6 spelling.
        assert!(validate_download_url("http://[::ffff:127.0.0.1]/file.zip").is_err());
        assert!(validate_download_url("http://[::ffff:10.0.0.1]/file.zip").is_err());
        assert!(validate_download_url("http://[::ffff:192.168.1.1]/file.zip").is_err());
        assert!(validate_download_url("http://[::ffff:169.254.169.254]/latest/meta-data/").is_err());
    }

    #[test]
    fn mapped_ipv4_addresses_use_ipv4_rules() {
        // Helper-level coverage: mapped loopback counts as loopback,
        // mapped private counts as forbidden, mapped public is fine.
        let mapped_loopback: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let mapped_private: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        let mapped_public: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(is_loopback(mapped_loopback));
        assert!(!is_loopback(mapped_private));
        assert!(!is_loopback(mapped_public));
        assert!(is_forbidden_ip(mapped_private));
        assert!(!is_forbidden_ip(mapped_public));
        assert!(!is_forbidden_ip(mapped_loopback));
    }

    #[test]
    fn validate_download_url_rejects_private_and_metadata_addresses() {
        assert!(validate_download_url("http://10.0.0.1/file.zip").is_err());
        assert!(validate_download_url("http://192.168.1.1/file.zip").is_err());
        assert!(validate_download_url("http://172.16.0.1/file.zip").is_err());
        assert!(validate_download_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_download_url("http://[fc00::1]/file.zip").is_err());
    }

    #[test]
    fn loopback_exception_is_off_by_default() {
        // The development exception is an explicit opt-in; with the
        // flag unset, loopback stays forbidden.
        assert!(!allow_loopback_downloads());
    }
}
