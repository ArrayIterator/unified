// security -- Network scan (target parsing and port probing).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::{error_response, success_response};
use actix_web::{HttpRequest, HttpResponse, web};
use regex::Regex;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::auth::*;
use super::constants::*;
use super::types::*;

/// Environment variable holding the explicit admin allowlist of
/// private scan targets: a comma-separated list of hostnames or IP
/// literals (for example `127.0.0.1, localhost, 10.0.0.9`). A
/// loopback, private, link-local or reserved target is only scanned
/// when it appears on this list, mirroring the explicit-allowlist
/// policy the web3 module applies to private chain endpoints.
pub(crate) const SCAN_ALLOWED_PRIVATE_HOSTS_ENV: &str = "SECURITY_SCAN_ALLOWED_PRIVATE_HOSTS";

/// Split an allowlist value into its entries, dropping blanks.
pub(crate) fn parse_allowlist_env(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// The admin-configured private-target allowlist for this process.
pub(crate) fn scan_allowed_private_hosts() -> Vec<String> {
    std::env::var(SCAN_ALLOWED_PRIVATE_HOSTS_ENV)
        .map(|value| parse_allowlist_env(&value))
        .unwrap_or_default()
}

/// The domain-name pattern, compiled once and shared by every
/// `parse_target` call.
fn domain_pattern() -> &'static Regex {
    static DOMAIN_PATTERN: OnceLock<Regex> = OnceLock::new();
    DOMAIN_PATTERN.get_or_init(|| {
        Regex::new(
            r"^(?i)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$",
        )
        .expect("domain validation pattern is valid")
    })
}

/// True when `addr` is loopback, private, link-local, unspecified or
/// reserved -- an address that must never be probed from the outside
/// unless an admin explicitly allowlisted it. Mirrors the web3
/// module's private-host policy, plus IPv6 link-local (`fe80::/10`)
/// and IPv4-mapped IPv6 addresses.
pub(crate) fn is_private_ip(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(addr) => {
            addr.is_loopback()
                || addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || unified_core::net::ipv4_is_reserved(&addr)
        }
        IpAddr::V6(addr) => {
            if let Some(mapped) = addr.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(mapped));
            }
            addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_unique_local()
                || is_ipv6_link_local(addr)
        }
    }
}

/// True when `addr` is an IPv6 link-local address (`fe80::/10`).
fn is_ipv6_link_local(addr: &Ipv6Addr) -> bool {
    addr.segments()[0] & 0xffc0 == 0xfe80
}

/// True when `host` is `localhost`, a `.local` / `.internal` name,
/// or a loopback, private, link-local, unspecified or reserved IP
/// literal. A public domain name is never private by this check: its
/// resolved addresses are checked separately in `probe_host`, so a
/// name that resolves to an internal address is caught too.
pub(crate) fn is_private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") || lower.ends_with(".internal") {
        return true;
    }
    match lower.parse::<IpAddr>() {
        Ok(addr) => is_private_ip(&addr),
        Err(_) => false,
    }
}

/// True when `host` appears on the admin allowlist (exact,
/// case-insensitive match of the hostname or IP literal).
pub(crate) fn is_allowlisted(host: &str, allowed_private_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowed_private_hosts
        .iter()
        .any(|allowed| allowed.trim().to_ascii_lowercase() == host)
}

/// Extract the bare host from a scan target. Accepts a bare host
/// (`example.com`, `localhost`, `8.8.8.8`, `::1`), a host with a
/// port (`example.com:8443`, `[::1]:8080`), or a full URL with an
/// optional path (`https://example.com:8443/path`). Bracketed IPv6
/// is handled by the URL parser instead of splitting on `:`, which
/// used to mangle it (`[::1]:8080` became `"["`).
fn extract_host(target: &str) -> UnifiedResult<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request("Scan target must not be empty"));
    }
    // A bare IP literal (v4 or v6, without brackets or a port) is a
    // host as-is; the URL parser would reject a bare `::1`.
    if trimmed.parse::<IpAddr>().is_ok() {
        return Ok(trimmed.to_string());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let url = url::Url::parse(&with_scheme).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Invalid scan target: \"{target}\" is not a valid URL or host"
        ))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Scan target must not be empty"))?;
    if host.is_empty() {
        return Err(UnifiedError::bad_request("Scan target must not be empty"));
    }
    Ok(host.to_string())
}

/// Validate that a target is a domain name or an IP address, with an
/// optional port. Returns the bare host (without port or scheme).
/// Loopback, private, link-local and reserved targets are rejected
/// unless they are on the admin allowlist
/// (`SCAN_ALLOWED_PRIVATE_HOSTS_ENV`), so the endpoint cannot be
/// used as an internal-network port-scan oracle.
pub(crate) fn parse_target(target: &str) -> UnifiedResult<String> {
    parse_target_allowed(target, &scan_allowed_private_hosts())
}

