// Webhooks — URL safety (SSRF guard).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// URL safety (SSRF guard).
// ---------------------------------------------------------------------------

/// Normalize an address before classification: an IPv4-mapped IPv6
/// address (`::ffff:127.0.0.1`) is the IPv4 address it wraps, so the
/// wrapped address is what must be classified — otherwise the mapped
/// loopback (or mapped private address) would pass the guard while
/// the socket connects to 127.0.0.1 (or 10.0.0.1).
///
/// Only the mapped form (`::ffff:0:0/96`) is unwrapped. The
/// IPv4-compatible form (`::/96`, via `to_ipv4()`) is deliberately
/// NOT unwrapped: it would map `::1` to `0.0.0.1` and break loopback
/// detection for the IPv6 loopback address.
///
/// The mapped check is done manually from `segments()` instead of
/// the standard-library mapped-address accessor (which needs
/// Rust 1.87) so this stays compatible with the declared MSRV
/// (Rust 1.85): an address is IPv4-mapped iff its first 80 bits
/// are zero and bits 80..96 are `0xffff`.
pub(crate) fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(addr) => {
            let segments = addr.segments();
            if segments[0] == 0
                && segments[1] == 0
                && segments[2] == 0
                && segments[3] == 0
                && segments[4] == 0
                && segments[5] == 0xffff
            {
                IpAddr::V4(Ipv4Addr::new(
                    (segments[6] >> 8) as u8,
                    (segments[6] & 0xff) as u8,
                    (segments[7] >> 8) as u8,
                    (segments[7] & 0xff) as u8,
                ))
            } else {
                IpAddr::V6(addr)
            }
        }
        IpAddr::V4(_) => ip,
    }
}

pub(crate) fn is_loopback(ip: IpAddr) -> bool {
    match normalize_ip(ip) {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback(),
    }
}

/// True when the address is private, link-local, or unspecified: never
/// a valid webhook target. Loopback is handled separately (allowed
/// only over http, for local development).
pub(crate) fn is_forbidden_ip(ip: IpAddr) -> bool {
    match normalize_ip(ip) {
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
            addr.is_unspecified()
                || addr.is_multicast()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (addr.segments()[0] == 0x2001 && addr.segments()[1] == 0x0db8) // documentation 2001:db8::/32
                || addr.is_unique_local()
        }
    }
}

/// Validate a webhook target URL at creation/update time: https only
/// (http only to a loopback host), no embedded credentials, no
/// forbidden IP-literal host.
pub fn validate_webhook_url(raw: &str) -> UnifiedResult<Url> {
    let url = Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid webhook URL \"{raw}\"")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Webhook URL must not contain embedded credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request("Webhook URL must have a host"));
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_loopback(ip) {
            if url.scheme() != "http" && url.scheme() != "https" {
                return Err(UnifiedError::bad_request(
                    "Webhook URL must use http or https",
                ));
            }
        } else {
            if url.scheme() != "https" {
                return Err(UnifiedError::bad_request(
                    "Webhook URL must use https (http is only allowed for loopback hosts)",
                ));
            }
            if is_forbidden_ip(ip) {
                return Err(UnifiedError::bad_request(
                    "Webhook URL host is a private, link-local, or reserved IP address",
                ));
            }
        }
    } else {
        if url.scheme() != "https" {
            return Err(UnifiedError::bad_request(
                "Webhook URL must use https (http is only allowed for loopback hosts)",
            ));
        }
        if host.eq_ignore_ascii_case("localhost") {
            return Err(UnifiedError::bad_request(
                "Use an explicit loopback IP (127.0.0.1 or [::1]) instead of \"localhost\"",
            ));
        }
    }
    Ok(url)
}

