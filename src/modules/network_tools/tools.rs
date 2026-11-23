//! DNS, ping, traceroute and port-check tool implementations.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use crate::modules::network::DnsVerifier;
use hickory_proto::rr::{Name, RData, RecordType};
use hickory_resolver::config::ResolverConfig;
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::TokioResolver;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream as TokioTcpStream;
use tokio::time::timeout;
use super::types::{MAX_BANNER_BYTES, MAX_PING_COUNT, MAX_TOTAL_TIMEOUT_SECS, MAX_TRACEROUTE_HOPS, ToolResult};
use super::validation::{clamp_connect_timeout, resolve_and_guard, validate_domain, validate_target};

// ---------------------------------------------------------------------------
// Tool implementations. Each returns a `ToolResult`; handlers record
// the run and wrap it in the standard envelope.
// ---------------------------------------------------------------------------

/// DNS lookup for one record type. SOA is resolved through hickory
/// directly because `DnsVerifier` does not expose an SOA lookup.
pub(crate) async fn tool_dns_lookup(target: &str, record_type: &str) -> UnifiedResult<Value> {
    let domain = validate_domain(target)?;
    let verifier = DnsVerifier::new();
    let rtype = record_type.trim().to_uppercase();
    match rtype.as_str() {
        "A" => {
            let addrs = verifier.lookup_a(&domain).await?;
            Ok(json!({ "record_type": "A", "domain": domain, "records": addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>() }))
        }
        "AAAA" => {
            let addrs = verifier.lookup_aaaa(&domain).await?;
            Ok(json!({ "record_type": "AAAA", "domain": domain, "records": addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>() }))
        }
        "MX" => {
            let records = verifier.lookup_mx(&domain).await?;
            Ok(json!({ "record_type": "MX", "domain": domain, "records": records }))
        }
        "TXT" => {
            let records = verifier.lookup_txt(&domain).await?;
            Ok(json!({ "record_type": "TXT", "domain": domain, "records": records }))
        }
        "CNAME" => {
            let record = verifier.lookup_cname(&domain).await?;
            Ok(json!({ "record_type": "CNAME", "domain": domain, "records": record.into_iter().collect::<Vec<_>>() }))
        }
        "NS" => {
            let records = verifier.lookup_ns(&domain).await?;
            Ok(json!({ "record_type": "NS", "domain": domain, "records": records }))
        }
        "SOA" => {
            // Same resolver construction as `DnsVerifier` (which does
            // not expose an SOA lookup): default config, parsed Name.
            let name: Name = format!("{domain}.")
                .parse()
                .map_err(|_| UnifiedError::bad_request(format!("Invalid domain name \"{domain}\"")))?;
            let resolver = TokioResolver::builder_with_config(
                ResolverConfig::default(),
                TokioConnectionProvider::default(),
            )
            .build();
            let lookup = resolver.lookup(name, RecordType::SOA).await.map_err(|error| {
                UnifiedError::bad_request(format!("SOA lookup failed for \"{domain}\": {error}"))
            })?;
            let mut records: Vec<Value> = Vec::new();
            for record in lookup.iter() {
                if let RData::SOA(soa) = record {
                    records.push(json!({
                        "mname": soa.mname().to_string(),
                        "rname": soa.rname().to_string(),
                        "serial": soa.serial(),
                        "refresh": soa.refresh(),
                        "retry": soa.retry(),
                        "expire": soa.expire(),
                        "minimum": soa.minimum(),
                    }));
                }
            }
            Ok(json!({ "record_type": "SOA", "domain": domain, "records": records }))
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unsupported DNS record type \"{other}\"; use A, AAAA, MX, TXT, CNAME, NS, or SOA"
        ))),
    }
}

/// Run the system `ping` binary with strictly validated arguments.
/// Returns (stdout, method). The target is validated as a hostname
/// or IP before it ever reaches the command line, and count/timeout
/// are clamped, so no shell metacharacter can be injected (no shell
/// is involved at all: arguments are passed as an argv array).
pub(crate) async fn run_ping_binary(
    target: &str,
    count: u32,
    timeout_secs: u64,
) -> UnifiedResult<(String, String)> {
    let output = timeout(
        Duration::from_secs(timeout_secs + 5),
        tokio::process::Command::new("ping")
            .arg("-c")
            .arg(count.to_string())
            .arg("-W")
            .arg(timeout_secs.to_string())
            .arg(target)
            .output(),
    )
    .await
    .map_err(|_| UnifiedError::internal("Ping timed out at the runner level"))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            UnifiedError::new(
                "The system `ping` binary is not available on this host; TCP-connect timing was used instead",
                503,
            )
        } else {
            UnifiedError::internal(format!("Cannot run the `ping` binary: {error}"))
        }
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((stdout, "system ping binary".to_string()))
}