/// `parse_target` with an explicit admin allowlist of private
/// targets (see `SCAN_ALLOWED_PRIVATE_HOSTS_ENV`).
pub(crate) fn parse_target_allowed(
    target: &str,
    allowed_private_hosts: &[String],
) -> UnifiedResult<String> {
    let host = extract_host(target)?;
    if !host.eq_ignore_ascii_case("localhost")
        && host.parse::<IpAddr>().is_err()
        && !domain_pattern().is_match(&host)
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid scan target: \"{host}\" is not a valid domain name or IP address"
        )));
    }
    if is_private_host(&host) && !is_allowlisted(&host, allowed_private_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "Scan target \"{host}\" is a loopback, private, link-local or reserved address; private targets require an explicit admin allowlist entry ({SCAN_ALLOWED_PRIVATE_HOSTS_ENV})"
        )));
    }
    Ok(host)
}

/// Probe whether a TCP port on a resolved socket address accepts
/// connections.
pub(crate) fn port_open_addr(addr: SocketAddr, timeout: Duration) -> (bool, u64) {
    let started = Instant::now();
    let open = TcpStream::connect_timeout(&addr, timeout).is_ok();
    (open, started.elapsed().as_millis() as u64)
}

/// Resolve `host` once, reject it when any resolved address is
/// loopback, private, link-local or reserved (unless the host is
/// allowlisted), then probe the resolved address directly. Resolving
/// once and connecting to the resolved address -- instead of
/// resolving again inside the probe -- keeps a hostile name from
/// swapping its answer between the check and the connect. A host
/// that does not resolve at all is reported as closed on both ports,
/// matching the previous behaviour.
pub(crate) fn probe_host(
    host: &str,
    allowed_private_hosts: &[String],
) -> UnifiedResult<(bool, u64, bool, u64)> {
    let timeout = Duration::from_secs(5);
    let lookup = if host.contains(':') {
        format!("[{host}]:443")
    } else {
        format!("{host}:443")
    };
    let addrs: Vec<SocketAddr> = match lookup.to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(_) => return Ok((false, 0, false, 0)),
    };
    let Some(first) = addrs.first().copied() else {
        return Ok((false, 0, false, 0));
    };
    if !is_allowlisted(host, allowed_private_hosts) {
        for addr in &addrs {
            if is_private_ip(&addr.ip()) {
                return Err(UnifiedError::forbidden(format!(
                    "Scan target \"{host}\" resolves to a private or loopback address ({}) and is not on the scan allowlist",
                    addr.ip()
                )));
            }
        }
    }
    let mut tls_addr = first;
    tls_addr.set_port(443);
    let mut http_addr = first;
    http_addr.set_port(80);
    let (tls_open, tls_latency) = port_open_addr(tls_addr, timeout);
    let (http_open, http_latency) = port_open_addr(http_addr, timeout);
    Ok((tls_open, tls_latency, http_open, http_latency))
}

pub(crate) async fn scan(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ScanRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    let host = match parse_target(&body.target) {
        Ok(host) => host,
        Err(error) => return error_response(&error, debug),
    };
    // Network probes block, so run them off the async worker threads.
    let allowed_private_hosts = scan_allowed_private_hosts();
    let host_for_probe = host.clone();
    let probed = web::block(move || probe_host(&host_for_probe, &allowed_private_hosts)).await;
    let (tls_open, tls_latency, http_open, http_latency) = match probed {
        Ok(Ok(values)) => values,
        Ok(Err(error)) => return error_response(&error, debug),
        Err(error) => {
            return error_response(
                &UnifiedError::internal(format!("Scan failed: {error}")),
                debug,
            );
        }
    };

    let mut findings = Vec::new();
    if tls_open {
        findings.push(ScanFinding {
            severity: "info".to_string(),
            code: "tls_port_open".to_string(),
            message: format!("TCP port 443 is open ({tls_latency} ms); TLS is served"),
        });
    } else {
        findings.push(ScanFinding {
            severity: "high".to_string(),
            code: "tls_port_closed".to_string(),
            message: "TCP port 443 is closed or unreachable; no TLS endpoint was found".to_string(),
        });
    }
    if http_open {
        findings.push(ScanFinding {
            severity: "info".to_string(),
            code: "http_port_open".to_string(),
            message: format!("TCP port 80 is open ({http_latency} ms)"),
        });
    } else {
        findings.push(ScanFinding {
            severity: "low".to_string(),
            code: "http_port_closed".to_string(),
            message: "TCP port 80 is closed or unreachable".to_string(),
        });
    }
    if tls_open && !http_open {
        findings.push(ScanFinding {
            severity: "info".to_string(),
            code: "https_only".to_string(),
            message: "Host serves HTTPS only; plain HTTP is not reachable".to_string(),
        });
    }

    success_response(
        ScanResponse {
            target: body.target.clone(),
            host,
            findings,
            scanned_at: chrono::Utc::now(),
        },
        200,
    )
}

