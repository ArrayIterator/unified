// Tax module — connector endpoint, signing, and mapping helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Connector endpoint validation (SSRF protection) and signing
// ---------------------------------------------------------------------------

/// True when the address must never be a connector target: private,
/// loopback, link-local, multicast, unspecified, broadcast,
/// benchmarking, documentation, or reserved ranges.
///
/// An IPv4-mapped IPv6 address (`::ffff:10.0.0.1`) is classified by
/// the IPv4 address it wraps — otherwise the mapped private address
/// would pass the guard while the socket connects to 10.0.0.1. Only
/// the mapped form is unwrapped: unwrapping the IPv4-compatible
/// form (`::/96`) would map `::1` to `0.0.0.1` and break loopback
/// detection for the IPv6 loopback address.
pub(crate) fn address_is_forbidden(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || unified_core::net::ipv4_is_benchmarking(&ip)
                || ip.is_documentation()
                || unified_core::net::ipv4_is_reserved(&ip)
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return address_is_forbidden(IpAddr::V4(mapped));
            }
            // Segment masks (same shape as the webhooks module):
            // link-local fe80::/10, unique-local fc00::/7,
            // documentation 2001:db8::/32, benchmarking 2001:2::/48.
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || (segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0)
        }
    }
}

/// Validate a connector endpoint URL: HTTPS only, no userinfo, host
/// present and listed in `allowed_hosts` (exact match, case
/// insensitive), and every address the host resolves to must be a
/// public address. Hostnames `localhost` and `*.local` are rejected
/// outright, and so is any literal IP that is not public.
pub(crate) fn validate_endpoint_url(
    endpoint: &str,
    allowed_hosts: &[String],
) -> UnifiedResult<Url> {
    let url = Url::parse(endpoint.trim())
        .map_err(|_| UnifiedError::bad_request("endpoint_url must be a valid URL"))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request("endpoint_url must use https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "endpoint_url must not contain userinfo",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("endpoint_url must have a host"))?;
    let host_lower = host.to_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".local") {
        return Err(UnifiedError::bad_request(
            "endpoint_url host must not be localhost or a .local name",
        ));
    }
    if !allowed_hosts
        .iter()
        .any(|allowed| allowed.trim().eq_ignore_ascii_case(&host_lower))
    {
        return Err(UnifiedError::bad_request(
            "endpoint_url host is not in the connector allowed_hosts list",
        ));
    }
    if let Ok(addr) = host.parse::<IpAddr>() {
        if address_is_forbidden(addr) {
            return Err(UnifiedError::bad_request(
                "endpoint_url host must resolve to a public address",
            ));
        }
        return Ok(url);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let resolved = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|_| UnifiedError::bad_request("endpoint_url host could not be resolved"))?;
    let mut any = false;
    for socket in resolved {
        any = true;
        if address_is_forbidden(socket.ip()) {
            return Err(UnifiedError::bad_request(
                "endpoint_url host resolves to a non-public address",
            ));
        }
    }
    if !any {
        return Err(UnifiedError::bad_request(
            "endpoint_url host could not be resolved",
        ));
    }
    Ok(url)
}

/// Validate a connector endpoint like `validate_endpoint_url`, and
/// additionally resolve the host once and return the validated
/// address to connect to.
///
/// DNS-rebinding defence: the address is validated and pinned in
/// the same resolution, and the caller must hand the returned
/// address to the HTTP client as a DNS override
/// (`ClientBuilder::resolve`), so the client connects to the
/// validated address instead of re-resolving the hostname (where
/// a rebinding answer could point at an internal address).
/// Returns `None` for the address when the host is already a
/// literal IP: there is nothing to re-resolve, so no override is
/// needed.
pub(crate) fn validate_endpoint_url_pinned(
    endpoint: &str,
    allowed_hosts: &[String],
) -> UnifiedResult<(Url, Option<SocketAddr>)> {
    let url = validate_endpoint_url(endpoint, allowed_hosts)?;
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("endpoint_url must have a host"))?;
    if host.parse::<IpAddr>().is_ok() {
        return Ok((url, None));
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let resolved: Vec<SocketAddr> = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|_| UnifiedError::bad_request("endpoint_url host could not be resolved"))?
        .collect();
    let Some(pinned) = resolved.first().copied() else {
        return Err(UnifiedError::bad_request(
            "endpoint_url host could not be resolved",
        ));
    };
    for socket in &resolved {
        if address_is_forbidden(socket.ip()) {
            return Err(UnifiedError::bad_request(
                "endpoint_url host resolves to a non-public address",
            ));
        }
    }
    Ok((url, Some(pinned)))
}

