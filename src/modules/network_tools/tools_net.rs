//! WHOIS, HTTP check, IP info, subnet, webhook and blacklist tools.

use unified_core::context::{AppContext, GeoInfo};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use crate::modules::geo;
use crate::modules::network::DnsVerifier;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use super::types::{DEFAULT_DNSBL_ZONES, MAX_HTTP_REDIRECTS, MAX_TOTAL_TIMEOUT_SECS, MAX_WEBHOOK_BODY_BYTES, MAX_WHOIS_TEXT_CHARS, ToolResult};
use super::validation::{clamp_total_timeout, guard_url, pinned_http_client, resolve_and_guard, url_port_or_default, valid_hostname_or_ip, validate_target};

// WHOIS (TCP port 43, one referral follow).

pub(crate) async fn whois_query(server: &str, query: &str, timeout_secs: u64) -> UnifiedResult<String> {
    // The server name is caller input for the first hop and a
    // referral taken from a WHOIS response (attacker-influenced
    // data) for the second, so it passes the same SSRF guard as any
    // other probe target: a link-local, loopback, or other
    // restricted address - hostname or IP literal - is refused
    // before any socket opens. The connection is then pinned to the
    // guarded addresses, never re-resolved at connect time.
    let addrs = resolve_and_guard(server, false).await?;
    let mut last_error: Option<String> = None;
    for ip in addrs {
        let addr = SocketAddr::new(ip, 43);
        match timeout(
            Duration::from_secs(timeout_secs),
            TokioTcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(mut stream)) => {
                let _ = stream.write_all(format!("{query}\r\n").as_bytes()).await;
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let read_result = timeout(Duration::from_secs(timeout_secs), async {
                    loop {
                        match stream.read(&mut chunk).await {
                            Ok(0) => break,
                            Ok(read) => {
                                buf.extend_from_slice(&chunk[..read]);
                                if buf.len() > 65536 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                })
                .await;
                if read_result.is_err() {
                    return Err(UnifiedError::bad_request(
                        "WHOIS query timed out while reading the response",
                    ));
                }
                return Ok(String::from_utf8_lossy(&buf).to_string());
            }
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("connect timed out".to_string()),
        }
    }
    Err(UnifiedError::bad_request(format!(
        "Cannot reach WHOIS server \"{server}\": {}",
        last_error.unwrap_or_default()
    )))
}

pub(crate) fn whois_parse(raw: &str) -> HashMap<String, String> {
    let mut fields: HashMap<String, String> = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            if !value.is_empty()
                && matches!(
                    key.as_str(),
                    "domain name"
                        | "registrar"
                        | "creation date"
                        | "updated date"
                        | "registry expiry date"
                        | "expiry date"
                        | "name server"
                        | "status"
                        | "registrant organization"
                        | "orgname"
                        | "netname"
                        | "country"
                        | "cidr"
                        | "inetnum"
                        | "whois server"
                )
            {
                fields
                    .entry(key)
                    .and_modify(|existing| {
                        if !existing.contains(&value) {
                            existing.push_str("; ");
                            existing.push_str(&value);
                        }
                    })
                    .or_insert(value);
            }
        }
    }
    fields
}

pub(crate) async fn tool_whois(target: &str, timeout_secs: u64) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = target.trim().to_string();
    if target.is_empty() || target.len() > 253 {
        return Err(UnifiedError::bad_request("WHOIS target must not be empty"));
    }
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    // IP targets go to the IANA / RIR referral chain; domains go to
    // the IANA WHOIS server first, then follow one referral.
    let is_ip = target.parse::<IpAddr>().is_ok();
    let mut server = "whois.iana.org".to_string();
    let mut raw = whois_query(&server, &target, timeout_secs).await?;
    if !is_ip {
        let fields = whois_parse(&raw);
        if let Some(referral) = fields.get("whois server") {
            let referral = referral
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !referral.is_empty()
                && referral != server
                && valid_hostname_or_ip(&referral)
            {
                server = referral;
                raw = whois_query(&server, &target, timeout_secs).await?;
            }
        }
    } else {
        // For IPs, follow the referral once as well (IANA points at
        // the responsible RIR).
        let fields = whois_parse(&raw);
        if let Some(referral) = fields.get("whois server") {
            let referral = referral
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !referral.is_empty() && referral != server && valid_hostname_or_ip(&referral) {
                server = referral;
                raw = whois_query(&server, &target, timeout_secs).await?;
            }
        }
    }
    let fields = whois_parse(&raw);
    let truncated: String = raw.chars().take(MAX_WHOIS_TEXT_CHARS).collect();
    Ok(ToolResult {
        tool: "whois".to_string(),
        target,
        outcome: "success".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some(format!("tcp/43 via {server}")),
        result: json!({
            "server": server,
            "fields": fields,
            "raw": truncated,
            "raw_truncated": raw.chars().count() > MAX_WHOIS_TEXT_CHARS,
        }),
        notes: Vec::new(),
    })
}