pub(crate) fn parse_ping_output(stdout: &str) -> Value {
    let mut transmitted: Option<u32> = None;
    let mut received: Option<u32> = None;
    let mut packet_loss_pct: Option<f64> = None;
    let mut rtt_min_ms: Option<f64> = None;
    let mut rtt_avg_ms: Option<f64> = None;
    let mut rtt_max_ms: Option<f64> = None;
    let mut rtt_mdev_ms: Option<f64> = None;
    for line in stdout.lines() {
        if line.contains("packets transmitted") {
            // `4 packets transmitted, 4 received, 0% packet loss, time 3002ms`
            let parts: Vec<&str> = line.split(',').collect();
            for part in parts {
                let part = part.trim();
                if let Some(num) = part.split_whitespace().next() {
                    if part.contains("packets transmitted") {
                        transmitted = num.parse().ok();
                    } else if part.contains("received") {
                        received = num.parse().ok();
                    } else if part.contains("packet loss") {
                        packet_loss_pct = part
                            .split_whitespace()
                            .next()
                            .map(|n| n.trim_end_matches('%'))
                            .and_then(|n| n.parse().ok());
                    }
                }
            }
        }
        if line.contains("rtt min/avg/max/mdev") {
            // `rtt min/avg/max/mdev = 12.1/13.4/15.2/1.1 ms`
            if let Some(values) = line.split('=').nth(1) {
                let nums: Vec<f64> = values
                    .trim()
                    .trim_end_matches(" ms")
                    .split('/')
                    .filter_map(|n| n.trim().parse().ok())
                    .collect();
                if nums.len() == 4 {
                    rtt_min_ms = Some(nums[0]);
                    rtt_avg_ms = Some(nums[1]);
                    rtt_max_ms = Some(nums[2]);
                    rtt_mdev_ms = Some(nums[3]);
                }
            }
        }
    }
    json!({
        "transmitted": transmitted,
        "received": received,
        "packet_loss_pct": packet_loss_pct,
        "rtt_min_ms": rtt_min_ms,
        "rtt_avg_ms": rtt_avg_ms,
        "rtt_max_ms": rtt_max_ms,
        "rtt_mdev_ms": rtt_mdev_ms,
        "raw": stdout.chars().take(4096).collect::<String>(),
    })
}

/// TCP-connect timing fallback for ping when the system binary is
/// absent: measures connect latency to ports 80 and 443 and reports
/// the fastest successful one. This is NOT ICMP; the result says so.
pub(crate) async fn ping_tcp_fallback(addrs: &[IpAddr], timeout_secs: u64) -> Value {
    let mut best_ms: Option<f64> = None;
    let mut tried = 0u32;
    let mut succeeded = 0u32;
    for ip in addrs {
        for port in [80u16, 443u16] {
            tried += 1;
            let addr = SocketAddr::new(*ip, port);
            let started = Instant::now();
            match timeout(
                Duration::from_secs(timeout_secs),
                TokioTcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(_stream)) => {
                    succeeded += 1;
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    best_ms = Some(best_ms.map_or(ms, |best: f64| best.min(ms)));
                }
                _ => {}
            }
        }
    }
    let loss = if tried == 0 {
        None
    } else {
        Some(((tried - succeeded) as f64 / tried as f64) * 100.0)
    };
    json!({
        "transmitted": tried,
        "received": succeeded,
        "packet_loss_pct": loss,
        "rtt_min_ms": best_ms,
        "rtt_avg_ms": best_ms,
        "rtt_max_ms": best_ms,
        "rtt_mdev_ms": null,
        "raw": "",
    })
}

