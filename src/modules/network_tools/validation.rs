//! Target validation, SSRF guards and timeout clamps.

use unified_core::error::{UnifiedError, UnifiedResult};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration};
use super::types::{DEFAULT_CONNECT_TIMEOUT_SECS, DEFAULT_TOTAL_TIMEOUT_SECS, MAX_CONNECT_TIMEOUT_SECS, MAX_TOTAL_TIMEOUT_SECS};

// ---------------------------------------------------------------------------
// Input validation and SSRF guards.
// ---------------------------------------------------------------------------

/// A hostname is accepted when it is a dotted name of labels made of
/// letters, digits, and hyphens (no leading/trailing hyphen, no empty
/// labels, at most 253 characters), or a literal IP address.
pub(crate) fn valid_hostname_or_ip(target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() || target.len() > 253 {
        return false;
    }
    if target.parse::<IpAddr>().is_ok() {
        return true;
    }
    if target.starts_with('.') || target.ends_with('.') || target.contains("..") {
        return false;
    }
    target.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

pub(crate) fn validate_target(target: &str) -> UnifiedResult<String> {
    let target = target.trim().to_string();
    if target.is_empty() {
        return Err(UnifiedError::bad_request("Target must not be empty"));
    }
    if target.contains('/') {
        return Err(UnifiedError::bad_request(
            "Target must be a hostname or IP address, not a URL or path",
        ));
    }
    if !valid_hostname_or_ip(&target) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid hostname or IP address: \"{target}\""
        )));
    }
    Ok(target)
}

pub(crate) fn validate_domain(domain: &str) -> UnifiedResult<String> {
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if domain.is_empty() || domain.parse::<IpAddr>().is_ok() {
        return Err(UnifiedError::bad_request(
            "Expected a domain name, not an empty value or an IP address",
        ));
    }
    if !valid_hostname_or_ip(&domain) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid domain name: \"{domain}\""
        )));
    }
    Ok(domain)
}

/// True when the address is NOT safe to probe from the server:
/// private, loopback, link-local, unspecified, reserved, or
/// multicast ranges, in either address family (including
/// IPv4-mapped IPv6 forms and IPv4-embedding IPv6 forms).
pub(crate) fn ip_is_restricted(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // IPv4-mapped / compatible forms are handled by the
            // caller through `normalize_ip`; here the plain checks.
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || unified_core::net::ipv4_is_reserved(&v4)
                || v4.is_unspecified()
                || v4.is_broadcast()
                // 0.0.0.0/8 ("this network"): only 0.0.0.0 itself is
                // `is_unspecified`, the rest of the range is equally
                // unusable as a probe target.
                || octets[0] == 0
                // Carrier-grade NAT and documentation ranges.
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            // NAT64 well-known prefix 64:ff9b::/96: the last 32 bits
            // embed an IPv4 address, which a NAT64 gateway would
            // reach directly (e.g. 64:ff9b::127.0.0.1 is loopback).
            // Re-check the embedded address the same way.
            if segments[0] == 0x0064
                && segments[1] == 0xff9b
                && segments[2] == 0
                && segments[3] == 0
                && segments[4] == 0
                && segments[5] == 0
            {
                let embedded = Ipv4Addr::new(
                    (segments[6] >> 8) as u8,
                    (segments[6] & 0xff) as u8,
                    (segments[7] >> 8) as u8,
                    (segments[7] & 0xff) as u8,
                );
                if ip_is_restricted(IpAddr::V4(embedded)) {
                    return true;
                }
            }
            // 6to4 2002::/16: the 32 bits after the prefix embed an
            // IPv4 address; re-check the embedded address as well.
            if segments[0] == 0x2002 {
                let embedded = Ipv4Addr::new(
                    (segments[1] >> 8) as u8,
                    (segments[1] & 0xff) as u8,
                    (segments[2] >> 8) as u8,
                    (segments[2] & 0xff) as u8,
                );
                if ip_is_restricted(IpAddr::V4(embedded)) {
                    return true;
                }
            }
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || matches!(segments, [0, 0, 0, 0, 0, 0xffff, _, _])
                || v6.to_ipv4_mapped().map(|v4| ip_is_restricted(IpAddr::V4(v4))).unwrap_or(false)
                || {
                    // Unique-local (fc00::/7) and link-local (fe80::/10).
                    let first = segments[0];
                    (0xfc00..=0xfdff).contains(&first) || (0xfe80..=0xfebf).contains(&first)
                }
        }
    }
}

