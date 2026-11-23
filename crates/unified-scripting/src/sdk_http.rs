//! Outbound HTTP for script modules (`Unified.http.fetch`).
//!
//! Synchronous by design: the fetch runs on the scripting worker
//! thread (a plain OS thread, never a tokio worker) with a timeout,
//! so the JavaScript call blocks until the response arrives. There
//! is no `Promise`-based fetch in v1.
//!
//! SSRF rules — the same pattern the downloader and webhook
//! validators use, not a weaker copy:
//!
//! * `http`/`https` only, no embedded credentials, host required.
//! * The hostname is resolved and EVERY resolved address must be
//!   public (`unified_core::net::ip_is_global`); one private
//!   address in the answer refuses the whole fetch, because the
//!   HTTP client picks among the resolved addresses.
//! * Loopback is refused unless the admin enabled the development
//!   exception (`http_allow_loopback`).
//! * An admin-configured allowlist names exact hosts (for example
//!   an internal service) that skip the public-IP check; the
//!   scheme/credential rules still apply to them.
//! * Redirects are never followed (the redirect target is not
//!   re-validated by the client); a 3xx is returned as-is.

use reqwest::blocking::{Client, Response};
use serde_json::{json, Value};
use std::io::Read;
use std::net::ToSocketAddrs;
use std::time::Duration;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::net::ip_is_global;

/// Default fetch timeout (10 s).
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum fetch timeout a module may request (30 s).
pub const MAX_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum response body a fetch may return (5 MiB).
pub const MAX_FETCH_BODY_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum URL length.
pub const MAX_FETCH_URL_LEN: usize = 2048;

/// One fetch request, as parsed from the JavaScript options object.
#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout: Duration,
}

/// Parse the JavaScript options object (`{ method, headers, body,
/// timeout_ms }`) into a `FetchRequest`. Unknown keys are ignored.
pub fn parse_fetch_options(url: &str, options: &Value) -> UnifiedResult<FetchRequest> {
    if url.len() > MAX_FETCH_URL_LEN {
        return Err(UnifiedError::bad_request("URL is too long"));
    }
    let method = options
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    if !matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        return Err(UnifiedError::bad_request(format!(
            "Unsupported HTTP method: {method}"
        )));
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(map) = options.get("headers").and_then(Value::as_object) {
        for (name, value) in map {
            let Some(text) = value.as_str() else {
                return Err(UnifiedError::bad_request(
                    "HTTP header values must be strings",
                ));
            };
            headers.push((name.clone(), text.to_string()));
        }
    }
    let body = options
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_string);
    if body.is_some() && matches!(method.as_str(), "GET" | "HEAD") {
        return Err(UnifiedError::bad_request(
            "GET and HEAD requests must not carry a body",
        ));
    }
    let timeout_ms = options
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_FETCH_TIMEOUT.as_millis() as u64);
    let timeout = Duration::from_millis(timeout_ms).clamp(Duration::from_millis(100), MAX_FETCH_TIMEOUT);
    Ok(FetchRequest {
        method,
        url: url.to_string(),
        headers,
        body,
        timeout,
    })
}

/// Validate a fetch URL against the SSRF rules and return the
/// parsed URL. The DNS lookup happens here, on the worker thread;
/// the address set is checked again by the caller at fetch time
/// when the client resolves (a DNS change between the two checks —
/// rebinding — is a recorded open limitation, as in the
/// downloader).
pub fn validate_fetch_url(
    raw: &str,
    allowlist: &[String],
    allow_loopback: bool,
) -> UnifiedResult<url::Url> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid URL: {raw}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Fetch URL must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Fetch URL must not contain credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request("Fetch URL has no host"));
    };
    if allowlist.iter().any(|allowed| allowed.eq_ignore_ascii_case(host)) {
        return Ok(url);
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            if !allow_loopback {
                return Err(UnifiedError::bad_request(
                    "Fetch to a loopback address is not allowed",
                ));
            }
        } else if !ip_is_global(&ip) {
            return Err(UnifiedError::bad_request(
                "Fetch to a non-public address is not allowed",
            ));
        }
        return Ok(url);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return Err(UnifiedError::bad_request(format!(
            "Cannot resolve host: {host}"
        )));
    };
    let mut any = false;
    for socket in addresses {
        any = true;
        let ip = socket.ip();
        if ip.is_loopback() {
            if !allow_loopback {
                return Err(UnifiedError::bad_request(
                    "Fetch host resolves to a loopback address",
                ));
            }
        } else if !ip_is_global(&ip) {
            return Err(UnifiedError::bad_request(
                "Fetch host resolves to a non-public address",
            ));
        }
    }
    if !any {
        return Err(UnifiedError::bad_request(format!(
            "Cannot resolve host: {host}"
        )));
    }
    Ok(url)
}