pub(crate) async fn tool_ping(
    target: &str,
    count: u32,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let count = count.clamp(1, MAX_PING_COUNT);
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    let addrs = resolve_and_guard(&target, allow_private).await?;
    let mut notes: Vec<String> = Vec::new();
    // Pass the guarded resolved address (IP literal) to the ping
    // binary, not the hostname: the binary must not re-resolve the
    // name to a different address after the guard ran.
    let pinned = pinned_target_literal(&addrs);
    notes.push(format!(
        "Ping ran against the guarded address {pinned}; the hostname was not re-resolved by the ping binary."
    ));
    let (result, method, outcome) = match run_ping_binary(&pinned, count, timeout_secs).await {
        Ok((stdout, method)) => {
            let parsed = parse_ping_output(&stdout);
            let loss = parsed
                .get("packet_loss_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(100.0);
            let outcome = if loss < 100.0 { "success" } else { "failed" };
            (parsed, method, outcome)
        }
        Err(error) if error.code == 503 => {
            notes.push(
                "The system `ping` binary is not available on this host; result is TCP-connect timing to ports 80/443, not ICMP echo."
                    .to_string(),
            );
            let parsed = ping_tcp_fallback(&addrs, timeout_secs).await;
            let loss = parsed
                .get("packet_loss_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(100.0);
            let outcome = if loss < 100.0 { "success" } else { "failed" };
            (
                parsed,
                "tcp connect timing fallback (no ping binary)".to_string(),
                outcome,
            )
        }
        Err(error) => return Err(error),
    };
    Ok(ToolResult {
        tool: "ping".to_string(),
        target,
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some(method),
        result,
        notes,
    })
}

pub(crate) fn parse_traceroute_output(stdout: &str) -> Vec<Value> {
    let mut hops: Vec<Value> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("traceroute") {
            continue;
        }
        // ` 1  192.168.1.1 (192.168.1.1)  1.234 ms  1.102 ms`
        // or ` 2  * * *` for a silent hop.
        let mut parts = line.split_whitespace();
        let Some(hop_no) = parts.next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let mut host: Option<String> = None;
        let mut ip: Option<String> = None;
        let mut rtts: Vec<f64> = Vec::new();
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (index, token) in tokens.iter().enumerate() {
            if *token == "*" {
                continue;
            }
            if token.starts_with('(') && token.ends_with(')') {
                ip = Some(token.trim_matches(|c| c == '(' || c == ')').to_string());
                if index > 1 {
                    host = Some(tokens[index - 1].to_string());
                }
            } else if *token == "ms" {
                if index > 0 {
                    if let Ok(ms) = tokens[index - 1].parse::<f64>() {
                        rtts.push(ms);
                    }
                }
            } else if host.is_none()
                && !token.parse::<f64>().is_ok()
                && *token != "ms"
                && index > 0
            {
                // A bare hostname or IP token with no parentheses.
                if token.parse::<IpAddr>().is_ok() {
                    ip = Some(token.to_string());
                } else {
                    host = Some(token.to_string());
                }
            }
        }
        let _ = parts;
        hops.push(json!({
            "hop": hop_no,
            "host": host,
            "ip": ip,
            "rtt_ms": rtts,
        }));
    }
    hops
}

/// The address literal passed to the `ping` / `traceroute` binaries:
/// the first address that passed the SSRF guard. Passing the
/// hostname would let the binary re-resolve it to a different
/// address after the guard ran (DNS-rebinding TOCTOU).
pub(crate) fn pinned_target_literal(addrs: &[IpAddr]) -> String {
    addrs[0].to_string()
}

/// Wall-clock bound for the traceroute runner: the caller's total
/// timeout plus a fixed margin for process startup and teardown.
/// Deliberately NOT multiplied by the hop count: the old
/// `timeout_secs * max_hops + 10` bound let one request hold a
/// worker for up to 910 s (30 s x 30 hops + 10 s).
pub(crate) fn traceroute_runner_timeout(timeout_secs: u64) -> Duration {
    Duration::from_secs(timeout_secs.saturating_add(10))
}

pub(crate) async fn tool_traceroute(
    target: &str,
    max_hops: u32,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let max_hops = max_hops.clamp(1, MAX_TRACEROUTE_HOPS);
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    let addrs = resolve_and_guard(&target, allow_private).await?;
    // Pass the guarded resolved address (IP literal) to the
    // traceroute binary, not the hostname: the binary must not
    // re-resolve the name to a different address after the guard ran.
    let pinned = pinned_target_literal(&addrs);
    let output = timeout(
        traceroute_runner_timeout(timeout_secs),
        tokio::process::Command::new("traceroute")
            .arg("-n")
            .arg("-q")
            .arg("1")
            .arg("-m")
            .arg(max_hops.to_string())
            .arg("-w")
            .arg(timeout_secs.to_string())
            .arg(&pinned)
            .output(),
    )
    .await
    .map_err(|_| UnifiedError::internal("Traceroute timed out at the runner level"))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            UnifiedError::new(
                "The system `traceroute` binary is not available on this host; hop resolution depends on the host tool and has no faithful fallback",
                503,
            )
        } else {
            UnifiedError::internal(format!("Cannot run the `traceroute` binary: {error}"))
        }
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let hops = parse_traceroute_output(&stdout);
    let outcome = if hops.is_empty() { "failed" } else { "success" };
    Ok(ToolResult {
        tool: "traceroute".to_string(),
        target,
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("system traceroute binary".to_string()),
        result: json!({ "hops": hops }),
        notes: vec![
            "Hop hostnames are resolved by the host `traceroute` tool; a hop with no hostname or IP timed out or does not answer.".to_string(),
            "The traceroute target was the guarded resolved address; the hostname was not re-resolved by the traceroute binary.".to_string(),
        ],
    })
}

