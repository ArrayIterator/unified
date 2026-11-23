//! panels module - Adapter trait, HTTP helpers and JSON value helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, RETRY_AFTER};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use url::Url;

use super::constants::COLLECTIONS;
use super::constants::DEFAULT_CONNECT_TIMEOUT_SECS;
use super::constants::DEFAULT_TIMEOUT_SECS;
use super::constants::MAX_REDIRECTS;
use super::constants::MAX_RETRY_AFTER;
use super::constants::USER_AGENT;
use super::handlers_connections::test_connection;
use super::types::PanelCapabilities;
use super::types::redirect_target_allowed;
use super::types::resolved_addresses_are_safe;
use super::types::PanelClient;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelInvoice;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelServerAccount;
use super::types::PanelService;
use super::types::PanelSyncCounts;
use super::types::capabilities_for;

// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------

/// The interface every panel adapter implements. List methods have
/// default implementations that fail with a clear "not supported"
/// error, so an adapter only implements the collections its panel
/// actually exposes; `capabilities()` is the contract callers check
/// first. `sync_all` has a default implementation that fetches every
/// supported collection and aggregates counts and errors.
///
/// Completeness contract: a list method must return the whole
/// collection or fail. It must never return a truncated page as if
/// it were the full collection, because sync treats a stored record
/// absent from the fetched set as deleted on the remote side (see
/// `resolve_conflict` remote-wins). An adapter whose panel paginates
/// must walk every page, and must fail instead of truncating when
/// the collection is too large to fetch completely.
#[async_trait]
pub trait PanelAdapter: Send + Sync {
    fn kind(&self) -> PanelKind;

    fn capabilities(&self) -> PanelCapabilities {
        capabilities_for(self.kind())
    }

    /// Verify the credentials against the real panel API.
    async fn test_connection(&self) -> UnifiedResult<PanelInfo>;

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        Err(unsupported(self.kind(), "clients"))
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        Err(unsupported(self.kind(), "products"))
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        Err(unsupported(self.kind(), "services"))
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        Err(unsupported(self.kind(), "domains"))
    }

    async fn list_invoices(&self) -> UnifiedResult<Vec<PanelInvoice>> {
        Err(unsupported(self.kind(), "invoices"))
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        Err(unsupported(self.kind(), "server_accounts"))
    }

    /// Fetch every supported collection and return per-collection
    /// counts plus per-collection errors. A failed collection does
    /// not abort the others.
    async fn sync_all(&self) -> UnifiedResult<PanelSyncCounts> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut errors: HashMap<String, String> = HashMap::new();
        let capabilities = self.capabilities();
        for collection in COLLECTIONS {
            if !capabilities.supports(collection) {
                continue;
            }
            let result: UnifiedResult<usize> = match collection {
                "clients" => self.list_clients().await.map(|items| items.len()),
                "products" => self.list_products().await.map(|items| items.len()),
                "services" => self.list_services().await.map(|items| items.len()),
                "domains" => self.list_domains().await.map(|items| items.len()),
                "invoices" => self.list_invoices().await.map(|items| items.len()),
                "server_accounts" => self.list_server_accounts().await.map(|items| items.len()),
                _ => Ok(0),
            };
            match result {
                Ok(count) => {
                    counts.insert(collection.to_string(), count);
                }
                Err(error) => {
                    counts.insert(collection.to_string(), 0);
                    errors.insert(collection.to_string(), error.message);
                }
            }
        }
        Ok(PanelSyncCounts {
            counts,
            errors,
            synced_at: Utc::now(),
        })
    }
}

pub(crate) fn unsupported(kind: PanelKind, collection: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Panel \"{}\" does not support collection \"{collection}\"",
        kind.as_str()
    ))
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 hex of the canonical JSON rendering of a payload. Used as
/// the stored record hash for change detection.
pub fn payload_hash(payload: &Value) -> String {
    hex_encode(&Sha256::digest(payload.to_string().as_bytes()))
}