// HTTP header / status check with manual redirect following so every
// hop is re-checked by the SSRF guard.

pub(crate) async fn tool_http_check(
    url_text: &str,
    method: Option<String>,
    expected_status: Option<u16>,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let method = method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
    if !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
        return Err(UnifiedError::bad_request(
            "HTTP check supports GET, HEAD, and OPTIONS only",
        ));
    }
    let total_timeout = clamp_total_timeout(Some(timeout_secs));
    let (mut current_url, mut current_addrs) = guard_url(url_text, allow_private).await?;
    let mut redirects: Vec<String> = Vec::new();
    let mut last_status: u16 = 0;
    let mut last_headers: HashMap<String, String> = HashMap::new();
    let mut last_body_size: u64 = 0;
    let dns_ms: Option<f64> = None;
    let connect_ms: Option<f64> = None;
    let mut ttfb_ms: Option<f64> = None;
    for _hop in 0..=MAX_HTTP_REDIRECTS {
        // Rebuild the client for every hop from the addresses that
        // passed the guard for this hop's URL: the client performs
        // no second DNS lookup, so a hostname that rebinds between
        // validation and connect cannot steer the request at a
        // private address. The original hostname stays in the URL,
        // so the Host header and TLS SNI are unchanged.
        let parsed_url = reqwest::Url::parse(&current_url).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid URL: \"{current_url}\""))
        })?;
        let host = parsed_url.host_str().unwrap_or("").to_string();
        let port = url_port_or_default(&parsed_url);
        let client = pinned_http_client(&host, &current_addrs, port, total_timeout)?;
        let hop_start = Instant::now();
        let request = client
            .request(
                method.parse().map_err(|_| {
                    UnifiedError::bad_request("Invalid HTTP method")
                })?,
                current_url.clone(),
            )
            .build()
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build request: {error}")))?;
        let response = timeout(total_timeout, client.execute(request))
            .await
            .map_err(|_| UnifiedError::bad_request("HTTP check timed out"))?
            .map_err(|error| UnifiedError::bad_request(format!("HTTP request failed: {error}")))?;
        ttfb_ms = Some(hop_start.elapsed().as_secs_f64() * 1000.0);
        last_status = response.status().as_u16();
        last_headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect();
        let is_redirect = (300..400).contains(&last_status);
        let next_location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Read the body only to measure its size; it is never stored.
        let body_bytes = response
            .bytes()
            .await
            .map_err(|error| UnifiedError::bad_request(format!("Cannot read response body: {error}")))?;
        last_body_size = body_bytes.len() as u64;
        if is_redirect {
            let Some(location) = next_location else {
                break;
            };
            let next_url = reqwest::Url::parse(&current_url)
                .and_then(|base| base.join(&location))
                .map_err(|_| {
                    UnifiedError::bad_request(format!("Invalid redirect target: \"{location}\""))
                })?;
            // Re-check the redirect target with the same SSRF guard,
            // and pin the next hop's client to the new addresses.
            let (checked, checked_addrs) = guard_url(next_url.as_str(), allow_private).await?;
            redirects.push(checked.clone());
            current_url = checked;
            current_addrs = checked_addrs;
            continue;
        }
        break;
    }
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    let outcome = match expected_status {
        Some(expected) if last_status != expected => "failed",
        Some(_) => "success",
        None if (200..400).contains(&last_status) => "success",
        None => "failed",
    };
    // DNS / connect timings are not separable through reqwest's
    // public API; report them as not measured rather than inventing
    // numbers.
    Ok(ToolResult {
        tool: "http_check".to_string(),
        target: url_text.to_string(),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("reqwest, manual redirects".to_string()),
        result: json!({
            "method": method,
            "status": last_status,
            "expected_status": expected_status,
            "headers": last_headers,
            "final_url": current_url,
            "redirects": redirects,
            "redirect_count": redirects.len(),
            "body_size_bytes": last_body_size,
            "timings_ms": {
                "dns": dns_ms,
                "connect": connect_ms,
                "ttfb": ttfb_ms,
                "total": total_ms,
            },
        }),
        notes: vec![
            "DNS and connect timings are reported as null: reqwest does not expose per-phase timings, and this module does not invent them.".to_string(),
            "The response body is measured for size only and is never stored.".to_string(),
            "Each hop connects only to the addresses that passed the SSRF guard for that hop's URL; the hostname is not re-resolved at connect time.".to_string(),
        ],
    })
}