/// Resolve a webhook target URL once and validate every address it
/// resolves to. Returns the validated socket addresses for the
/// delivery to connect to. Callers must connect only to these
/// addresses (for example through a reqwest DNS override), keeping
/// the original host in the URL for the Host header and TLS SNI —
/// the request must never re-resolve the host, because a second
/// lookup could return a different, private address after this
/// check passed (DNS-rebinding TOCTOU). An IP-literal host resolves
/// to itself. Every address must be safe: requiring only one safe
/// address would let a resolver that returns a mix of public and
/// private addresses steer the request at a private one (the HTTP
/// client picks among the resolved addresses), so a mixed answer is
/// refused outright.
pub(crate) fn resolve_validated_addresses(url: &Url) -> UnifiedResult<Vec<SocketAddr>> {
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request("Webhook URL must have a host"));
    };
    let port = url.port_or_known_default().unwrap_or(443);
    if let Ok(ip) = host.parse::<IpAddr>() {
        let ip = normalize_ip(ip);
        if !is_loopback(ip) && is_forbidden_ip(ip) {
            return Err(UnifiedError::bad_request(
                "Webhook URL host is a private, link-local, or reserved IP address",
            ));
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return Err(UnifiedError::bad_request(format!(
            "Webhook URL host \"{host}\" does not resolve"
        )));
    };
    let mut validated = Vec::new();
    for socket in addresses {
        let ip = normalize_ip(socket.ip());
        if !is_loopback(ip) && is_forbidden_ip(ip) {
            return Err(UnifiedError::bad_request(
                "Webhook URL host resolves to a private, link-local, or reserved IP address",
            ));
        }
        validated.push(SocketAddr::new(ip, socket.port()));
    }
    if validated.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "Webhook URL host \"{host}\" does not resolve"
        )));
    }
    Ok(validated)
}