/// Build the shared panel HTTP client for one connection base URL.
/// Redirects are NOT followed blindly (the reqwest default follows
/// up to 10): a redirect is followed only when its target passes
/// the same checks as the configured endpoint — same host, http(s),
/// no embedded credentials, and no private / loopback / link-local
/// address unless the connection opted into a private endpoint.
/// That keeps a panel 302 to an internal host from bypassing the
/// host allow-list, and keeps the panel credentials (sent only to
/// the configured host) off any redirected host.
pub(crate) fn build_client(base: &Url, allow_private: bool) -> UnifiedResult<reqwest::Client> {
    let allowed_host = base.host_str().unwrap_or("").to_ascii_lowercase();
    let redirect = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() as u32 >= MAX_REDIRECTS {
            return attempt.stop();
        }
        match redirect_target_allowed(attempt.url(), &allowed_host, allow_private) {
            Ok(()) => attempt.follow(),
            Err(_) => attempt.stop(),
        }
    });
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .redirect(redirect)
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are ignored),
/// capped so a misbehaving panel cannot stall a caller indefinitely.
pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a panel HTTP response to an error. A 429 keeps its status and
/// carries the `Retry-After` delay in metadata; 401/403/404 map to the
/// matching `UnifiedError`; other failures stay generic so panel
/// response bodies (which may contain account data) are never leaked
/// into the client-visible message.
pub(crate) fn http_status_error(
    status: StatusCode,
    panel: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Panel \"{panel}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("panel", Value::String(panel.to_string()));
            if let Some(delay) = retry_after {
                error = error.with_metadata("retry_after_secs", Value::from(delay.as_secs()));
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Panel \"{panel}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Panel \"{panel}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Panel \"{panel}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => UnifiedError::bad_request(message)
            .with_metadata("panel", Value::String(panel.to_string())),
        _ => {
            UnifiedError::internal(message).with_metadata("panel", Value::String(panel.to_string()))
        }
    }
}

/// Send a request and return the parsed JSON body, or a mapped error.
/// A 429 is returned as an error (with `retry_after_secs` in metadata)
/// instead of being retried here, so the caller decides.
pub(crate) async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    panel: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Panel \"{panel}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(
            status,
            panel,
            parse_retry_after(&response),
        ));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Panel \"{panel}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and return the raw text body, or a mapped error.
/// Used for panels that answer plain text / URL-encoded lists.
pub(crate) async fn send_text(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    panel: &str,
) -> UnifiedResult<String> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Panel \"{panel}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(
            status,
            panel,
            parse_retry_after(&response),
        ));
    }
    response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Panel \"{panel}\" returned an unreadable response: {error}"
        ))
    })
}

/// Refuse a URL whose host differs from the connection endpoint host.
/// This is the per-connection host allow-list: a panel adapter may
/// only talk to the host the operator configured for that connection.
/// The host is also re-checked against the SSRF rules (unless the
/// connection opted into a private endpoint), so a DNS answer that
/// changed since the endpoint was validated cannot smuggle the
/// request onto the internal network.
pub(crate) fn check_panel_host(
    url: &Url,
    base: &Url,
    panel: &str,
    allow_private: bool,
) -> UnifiedResult<()> {
    let host = url.host_str().unwrap_or("");
    let allowed = base.host_str().unwrap_or("");
    if !host.eq_ignore_ascii_case(allowed) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to host \"{host}\": panel \"{panel}\" is only allowed to talk to its configured host \"{allowed}\""
        )));
    }
    if !allow_private && !resolved_addresses_are_safe(url) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to host \"{host}\": panel \"{panel}\" host resolves to a private, loopback, link-local, or reserved address"
        )));
    }
    Ok(())
}

/// Join a base URL and a relative path without letting the path
/// escape the base (no absolute URLs, no `..` segments).
pub(crate) fn join_path(base: &Url, path: &str) -> UnifiedResult<Url> {
    let trimmed = path.trim();
    if !trimmed.starts_with('/') || trimmed.contains("://") || trimmed.contains("..") {
        return Err(UnifiedError::bad_request(format!(
            "Panel path \"{trimmed}\" must be a relative path starting with \"/\""
        )));
    }
    base.join(trimmed).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Cannot join panel path \"{trimmed}\" to the base URL"
        ))
    })
}