// IP info + Geo (delegates to the geo module) and reverse DNS.

pub(crate) async fn tool_ip_info(ctx: &AppContext, target: &str) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = target.trim().to_string();
    let ip: IpAddr = target
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid IP address: \"{target}\"")))?;
    let geo_info: GeoInfo = geo::lookup_geo(ctx, &target)?;
    // Reverse DNS through the shared DNS verifier (PTR records).
    let verifier = DnsVerifier::new();
    let ptr_names: Vec<String> = verifier.lookup_ptr(ip).await.unwrap_or_default();
    Ok(ToolResult {
        tool: "ip_info".to_string(),
        target,
        outcome: "success".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("geo module lookup + dns verifier ptr".to_string()),
        result: json!({
            "geo": geo_info,
            "reverse_dns": ptr_names,
        }),
        notes: Vec::new(),
    })
}

// Subnet / CIDR calculator (pure computation, IPv4 + IPv6).

pub(crate) fn tool_subnet(cidr_text: &str, contains: Option<&str>) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let (addr_text, prefix_text) = cidr_text
        .split_once('/')
        .ok_or_else(|| UnifiedError::bad_request("CIDR must look like 192.168.0.0/24"))?;
    let prefix: u32 = prefix_text
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request("CIDR prefix must be a number"))?;
    let addr: IpAddr = addr_text
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid IP address in CIDR: \"{addr_text}\"")))?;
    let (result, contains_result) = match addr {
        IpAddr::V4(v4) => {
            if prefix > 32 {
                return Err(UnifiedError::bad_request("IPv4 prefix must be 0-32"));
            }
            let ip_u32 = u32::from(v4);
            let mask: u32 = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            let network = ip_u32 & mask;
            let broadcast = network | !mask;
            let host_bits = 32 - prefix;
            let usable_count: u128 = if host_bits == 0 {
                1
            } else if host_bits == 1 {
                2
            } else {
                (1u128 << host_bits) - 2
            };
            let (first, last) = if prefix >= 31 {
                (network, broadcast)
            } else {
                (network + 1, broadcast - 1)
            };
            let contains_check = match contains {
                Some(candidate) => {
                    let candidate_ip: IpAddr = candidate.trim().parse().map_err(|_| {
                        UnifiedError::bad_request(format!("Invalid contains address: \"{candidate}\""))
                    })?;
                    match candidate_ip {
                        IpAddr::V4(candidate_v4) => {
                            let candidate_u32 = u32::from(candidate_v4);
                            Some((candidate_u32 & mask) == network)
                        }
                        IpAddr::V6(_) => Some(false),
                    }
                }
                None => None,
            };
            (
                json!({
                    "family": "ipv4",
                    "input": cidr_text,
                    "network": Ipv4Addr::from(network).to_string(),
                    "broadcast": Ipv4Addr::from(broadcast).to_string(),
                    "netmask": Ipv4Addr::from(mask).to_string(),
                    "wildcard_mask": Ipv4Addr::from(!mask).to_string(),
                    "first_usable": Ipv4Addr::from(first).to_string(),
                    "last_usable": Ipv4Addr::from(last).to_string(),
                    "prefix": prefix,
                    "total_addresses": 1u128 << host_bits,
                    "usable_hosts": usable_count,
                    "cidr_range": format!("{} - {}", Ipv4Addr::from(network), Ipv4Addr::from(broadcast)),
                }),
                contains_check,
            )
        }
        IpAddr::V6(v6) => {
            if prefix > 128 {
                return Err(UnifiedError::bad_request("IPv6 prefix must be 0-128"));
            }
            let ip_u128 = u128::from(v6);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            let network = ip_u128 & mask;
            let last = network | !mask;
            let host_bits = 128 - prefix;
            let total: u128 = if host_bits >= 128 {
                u128::MAX
            } else {
                1u128 << host_bits
            };
            let contains_check = match contains {
                Some(candidate) => {
                    let candidate_ip: IpAddr = candidate.trim().parse().map_err(|_| {
                        UnifiedError::bad_request(format!("Invalid contains address: \"{candidate}\""))
                    })?;
                    match candidate_ip {
                        IpAddr::V6(candidate_v6) => {
                            Some((u128::from(candidate_v6) & mask) == network)
                        }
                        IpAddr::V4(_) => Some(false),
                    }
                }
                None => None,
            };
            (
                json!({
                    "family": "ipv6",
                    "input": cidr_text,
                    "network": Ipv6Addr::from(network).to_string(),
                    "broadcast": null,
                    "netmask": Ipv6Addr::from(mask).to_string(),
                    "wildcard_mask": Ipv6Addr::from(!mask).to_string(),
                    "first_usable": Ipv6Addr::from(network).to_string(),
                    "last_usable": Ipv6Addr::from(last).to_string(),
                    "prefix": prefix,
                    "total_addresses": total.to_string(),
                    "usable_hosts": total.to_string(),
                    "cidr_range": format!("{} - {}", Ipv6Addr::from(network), Ipv6Addr::from(last)),
                }),
                contains_check,
            )
        }
    };
    let mut result = result;
    if let Some(contains_check) = contains_result {
        result["contains"] = json!(contains_check);
        result["contains_address"] = json!(contains);
    }
    Ok(ToolResult {
        tool: "subnet".to_string(),
        target: cidr_text.to_string(),
        outcome: "success".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("pure computation".to_string()),
        result,
        notes: vec!["IPv6 has no broadcast address; first/last usable are the network range ends.".to_string()],
    })
}