/// Execute the fetch (blocking, on the worker thread) and return
/// the response as JSON: `{ ok, status, headers, body }`.
pub fn fetch_blocking(
    request: &FetchRequest,
    allowlist: &[String],
    allow_loopback: bool,
) -> UnifiedResult<Value> {
    let url = validate_fetch_url(&request.url, allowlist, allow_loopback)?;
    let client = Client::builder()
        .timeout(request.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Unified-scripting/0.1")
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let mut builder = match request.method.as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        "HEAD" => client.head(url),
        "OPTIONS" => client.request(reqwest::Method::OPTIONS, url),
        _ => unreachable!("method validated by parse_fetch_options"),
    };
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    let response: Response = builder
        .send()
        .map_err(|error| UnifiedError::internal(format!("Fetch failed: {error}")))?;
    let status = response.status().as_u16();
    let mut headers = serde_json::Map::new();
    for (name, value) in response.headers().iter() {
        let text = value.to_str().unwrap_or("").to_string();
        headers
            .entry(name.as_str().to_string())
            .and_modify(|existing| {
                if let Value::String(current) = existing {
                    *current = format!("{current}, {text}");
                }
            })
            .or_insert_with(|| Value::String(text));
    }
    let mut limited = response.take(MAX_FETCH_BODY_BYTES + 1);
    let mut body = String::new();
    limited
        .read_to_string(&mut body)
        .map_err(|error| UnifiedError::internal(format!("Cannot read response body: {error}")))?;
    if body.len() as u64 > MAX_FETCH_BODY_BYTES {
        return Err(UnifiedError::bad_request(format!(
            "Response body exceeds the {MAX_FETCH_BODY_BYTES}-byte limit"
        )));
    }
    Ok(json!({
        "ok": (200..300).contains(&status),
        "status": status,
        "headers": headers,
        "body": body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_loopback_urls_are_refused() {
        assert!(validate_fetch_url("http://127.0.0.1/x", &[], false).is_err());
        assert!(validate_fetch_url("http://10.0.0.5/x", &[], false).is_err());
        assert!(validate_fetch_url("http://192.168.1.1/x", &[], false).is_err());
        assert!(validate_fetch_url("http://172.16.0.9/x", &[], false).is_err());
        assert!(validate_fetch_url("http://[::1]/x", &[], false).is_err());
        assert!(validate_fetch_url("ftp://example.com/x", &[], false).is_err());
        assert!(validate_fetch_url("http://user:pass@example.com/x", &[], false).is_err());
        // Loopback is allowed only with the dev exception.
        assert!(validate_fetch_url("http://127.0.0.1/x", &[], true).is_ok());
        // An allowlisted host skips the address check.
        assert!(validate_fetch_url(
            "http://internal.example/x",
            &["internal.example".to_string()],
            false
        )
        .is_ok());
    }

    #[test]
    fn options_parsing_rejects_bad_methods_and_bodies() {
        let request = parse_fetch_options("https://example.com", &json!({})).expect("defaults");
        assert_eq!(request.method, "GET");
        assert_eq!(request.timeout, DEFAULT_FETCH_TIMEOUT);
        assert!(parse_fetch_options("https://example.com", &json!({"method": "TRACE"})).is_err());
        assert!(parse_fetch_options(
            "https://example.com",
            &json!({"method": "GET", "body": "x"})
        )
        .is_err());
        let request = parse_fetch_options(
            "https://example.com",
            &json!({"method": "post", "timeout_ms": 999999}),
        )
        .expect("post");
        assert_eq!(request.method, "POST");
        assert_eq!(request.timeout, MAX_FETCH_TIMEOUT);
    }
}