pub(crate) fn bearer_header(token: &str) -> UnifiedResult<HeaderMap> {
    let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

pub(crate) fn api_key_header(name: &str, value: &str) -> UnifiedResult<HeaderMap> {
    let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid API key header name \"{name}\""))
    })?;
    let header_value = HeaderValue::from_str(value).map_err(|_| {
        UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(header_name, header_value);
    Ok(headers)
}

/// Normalize a panel-native status string onto the shared set.
pub(crate) fn normalize_status(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "active" | "running" | "on" | "up" | "available" | "ok" | "paid" | "enabled" => {
            "active".to_string()
        }
        "suspended" | "paused" => "suspended".to_string(),
        "cancelled" | "canceled" | "terminated" | "deleted" | "closed" | "expired" => {
            "cancelled".to_string()
        }
        "pending" | "unpaid" | "incomplete" | "draft" => "pending".to_string(),
        "fraud" | "error" | "failed" => "error".to_string(),
        "" => "unknown".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// JSON value helpers.
// ---------------------------------------------------------------------------

/// A string, number, or boolean rendered as a string.
pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

pub(crate) fn json_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(crate) fn json_string_owned(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(value_to_string)
}

pub(crate) fn json_f64(value: &Value, key: &str) -> Option<f64> {
    match value.get(key) {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Read a dotted field path (`user.id`, `profile.name`) from a JSON
/// object.
pub(crate) fn nested_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Extract a record array from a panel response. Tries, in order: the
/// value itself when it is an array; the configured `list_field`;
/// then the well-known wrapper keys `data`, `items`, and each of the
/// caller-supplied wrapper keys (for example WHMCS `clients.client`).
pub(crate) fn extract_array<'a>(
    value: &'a Value,
    list_field: Option<&str>,
    wrappers: &[&str],
) -> Vec<&'a Value> {
    if let Value::Array(items) = value {
        return items.iter().collect();
    }
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(field) = list_field {
        candidates.push(field);
    }
    candidates.extend_from_slice(&["data", "items"]);
    candidates.extend_from_slice(wrappers);
    for candidate in candidates {
        if let Some(found) = nested_value(value, candidate) {
            if let Value::Array(items) = found {
                return items.iter().collect();
            }
        }
    }
    Vec::new()
}

/// Extract a record array from a response that is a map of objects
/// keyed by username / id (DirectAdmin style): each value becomes one
/// record, with the key injected as `id` when the object has none.
pub(crate) fn extract_map_records(value: &Value, wrappers: &[&str]) -> Vec<Value> {
    let mut target: Option<&Value> = None;
    if value.is_object() {
        target = Some(value);
    }
    for wrapper in wrappers {
        if let Some(found) = nested_value(value, wrapper) {
            if found.is_object() {
                target = Some(found);
                break;
            }
        }
    }
    let Some(Value::Object(map)) = target else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(key, item)| {
            if !item.is_object() {
                return None;
            }
            let mut record = item.clone();
            if let Value::Object(object) = &mut record {
                object
                    .entry("id".to_string())
                    .or_insert_with(|| Value::String(key.clone()));
                object
                    .entry("username".to_string())
                    .or_insert_with(|| Value::String(key.clone()));
            }
            Some(record)
        })
        .collect()
}

/// First non-empty string among several keys, or `None`.
pub(crate) fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| json_string_owned(value, key))
        .filter(|text| !text.trim().is_empty())
}