// Webhook tester.

pub(crate) async fn tool_webhook_test(
    url_text: &str,
    payload: Value,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let total_timeout = clamp_total_timeout(Some(timeout_secs));
    let (url, addrs) = guard_url(url_text, allow_private).await?;
    // Pin the connection to the guarded addresses (same
    // DNS-rebinding protection as the HTTP checker); the original
    // hostname stays in the URL, so Host and TLS SNI are unchanged.
    let parsed_url = reqwest::Url::parse(&url)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid URL: \"{url}\"")))?;
    let host = parsed_url.host_str().unwrap_or("").to_string();
    let port = url_port_or_default(&parsed_url);
    let client = pinned_http_client(&host, &addrs, port, total_timeout)?;
    let response = timeout(
        total_timeout,
        client.post(url.clone()).json(&payload).send(),
    )
    .await
    .map_err(|_| UnifiedError::bad_request("Webhook test timed out"))?
    .map_err(|error| UnifiedError::bad_request(format!("Webhook request failed: {error}")))?;
    let status = response.status().as_u16();
    let headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    // Redirects are NOT followed for webhook tests: a redirect to a
    // private target must never be chased silently.
    if (300..400).contains(&status) {
        return Ok(ToolResult {
            tool: "webhook_test".to_string(),
            target: url_text.to_string(),
            outcome: "failed".to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            method: Some("reqwest post, no redirect following".to_string()),
            result: json!({
                "url": url,
                "status": status,
                "headers": headers,
                "body": null,
                "body_truncated": false,
                "redirect_not_followed": true,
            }),
            notes: vec!["The endpoint redirected; redirects are never followed by the webhook tester.".to_string()],
        });
    }
    let body_bytes = response
        .bytes()
        .await
        .map_err(|error| UnifiedError::bad_request(format!("Cannot read webhook response: {error}")))?;
    let truncated = body_bytes.len() > MAX_WEBHOOK_BODY_BYTES;
    let body_text = String::from_utf8_lossy(&body_bytes[..body_bytes.len().min(MAX_WEBHOOK_BODY_BYTES)]).to_string();
    let outcome = if (200..300).contains(&status) {
        "success"
    } else {
        "failed"
    };
    Ok(ToolResult {
        tool: "webhook_test".to_string(),
        target: url_text.to_string(),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("reqwest post, no redirect following".to_string()),
        result: json!({
            "url": url,
            "status": status,
            "headers": headers,
            "body": body_text,
            "body_truncated": truncated,
            "redirect_not_followed": false,
        }),
        notes: vec!["The connection was pinned to the addresses that passed the SSRF guard; the hostname was not re-resolved at connect time.".to_string()],
    })
}

// DNS blacklist check.