/// Resolve a hostname and check EVERY resolved address. Returns the
/// resolved addresses on success. When `allow_private` is false any
/// restricted address fails the whole target.
pub(crate) async fn resolve_and_guard(
    host: &str,
    allow_private: bool,
) -> UnifiedResult<Vec<IpAddr>> {
    let host = host.trim();
    if host.is_empty() {
        return Err(UnifiedError::bad_request("Host must not be empty"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !allow_private && ip_is_restricted(ip) {
            return Err(UnifiedError::forbidden(format!(
                "Target {ip} is in a restricted range; probing it requires network_tools:probe_private"
            )));
        }
        return Ok(vec![ip]);
    }
    if !valid_hostname_or_ip(host) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid hostname: \"{host}\""
        )));
    }
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host, 0))
        .await
        .map_err(|error| {
            UnifiedError::bad_request(format!("Cannot resolve \"{host}\": {error}"))
        })?
        .map(|socket| socket.ip())
        .collect();
    if addrs.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "Hostname \"{host}\" did not resolve to any address"
        )));
    }
    if !allow_private {
        for ip in &addrs {
            if ip_is_restricted(*ip) {
                return Err(UnifiedError::forbidden(format!(
                    "Hostname \"{host}\" resolves to restricted address {ip}; probing it requires network_tools:probe_private"
                )));
            }
        }
    }
    Ok(addrs)
}

/// Validate a caller-supplied URL for the HTTP / webhook tools:
/// http(s) only, no embedded credentials, host passes the SSRF guard.
/// Returns the parsed URL plus its resolved addresses.
pub(crate) async fn guard_url(url_text: &str, allow_private: bool) -> UnifiedResult<(String, Vec<IpAddr>)> {
    let url_text = url_text.trim();
    if url_text.len() > 2048 {
        return Err(UnifiedError::bad_request("URL is too long"));
    }
    let parsed = url_text
        .parse::<reqwest::Url>()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid URL: \"{url_text}\"")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Only http and https URLs are allowed, not \"{other}\""
            )))
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UnifiedError::bad_request(
            "URLs with embedded credentials are not allowed",
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(UnifiedError::bad_request("URL has no host"));
    };
    let addrs = resolve_and_guard(host, allow_private).await?;
    Ok((parsed.to_string(), addrs))
}

/// Build an HTTP client pinned to the addresses that already passed
/// the SSRF guard: reqwest resolves `host` to exactly these
/// `SocketAddr`s and performs no second DNS lookup, so a DNS change
/// between validation and connect cannot steer the request at a
/// private address (DNS-rebinding TOCTOU). The original hostname
/// stays in the request URL, so the Host header and TLS SNI are
/// unchanged. IP-literal hosts need no override: no lookup happens
/// for them. Callers must re-guard and rebuild the client for every
/// redirect hop, never reuse a client built for another URL.
pub(crate) fn pinned_http_client(
    host: &str,
    addrs: &[IpAddr],
    port: u16,
    total_timeout: Duration,
) -> UnifiedResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(total_timeout)
        .user_agent("Unified-Network-Tools/1.0");
    if host.parse::<IpAddr>().is_err() {
        let sockets: Vec<SocketAddr> =
            addrs.iter().map(|ip| SocketAddr::new(*ip, port)).collect();
        if !sockets.is_empty() {
            builder = builder.resolve_to_addrs(host, &sockets);
        }
    }
    builder
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Port of `url` for the pinning override: the explicit port, or the
/// scheme default (80 / 443).
pub(crate) fn url_port_or_default(url: &reqwest::Url) -> u16 {
    url.port_or_known_default().unwrap_or_else(|| {
        if url.scheme() == "https" {
            443
        } else {
            80
        }
    })
}

pub(crate) fn clamp_timeout(requested: Option<u64>, default_secs: u64, max_secs: u64) -> Duration {
    let secs = requested.unwrap_or(default_secs).clamp(1, max_secs);
    Duration::from_secs(secs)
}

pub(crate) fn clamp_connect_timeout(requested: Option<u64>) -> Duration {
    clamp_timeout(
        requested,
        DEFAULT_CONNECT_TIMEOUT_SECS,
        MAX_CONNECT_TIMEOUT_SECS,
    )
}