/// Person / client display name from the usual name fields.
pub(crate) fn display_name(value: &Value) -> String {
    let full = first_string(value, &["name", "fullname", "display_name"]);
    if let Some(name) = full {
        return name;
    }
    let first = first_string(value, &["firstname", "first_name"]);
    let last = first_string(value, &["lastname", "last_name"]);
    match (first, last) {
        (Some(first), Some(last)) => format!("{first} {last}"),
        (Some(first), None) => first,
        (None, Some(last)) => last,
        (None, None) => "Unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::build_client;
    use super::check_panel_host;
    use super::extract_array;
    use super::hex_encode;
    use super::json_f64;
    use super::json_string;
    use super::nested_value;
    use super::normalize_status;
    use super::payload_hash;
    use super::value_to_string;
    use serde_json::{Value, json};
    use url::Url;

    #[test]
    fn normalize_status_groups() {
        assert_eq!(normalize_status("Active"), "active");
        assert_eq!(normalize_status("RUNNING"), "active");
        assert_eq!(normalize_status("suspended"), "suspended");
        assert_eq!(normalize_status("CANCELLED"), "cancelled");
        assert_eq!(normalize_status("pending"), "pending");
        assert_eq!(normalize_status("failed"), "error");
        assert_eq!(normalize_status(""), "unknown");
        assert_eq!(normalize_status("weird"), "weird");
    }

    #[test]
    fn value_to_string_renders_scalars() {
        assert_eq!(value_to_string(&json!(42)), Some("42".to_string()));
        assert_eq!(value_to_string(&json!("x")), Some("x".to_string()));
        assert_eq!(value_to_string(&json!(true)), Some("true".to_string()));
        assert_eq!(value_to_string(&json!(null)), None);
        assert_eq!(value_to_string(&json!([1])), None);
    }

    #[test]
    fn json_string_reads_field() {
        let value = json!({"a": "b"});
        assert_eq!(json_string(&value, "a"), Some("b"));
        assert_eq!(json_string(&value, "missing"), None);
    }

    #[test]
    fn json_f64_reads_numbers_and_numeric_strings() {
        assert_eq!(json_f64(&json!({"x": 2}), "x"), Some(2.0));
        assert_eq!(json_f64(&json!({"x": "1.5"}), "x"), Some(1.5));
        assert_eq!(json_f64(&json!({"x": "nope"}), "x"), None);
    }

    #[test]
    fn nested_value_follows_dotted_path() {
        let value = json!({"user": {"id": 7}});
        assert_eq!(nested_value(&value, "user.id"), Some(&Value::from(7)));
        assert_eq!(nested_value(&value, "user.missing"), None);
    }

    #[test]
    fn extract_array_unwraps_known_shapes() {
        assert_eq!(extract_array(&json!([1, 2]), None, &[]).len(), 2);
        assert_eq!(extract_array(&json!({"data": [1]}), None, &[]).len(), 1);
        assert_eq!(extract_array(&json!({"items": []}), None, &[]).len(), 0);
    }

    #[test]
    fn payload_hash_is_deterministic() {
        let first = payload_hash(&json!({"a": 1}));
        assert_eq!(first, payload_hash(&json!({"a": 1})));
        assert_ne!(first, payload_hash(&json!({"a": 2})));
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn hex_encode_matches() {
        assert_eq!(hex_encode(&[0xab, 0x01, 0xff]), "ab01ff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn check_panel_host_enforces_same_host_and_safe_address() {
        let base = Url::parse("https://93.184.216.34/").unwrap();
        let same = Url::parse("https://93.184.216.34/api").unwrap();
        assert!(check_panel_host(&same, &base, "whmcs", false).is_ok());
        let other = Url::parse("https://93.184.216.35/api").unwrap();
        assert!(check_panel_host(&other, &base, "whmcs", false).is_err());
        let internal_base = Url::parse("https://127.0.0.1/").unwrap();
        let internal = Url::parse("https://127.0.0.1/api").unwrap();
        assert!(check_panel_host(&internal, &internal_base, "whmcs", false).is_err());
        assert!(check_panel_host(&internal, &internal_base, "whmcs", true).is_ok());
    }

    #[test]
    fn build_client_accepts_public_base() {
        let base = Url::parse("https://93.184.216.34/").unwrap();
        assert!(build_client(&base, false).is_ok());
    }
    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

}