pub(crate) fn validate_event_names(events: &[String]) -> UnifiedResult<()> {
    if events.is_empty() {
        return Err(UnifiedError::bad_request(
            "events must contain at least one event name",
        ));
    }
    for event in events {
        if !KNOWN_EVENTS.contains(&event.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown event \"{event}\"; see GET /api/v1/webhooks/events for the subscribable events"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_webhook_url_accepts_https_host() {
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
    }

    #[test]
    fn validate_webhook_url_accepts_http_loopback() {
        assert!(validate_webhook_url("http://127.0.0.1:8080/hook").is_ok());
    }

    #[test]
    fn validate_webhook_url_accepts_https_loopback() {
        assert!(validate_webhook_url("https://127.0.0.1/hook").is_ok());
    }

    #[test]
    fn validate_webhook_url_rejects_http_to_public_host() {
        assert!(validate_webhook_url("http://example.com/hook").is_err());
    }

    #[test]
    fn validate_webhook_url_rejects_embedded_credentials() {
        assert!(validate_webhook_url("https://user:pass@example.com/hook").is_err());
    }

    #[test]
    fn validate_webhook_url_rejects_private_ip_literal() {
        assert!(validate_webhook_url("https://10.0.0.1/hook").is_err());
        assert!(validate_webhook_url("https://192.168.1.10/hook").is_err());
    }

    #[test]
    fn validate_webhook_url_rejects_localhost_name() {
        assert!(validate_webhook_url("https://localhost/hook").is_err());
    }

    #[test]
    fn validate_webhook_url_rejects_garbage() {
        assert!(validate_webhook_url("not a url").is_err());
        assert!(validate_webhook_url("ftp://example.com/hook").is_err());
    }

    #[test]
    fn is_forbidden_ip_covers_private_and_public() {
        let private: std::net::IpAddr = "10.1.2.3".parse().unwrap();
        let public: std::net::IpAddr = "8.8.8.8".parse().unwrap();
        let loopback: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        assert!(is_forbidden_ip(private));
        assert!(!is_forbidden_ip(public));
        assert!(is_loopback(loopback));
        assert!(!is_loopback(public));
    }

    #[test]
    fn normalize_ip_unwraps_ipv4_mapped_addresses_only() {
        // ::ffff:127.0.0.1 is the IPv4 loopback in mapped form.
        let mapped_loopback: std::net::IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(is_loopback(mapped_loopback));
        // The IPv6 loopback must stay the IPv6 loopback: unwrapping
        // the IPv4-compatible form (::/96) would map ::1 to 0.0.0.1
        // and break loopback detection.
        let v6_loopback: std::net::IpAddr = "::1".parse().unwrap();
        assert!(is_loopback(v6_loopback));
        assert_eq!(normalize_ip(v6_loopback), v6_loopback);
    }

    #[test]
    fn normalize_ip_leaves_non_mapped_ipv6_unchanged() {
        // A public IPv6 address is not IPv4-mapped and must pass
        // through unchanged.
        let public_v6: std::net::IpAddr = "2606:4700:4700::1111".parse().unwrap();
        assert_eq!(normalize_ip(public_v6), public_v6);
        // Same for the documentation prefix and the unspecified
        // address: neither has the ::ffff:0:0/96 mapped prefix.
        let doc_v6: std::net::IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(normalize_ip(doc_v6), doc_v6);
        let unspecified_v6: std::net::IpAddr = "::".parse().unwrap();
        assert_eq!(normalize_ip(unspecified_v6), unspecified_v6);
        // And a plain IPv4 address is returned as-is.
        let v4: std::net::IpAddr = "8.8.8.8".parse().unwrap();
        assert_eq!(normalize_ip(v4), v4);
    }

    #[test]
    fn is_forbidden_ip_rejects_ipv4_mapped_private_addresses() {
        // A mapped private address must not bypass the SSRF guard.
        let mapped_private: std::net::IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert!(is_forbidden_ip(mapped_private));
        let mapped_link_local: std::net::IpAddr = "::ffff:169.254.169.254".parse().unwrap();
        assert!(is_forbidden_ip(mapped_link_local));
        let mapped_public: std::net::IpAddr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(!is_forbidden_ip(mapped_public));
    }

    #[test]
    fn is_forbidden_ip_rejects_the_v6_documentation_prefix() {
        let doc: std::net::IpAddr = "2001:db8::1".parse().unwrap();
        assert!(is_forbidden_ip(doc));
        let doc_full: std::net::IpAddr = "2001:db8:85a3::8a2e:370:7334".parse().unwrap();
        assert!(is_forbidden_ip(doc_full));
        // Just outside the /32 is not documentation space.
        let outside: std::net::IpAddr = "2001:db9::1".parse().unwrap();
        assert!(!is_forbidden_ip(outside));
    }

    #[test]
    fn validate_webhook_url_rejects_mapped_private_ip_literal() {
        assert!(validate_webhook_url("https://[::ffff:10.0.0.1]/hook").is_err());
        assert!(validate_webhook_url("https://[::ffff:192.168.1.10]/hook").is_err());
        // A mapped loopback is the loopback host, so it follows the
        // same policy as 127.0.0.1 (allowed over http or https).
        assert!(validate_webhook_url("https://[::ffff:127.0.0.1]/hook").is_ok());
    }

    #[test]
    fn resolve_validated_addresses_returns_the_literal_for_ip_hosts() {
        let url = Url::parse("https://127.0.0.1:8443/hook").unwrap();
        let addresses = resolve_validated_addresses(&url).unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].ip().to_string(), "127.0.0.1");
        assert_eq!(addresses[0].port(), 8443);
    }

    #[test]
    fn resolve_validated_addresses_rejects_forbidden_ip_literals() {
        let url = Url::parse("https://10.0.0.1/hook").unwrap();
        assert!(resolve_validated_addresses(&url).is_err());
        let mapped = Url::parse("https://[::ffff:10.0.0.1]/hook").unwrap();
        assert!(resolve_validated_addresses(&mapped).is_err());
    }

    #[test]
    fn resolve_validated_addresses_rejects_unresolvable_hosts() {
        let url = Url::parse("https://nonexistent.invalid/hook").unwrap();
        assert!(resolve_validated_addresses(&url).is_err());
    }

    #[test]
    fn validate_event_names_accepts_known_and_rejects_unknown() {
        assert!(validate_event_names(&["webhook.ping".to_string()]).is_ok());
        assert!(validate_event_names(&["no.such.event".to_string()]).is_err());
        assert!(validate_event_names(&[]).is_err());
    }
}