pub(crate) fn clamp_total_timeout(requested: Option<u64>) -> Duration {
    clamp_timeout(requested, DEFAULT_TOTAL_TIMEOUT_SECS, MAX_TOTAL_TIMEOUT_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::network_tools::helpers::has_capability;

    #[test]
    fn valid_hostname_or_ip_accepts_names_and_ips() {
        assert!(valid_hostname_or_ip("example.com"));
        assert!(valid_hostname_or_ip("sub.example.co.uk"));
        assert!(valid_hostname_or_ip("8.8.8.8"));
        assert!(valid_hostname_or_ip("::1"));
    }

    #[test]
    fn valid_hostname_or_ip_rejects_bad_targets() {
        assert!(!valid_hostname_or_ip(""));
        assert!(!valid_hostname_or_ip("exa mple.com"));
        assert!(!valid_hostname_or_ip("example..com"));
        assert!(!valid_hostname_or_ip("-bad.com"));
        assert!(!valid_hostname_or_ip("bad-.com"));
        assert!(!valid_hostname_or_ip(".example.com"));
    }

    #[test]
    fn validate_target_rejects_urls_and_empty() {
        assert_eq!(
            validate_target("example.com").unwrap(),
            "example.com".to_string()
        );
        assert!(validate_target("").is_err());
        assert!(validate_target("http://example.com").is_err());
        assert!(validate_target("example.com/path").is_err());
    }

    #[test]
    fn validate_domain_normalizes() {
        assert_eq!(validate_domain("Example.COM.").unwrap(), "example.com");
        assert!(validate_domain("1.2.3.4").is_err());
        assert!(validate_domain("").is_err());
    }

    #[test]
    fn ip_is_restricted_blocks_private_and_loopback() {
        assert!(ip_is_restricted("10.0.0.1".parse().unwrap()));
        assert!(ip_is_restricted("192.168.1.1".parse().unwrap()));
        assert!(ip_is_restricted("127.0.0.1".parse().unwrap()));
        assert!(ip_is_restricted("192.0.2.1".parse().unwrap()));
        assert!(ip_is_restricted("100.64.0.1".parse().unwrap()));
        assert!(ip_is_restricted("::1".parse().unwrap()));
        assert!(!ip_is_restricted("8.8.8.8".parse().unwrap()));
        assert!(!ip_is_restricted("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn ip_is_restricted_blocks_ipv4_embedding_ipv6_forms() {
        // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4
        // address in the last 32 bits.
        assert!(ip_is_restricted("64:ff9b::127.0.0.1".parse().unwrap()));
        assert!(ip_is_restricted("64:ff9b::10.0.0.1".parse().unwrap()));
        assert!(ip_is_restricted("64:ff9b::169.254.169.254".parse().unwrap()));
        assert!(!ip_is_restricted("64:ff9b::8.8.8.8".parse().unwrap()));
        // 6to4 2002::/16 embeds an IPv4 address in the next 32 bits.
        assert!(ip_is_restricted("2002:7f00:1::".parse().unwrap()));
        assert!(ip_is_restricted("2002:a00:1::".parse().unwrap()));
        assert!(ip_is_restricted("2002:a9fe:a9fe::".parse().unwrap()));
        assert!(!ip_is_restricted("2002:808:808::".parse().unwrap()));
        // 0.0.0.0/8 beyond 0.0.0.0 itself.
        assert!(ip_is_restricted("0.0.0.0".parse().unwrap()));
        assert!(ip_is_restricted("0.0.0.1".parse().unwrap()));
        assert!(ip_is_restricted("0.1.2.3".parse().unwrap()));
        assert!(!ip_is_restricted("1.0.0.1".parse().unwrap()));
    }

    #[test]
    fn pinned_http_client_builds_for_hostnames_and_ip_literals() {
        let addrs: Vec<IpAddr> = vec!["93.184.216.34".parse().unwrap()];
        assert!(pinned_http_client(
            "example.com",
            &addrs,
            443,
            std::time::Duration::from_secs(10)
        )
        .is_ok());
        assert!(pinned_http_client(
            "93.184.216.34",
            &addrs,
            443,
            std::time::Duration::from_secs(10)
        )
        .is_ok());
        assert!(pinned_http_client(
            "example.com",
            &[],
            80,
            std::time::Duration::from_secs(10)
        )
        .is_ok());
    }

    #[test]
    fn url_port_or_default_uses_scheme_default() {
        let https: reqwest::Url = "https://example.com/".parse().unwrap();
        assert_eq!(url_port_or_default(&https), 443);
        let http: reqwest::Url = "http://example.com/".parse().unwrap();
        assert_eq!(url_port_or_default(&http), 80);
        let explicit: reqwest::Url = "https://example.com:8443/".parse().unwrap();
        assert_eq!(url_port_or_default(&explicit), 8443);
    }

    #[test]
    fn clamp_timeouts_apply_defaults_and_caps() {
        assert_eq!(
            clamp_connect_timeout(None),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            clamp_connect_timeout(Some(99)),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            clamp_connect_timeout(Some(0)),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            clamp_total_timeout(None),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            clamp_total_timeout(Some(99)),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            clamp_timeout(Some(3), 7, 9),
            std::time::Duration::from_secs(3)
        );
    }

    #[test]
    fn role_capabilities_cover_admin_and_viewer() {
        assert!(has_capability(Some("admin"), "network_tools:probe_private"));
        assert!(has_capability(Some("super_admin"), "network_tools:manage"));
        assert!(has_capability(Some("operator"), "network_tools:run"));
        assert!(!has_capability(Some("operator"), "network_tools:probe_private"));
        assert!(has_capability(Some("viewer"), "network_tools:view"));
        assert!(!has_capability(Some("viewer"), "network_tools:run"));
        assert!(!has_capability(None, "network_tools:view"));
        assert!(!has_capability(Some("unknown"), "network_tools:view"));
    }
}