/// Sign a request body for the `hmac` scheme: HMAC-SHA256 over
/// `timestamp + "." + body`, hex-encoded. The timestamp is Unix
/// seconds, so a replayed request expires with the provider's own
/// window.
pub(crate) fn sign_body(secret: &str, timestamp: i64, body: &str) -> String {
    let message = format!("{timestamp}.{body}");
    let tag = hmac_sha256(secret.as_bytes(), message.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// Read a dotted path (`a.b.c`) out of a JSON value.
pub(crate) fn read_dotted_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Apply a response mapping (`{"output_field": "source.path"}`) to a
/// provider response, returning the renamed fields that resolved.
pub(crate) fn apply_response_mapping(mapping: &Value, response: &Value) -> Value {
    let Some(map) = mapping.as_object() else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (output_field, source) in map {
        if let Some(path) = source.as_str() {
            if let Some(value) = read_dotted_path(response, path) {
                out.insert(output_field.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

/// Apply a request mapping (`{"provider_field": "local_field"}`) to a
/// local payload, renaming the fields that are present.
pub(crate) fn apply_request_mapping(mapping: &Value, payload: &Value) -> Value {
    let Some(map) = mapping.as_object() else {
        return payload.clone();
    };
    let mut out = match payload.as_object() {
        Some(object) => object.clone(),
        None => serde_json::Map::new(),
    };
    for (provider_field, local_field) in map {
        if let Some(local) = local_field.as_str() {
            if let Some(value) = payload.get(local) {
                out.insert(provider_field.clone(), value.clone());
                out.remove(local);
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forbidden(addr: &str) -> bool {
        address_is_forbidden(addr.parse::<IpAddr>().unwrap())
    }

    #[test]
    fn address_is_forbidden_blocks_ipv6_link_local_documentation_and_mapped() {
        // Regression (T4): the IPv6 check only covered loopback,
        // multicast, unspecified, and unique-local.
        assert!(forbidden("fe80::1"));
        assert!(forbidden("fe80::ffff"));
        assert!(forbidden("2001:db8::1"));
        assert!(forbidden("2001:db8:85a3::8a2e:370:7334"));
        assert!(forbidden("2001:2::1"));
        // IPv4-mapped addresses are classified by the IPv4
        // address they wrap.
        assert!(forbidden("::ffff:10.0.0.1"));
        assert!(forbidden("::ffff:127.0.0.1"));
        assert!(forbidden("::ffff:192.168.1.1"));
        assert!(forbidden("::ffff:169.254.1.1"));
        // Previously covered ranges still blocked.
        assert!(forbidden("::1"));
        assert!(forbidden("fc00::1"));
        assert!(forbidden("fd00::1"));
        assert!(forbidden("ff02::1"));
        assert!(forbidden("::"));
        // Public addresses still pass, including mapped public ones.
        assert!(!forbidden("2001:4860:4860::8888"));
        assert!(!forbidden("::ffff:8.8.8.8"));
        assert!(!forbidden("8.8.8.8"));
        assert!(!forbidden("1.1.1.1"));
    }

    #[test]
    fn validate_endpoint_url_rejects_forbidden_literal_ips() {
        assert!(validate_endpoint_url("https://10.0.0.1/hook", &["10.0.0.1".to_string()]).is_err());
        assert!(validate_endpoint_url("https://[fe80::1]/hook", &["fe80::1".to_string()]).is_err());
        assert!(validate_endpoint_url(
            "https://[::ffff:10.0.0.1]/hook",
            &["::ffff:10.0.0.1".to_string()]
        )
        .is_err());
        assert!(
            validate_endpoint_url("https://[2001:db8::1]/hook", &["2001:db8::1".to_string()])
                .is_err()
        );
        assert!(validate_endpoint_url("https://8.8.8.8/hook", &["8.8.8.8".to_string()]).is_ok());
        assert!(validate_endpoint_url(
            "https://[2001:4860:4860::8888]/hook",
            &["2001:4860:4860::8888".to_string()]
        )
        .is_ok());
    }

    #[test]
    fn validate_endpoint_url_pinned_needs_no_override_for_literal_ip() {
        // Regression (T4): the pinned variant must return the
        // validated address for hostnames and None for literals.
        let (url, pinned) =
            validate_endpoint_url_pinned("https://8.8.8.8/hook", &["8.8.8.8".to_string()]).unwrap();
        assert_eq!(url.as_str(), "https://8.8.8.8/hook");
        assert!(pinned.is_none());
    }
    #[test]
    fn sign_body_is_deterministic_64_hex_and_input_sensitive() {
        let first = sign_body("secret", 1_700_000_000, "body");
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(sign_body("secret", 1_700_000_000, "body"), first);
        assert_ne!(sign_body("other", 1_700_000_000, "body"), first);
        assert_ne!(sign_body("secret", 1_700_000_001, "body"), first);
        assert_ne!(sign_body("secret", 1_700_000_000, "other"), first);
    }

}