pub(crate) async fn tool_blacklist(
    target: &str,
    zones: Option<Vec<String>>,
    timeout_secs: u64,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    let addrs = resolve_and_guard(&target, false).await?;
    let ip = addrs[0];
    let IpAddr::V4(v4) = ip else {
        return Err(UnifiedError::bad_request(
            "DNS blacklist checks only support IPv4 targets",
        ));
    };
    let octets = v4.octets();
    let reversed = format!("{}.{}.{}.{}", octets[3], octets[2], octets[1], octets[0]);
    let zones: Vec<String> = match zones {
        Some(list) if !list.is_empty() => {
            let mut cleaned: Vec<String> = Vec::new();
            for zone in list {
                let zone = zone.trim().trim_end_matches('.').to_lowercase();
                if zone.is_empty() || !valid_hostname_or_ip(&zone) {
                    return Err(UnifiedError::bad_request(format!(
                        "Invalid DNSBL zone: \"{zone}\""
                    )));
                }
                cleaned.push(zone);
            }
            cleaned
        }
        _ => DEFAULT_DNSBL_ZONES.iter().map(|z| z.to_string()).collect(),
    };
    let verifier = DnsVerifier::new();
    let mut results: Vec<Value> = Vec::new();
    let mut listed_count = 0usize;
    for zone in &zones {
        let query = format!("{reversed}.{zone}");
        let listed = match timeout(
            Duration::from_secs(timeout_secs),
            verifier.lookup_a(&query),
        )
        .await
        {
            Ok(Ok(records)) => records
                .iter()
                .any(|ip| matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 127)),
            _ => false,
        };
        if listed {
            listed_count += 1;
        }
        results.push(json!({
            "zone": zone,
            "listed": listed,
            "query": query,
        }));
    }
    Ok(ToolResult {
        tool: "blacklist".to_string(),
        target,
        outcome: if listed_count == 0 {
            "success".to_string()
        } else {
            "failed".to_string()
        },
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("dnsbl a-record queries".to_string()),
        result: json!({
            "ip": ip.to_string(),
            "zones_checked": zones.len(),
            "listed_count": listed_count,
            "results": results,
        }),
        notes: vec!["A zone that does not answer is reported as not-listed, not as an error; absence of an answer is not proof of absence from the list.".to_string()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whois_parse_reads_known_fields() {
        let raw = "Domain Name: EXAMPLE.COM
Registrar: Example Registrar
            Creation Date: 2020-01-01T00:00:00Z
";
        let fields = whois_parse(raw);
        assert_eq!(fields.get("domain name").map(String::as_str), Some("EXAMPLE.COM"));
        assert_eq!(
            fields.get("registrar").map(String::as_str),
            Some("Example Registrar")
        );
        assert_eq!(
            fields.get("creation date").map(String::as_str),
            Some("2020-01-01T00:00:00Z")
        );
    }

    #[test]
    fn whois_parse_ignores_comments_and_unknown_keys() {
        let raw = "% comment line
# another comment
Foo: bar
Domain Name: EXAMPLE.COM
";
        let fields = whois_parse(raw);
        assert!(!fields.contains_key("foo"));
        assert_eq!(fields.get("domain name").map(String::as_str), Some("EXAMPLE.COM"));
    }

    #[test]
    fn whois_parse_joins_repeated_name_servers() {
        let raw = "Name Server: NS1.EXAMPLE.COM
Name Server: NS2.EXAMPLE.COM
";
        let fields = whois_parse(raw);
        assert_eq!(
            fields.get("name server").map(String::as_str),
            Some("NS1.EXAMPLE.COM; NS2.EXAMPLE.COM")
        );
    }

    #[test]
    fn whois_parse_skips_empty_values() {
        let raw = "Registrar: 
Domain Name: X.COM
";
        let fields = whois_parse(raw);
        assert!(!fields.contains_key("registrar"));
        assert_eq!(fields.get("domain name").map(String::as_str), Some("X.COM"));
    }

    /// A WHOIS referral server is attacker-influenced data: a
    /// restricted address (here an IP literal, so no network is
    /// needed to prove the guard fires) must be refused before any
    /// TCP/43 socket opens.
    #[tokio::test]
    async fn whois_query_rejects_restricted_referral_server() {
        // Link-local / metadata address as a referral "server".
        let error = whois_query("169.254.169.254", "example.com", 5)
            .await
            .unwrap_err();
        assert!(
            error.message.contains("restricted"),
            "unexpected error: {}",
            error.message
        );
        // Loopback literal likewise.
        assert!(whois_query("127.0.0.1", "example.com", 5)
            .await
            .is_err());
        // NAT64 embedding of loopback likewise.
        assert!(whois_query("64:ff9b::127.0.0.1", "example.com", 5)
            .await
            .is_err());
    }

    /// The same guard applies to the referral hostname path: an
    /// unresolvable name is refused by resolution, never by a
    /// connect attempt to an unchecked address.
    #[tokio::test]
    async fn whois_query_rejects_unresolvable_server() {
        assert!(whois_query("nonexistent.invalid", "example.com", 5)
            .await
            .is_err());
    }
}