pub(crate) async fn tool_port_check(
    target: &str,
    port: u16,
    grab_banner: bool,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let addrs = resolve_and_guard(&target, allow_private).await?;
    let connect_timeout = clamp_connect_timeout(Some(timeout_secs));
    let mut open = false;
    let mut latency_ms: Option<f64> = None;
    let mut banner: Option<String> = None;
    let mut last_error: Option<String> = None;
    for ip in &addrs {
        let addr = SocketAddr::new(*ip, port);
        let attempt = Instant::now();
        match timeout(connect_timeout, TokioTcpStream::connect(addr)).await {
            Ok(Ok(mut stream)) => {
                open = true;
                latency_ms = Some(attempt.elapsed().as_secs_f64() * 1000.0);
                if grab_banner {
                    let mut buf = vec![0u8; MAX_BANNER_BYTES];
                    match timeout(Duration::from_secs(2), stream.read(&mut buf)).await {
                        Ok(Ok(read)) if read > 0 => {
                            banner = Some(
                                String::from_utf8_lossy(&buf[..read])
                                    .chars()
                                    .take(MAX_BANNER_BYTES)
                                    .collect(),
                            );
                        }
                        _ => {}
                    }
                }
                break;
            }
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("connect timed out".to_string()),
        }
    }
    let outcome = if open { "success" } else { "failed" };
    Ok(ToolResult {
        tool: "port_check".to_string(),
        target: format!("{target}:{port}"),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("tcp connect".to_string()),
        result: json!({
            "host": target,
            "port": port,
            "open": open,
            "latency_ms": latency_ms,
            "banner": banner,
            "error": last_error,
            "resolved": addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
        }),
        notes: if grab_banner {
            vec!["Banner grab only reads up to 1 KB and never sends a payload.".to_string()]
        } else {
            Vec::new()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ping_output_reads_packet_counts_and_rtt() {
        let stdout = "PING example.com (93.184.216.34) 56(84) bytes of data.
            64 bytes from 93.184.216.34: icmp_seq=1 ttl=56 time=12.3 ms

            --- example.com ping statistics ---
            4 packets transmitted, 4 received, 0% packet loss, time 3002ms
            rtt min/avg/max/mdev = 12.100/13.400/15.200/1.100 ms
";
        let parsed = parse_ping_output(stdout);
        assert_eq!(parsed["transmitted"], json!(4));
        assert_eq!(parsed["received"], json!(4));
        assert_eq!(parsed["packet_loss_pct"], json!(0.0));
        assert_eq!(parsed["rtt_avg_ms"], json!(13.4));
        assert_eq!(parsed["rtt_min_ms"], json!(12.1));
    }

    #[test]
    fn parse_ping_output_handles_total_loss() {
        let stdout = "4 packets transmitted, 0 received, 100% packet loss, time 3002ms
";
        let parsed = parse_ping_output(stdout);
        assert_eq!(parsed["transmitted"], json!(4));
        assert_eq!(parsed["received"], json!(0));
        assert_eq!(parsed["packet_loss_pct"], json!(100.0));
        assert!(parsed["rtt_avg_ms"].is_null());
    }

    #[test]
    fn pinned_target_literal_uses_the_guarded_address() {
        let addrs: Vec<IpAddr> = vec!["93.184.216.34".parse().unwrap()];
        assert_eq!(pinned_target_literal(&addrs), "93.184.216.34");
        let v6: Vec<IpAddr> = vec!["2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()];
        assert_eq!(
            pinned_target_literal(&v6),
            "2606:2800:220:1:248:1893:25c8:1946"
        );
    }

    #[test]
    fn traceroute_runner_timeout_is_bounded_not_multiplied() {
        // Worst case under the old formula: 30 s x 30 hops + 10 s = 910 s.
        assert_eq!(
            traceroute_runner_timeout(30),
            Duration::from_secs(40)
        );
        assert_eq!(traceroute_runner_timeout(10), Duration::from_secs(20));
        assert_eq!(traceroute_runner_timeout(1), Duration::from_secs(11));
    }

    #[test]
    fn parse_traceroute_output_reads_hops() {
        let stdout = "traceroute to example.com (93.184.216.34), 30 hops max
            1  192.168.1.1 (192.168.1.1)  1.234 ms  1.102 ms
            2  * * *
";
        let hops = parse_traceroute_output(stdout);
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0]["hop"], json!(1));
        assert_eq!(hops[0]["ip"], json!("192.168.1.1"));
        assert_eq!(hops[0]["rtt_ms"], json!([1.234, 1.102]));
        assert_eq!(hops[1]["hop"], json!(2));
        assert!(hops[1]["host"].is_null());
        assert!(hops[1]["ip"].is_null());
        assert_eq!(hops[1]["rtt_ms"], json!([]));
    }
}