// Tests for the scan-target parser in this file. `probe_host` and
// `port_open_addr` do real networking and are intentionally not
// tested here.
#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn target_parses_url_with_port_and_path() {
        assert_eq!(
            parse_target("https://example.com:8443/path").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn target_rejects_loopback_ip() {
        // Explicit empty allowlist: independent of the ambient
        // SECURITY_SCAN_ALLOWED_PRIVATE_HOSTS value.
        assert!(parse_target_allowed("http://127.0.0.1", &[]).is_err());
        assert!(parse_target_allowed("127.0.0.1", &[]).is_err());
    }

    #[test]
    fn target_rejects_private_ips() {
        assert!(parse_target_allowed("10.0.0.5", &[]).is_err());
        assert!(parse_target_allowed("192.168.1.1", &[]).is_err());
        assert!(parse_target_allowed("172.16.0.1", &[]).is_err());
    }

    #[test]
    fn target_rejects_link_local_and_reserved_ips() {
        assert!(parse_target_allowed("169.254.10.20", &[]).is_err());
        assert!(parse_target_allowed("0.0.0.0", &[]).is_err());
    }

    #[test]
    fn target_rejects_local_suffix_names() {
        assert!(parse_target_allowed("printer.local", &[]).is_err());
        assert!(parse_target_allowed("db.internal", &[]).is_err());
    }

    #[test]
    fn target_rejects_ipv6_loopback_and_link_local() {
        assert!(parse_target_allowed("http://[::1]", &[]).is_err());
        assert!(parse_target_allowed("[::1]:8080", &[]).is_err());
        assert!(parse_target_allowed("[fe80::1]", &[]).is_err());
        assert!(parse_target_allowed("::1", &[]).is_err());
    }

    #[test]
    fn target_accepts_public_ipv4_and_ipv6() {
        assert_eq!(parse_target("8.8.8.8").unwrap(), "8.8.8.8");
        assert_eq!(
            parse_target("http://[2606:4700:4700::1111]").unwrap(),
            "2606:4700:4700::1111"
        );
    }

    #[test]
    fn target_parses_bracketed_ipv6_with_port() {
        assert_eq!(
            parse_target_allowed("http://[::1]:8080", &allowlist(&["::1"])).unwrap(),
            "::1"
        );
    }

    #[test]
    fn target_accepts_localhost_only_via_allowlist() {
        assert!(parse_target_allowed("localhost", &[]).is_err());
        assert_eq!(
            parse_target_allowed("localhost", &allowlist(&["localhost"])).unwrap(),
            "localhost"
        );
        assert_eq!(
            parse_target_allowed("http://127.0.0.1", &allowlist(&["127.0.0.1"])).unwrap(),
            "127.0.0.1"
        );
    }

    #[test]
    fn target_allowlist_match_is_exact() {
        assert!(parse_target_allowed("http://127.0.0.1", &allowlist(&["10.0.0.1"])).is_err());
        assert!(parse_target_allowed("localhost", &allowlist(&["127.0.0.1"])).is_err());
    }

    #[test]
    fn allowlist_env_parses_comma_separated_entries() {
        assert_eq!(
            parse_allowlist_env(" 127.0.0.1, localhost ,, 10.0.0.9 "),
            vec![
                "127.0.0.1".to_string(),
                "localhost".to_string(),
                "10.0.0.9".to_string()
            ]
        );
        assert!(parse_allowlist_env("").is_empty());
    }

    #[test]
    fn target_accepts_bare_host() {
        assert_eq!(parse_target("example.com").unwrap(), "example.com");
    }

    #[test]
    fn target_rejects_empty_input() {
        assert!(parse_target("").is_err());
    }

    #[test]
    fn target_rejects_invalid_host() {
        assert!(parse_target("not a host!!").is_err());
    }

    #[test]
    fn target_rejects_url_without_host() {
        assert!(parse_target("https://").is_err());
    }

    #[test]
    fn private_host_detection_matches_web3_policy() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("10.1.2.3"));
        assert!(is_private_host("169.254.169.254"));
        assert!(is_private_host("::1"));
        assert!(!is_private_host("8.8.8.8"));
        assert!(!is_private_host("example.com"));
    }
}
