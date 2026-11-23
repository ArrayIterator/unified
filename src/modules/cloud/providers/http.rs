// providers module -- Shared HTTP, signing, and payload helpers for provider adapters.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, RETRY_AFTER};
use reqwest::StatusCode;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;

use super::*;

// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------

/// The interface every provider adapter implements. Create / scale /
/// delete / start / stop / suspend / unsuspend / renew / change
/// password / usage have default implementations that fail with a
/// clear "not supported" error, so a read-only adapter only implements
/// the three required methods plus `capabilities`.
///
/// The surface follows the WHMCS provisioning-module standard
/// (CreateAccount, SuspendAccount, UnsuspendAccount, TerminateAccount,
/// Renew, ChangePassword, ChangePackage, MetaData, UsageUpdate /
/// MetricProvider): `create_server` is CreateAccount, `delete_server`
/// is TerminateAccount, `scale_server` is ChangePackage. Each adapter
/// translates its platform's API into this uniform interface; the
/// trait never reimplements a platform.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities_for(self.kind())
    }

    /// Provider metadata (WHMCS MetaData: DisplayName, APIVersion,
    /// RequiresServer). The display name comes from the registry
    /// definition; adapters override the API version.
    fn metadata(&self) -> ProviderMetadata {
        let display_name = self
            .kind()
            .definition()
            .map(|definition| definition.display_name.clone())
            .unwrap_or_else(|| self.kind().as_str().to_string());
        ProviderMetadata {
            display_name,
            api_version: "unknown".to_string(),
            requires_server: true,
        }
    }

    /// Verify the credentials against the real provider API.
    async fn test_connection(&self) -> UnifiedResult<ProviderInfo>;

    /// List servers / containers / clusters visible to the credentials.
    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>>;

    /// Status of one server / container / cluster by provider id.
    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo>;

    /// Create a server. Guarded by `supports_create`.
    async fn create_server(&self, _request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        Err(unsupported(
            self.kind(),
            "create",
            "supports_create",
        ))
    }

    /// Scale a server (change type / flavor / resources). Guarded by
    /// `supports_scale`.
    async fn scale_server(&self, _id: &str, _request: &ScaleServerRequest) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "scale", "supports_scale"))
    }

    /// Delete a server / container. Guarded by `supports_delete`.
    async fn delete_server(&self, _id: &str) -> UnifiedResult<()> {
        Err(unsupported(self.kind(), "delete", "supports_delete"))
    }

    /// Start a container / VPS. Guarded by `supports_start_stop`.
    async fn start_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(
            self.kind(),
            "start",
            "supports_start_stop",
        ))
    }

    /// Stop a container / VPS. Guarded by `supports_start_stop`.
    async fn stop_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "stop", "supports_start_stop"))
    }

    /// Suspend a server / VPS / container (WHMCS SuspendAccount).
    /// Guarded by `supports_suspend`.
    async fn suspend_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "suspend", "supports_suspend"))
    }

    /// Unsuspend a server / VPS / container (WHMCS UnsuspendAccount).
    /// Guarded by `supports_suspend`.
    async fn unsuspend_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "unsuspend", "supports_suspend"))
    }

    /// Renew a server (WHMCS Renew). Renewal is a billing-layer
    /// concept — platforms bill automatically — so no built-in adapter
    /// implements it yet. Guarded by `supports_renew`.
    async fn renew_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "renew", "supports_renew"))
    }

    /// Change a server password (WHMCS ChangePassword). Guarded by
    /// `supports_change_password`.
    async fn change_password(
        &self,
        _id: &str,
        _request: &ChangePasswordRequest,
    ) -> UnifiedResult<ServerInfo> {
        Err(unsupported(
            self.kind(),
            "change password",
            "supports_change_password",
        ))
    }

    /// Usage metrics for one server / container (WHMCS UsageUpdate /
    /// MetricProvider). Guarded by `supports_usage`.
    async fn usage_metrics(&self, _id: &str) -> UnifiedResult<UsageMetrics> {
        Err(unsupported(self.kind(), "usage metrics", "supports_usage"))
    }
}

pub(crate) fn unsupported(kind: ProviderKind, action: &str, capability: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Provider \"{}\" does not support {action} (capability {capability} is false)",
        kind.as_str()
    ))
}

/// Build provider metadata with an adapter-declared API version; the
/// display name comes from the registry definition (same as the trait
/// default in `CloudProvider::metadata`).
pub(crate) fn metadata_with_version(kind: ProviderKind, api_version: &str) -> ProviderMetadata {
    let display_name = kind
        .definition()
        .map(|definition| definition.display_name.clone())
        .unwrap_or_else(|| kind.as_str().to_string());
    ProviderMetadata {
        display_name,
        api_version: api_version.to_string(),
        requires_server: true,
    }
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

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

pub(crate) type HmacSha256 = Hmac<Sha256>;

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

pub(crate) fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Refuse a URL whose host is not on the provider's allow-list. Called
/// before every request to a fixed-host provider.
pub(crate) fn check_allowlist(url: &Url, kind: ProviderKind) -> UnifiedResult<()> {
    if !kind.has_fixed_hosts() {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("");
    if !kind.allowed_hosts().contains(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to non-allow-listed host \"{host}\" for provider \"{}\"",
            kind.as_str()
        )));
    }
    Ok(())
}

/// Parse a `Retry-After` header: seconds, or an IMF-fixdate HTTP
/// date (`Sun, 06 Nov 1994 08:49:37 GMT`), capped so a misbehaving
/// provider cannot stall a caller indefinitely. A date in the past
/// means retry immediately (`Duration::ZERO`).
pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after_value(value, Utc::now())
}

/// Parse one `Retry-After` header value against `now`. Split out so
/// the HTTP-date form is unit-testable without a response.
pub(crate) fn parse_retry_after_value(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    let trimmed = value.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER));
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT").ok()?;
    let delay_secs = naive
        .and_utc()
        .signed_duration_since(now)
        .num_seconds()
        .max(0);
    Some(Duration::from_secs(delay_secs as u64).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP response to an error. A 429 keeps its status and
/// carries the `Retry-After` delay in metadata; 401/403/404 map to the
/// matching `UnifiedError`; other failures stay generic so provider
/// response bodies (which may contain account data) are never leaked
/// into the client-visible message.
pub(crate) fn http_status_error(
    status: StatusCode,
    provider: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Provider \"{provider}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("provider", Value::String(provider.to_string()));
            if let Some(delay) = retry_after {
                error = error.with_metadata(
                    "retry_after_secs",
                    Value::from(delay.as_secs()),
                );
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Provider \"{provider}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => {
            UnifiedError::bad_request(message).with_metadata("provider", Value::String(provider.to_string()))
        }
        _ => UnifiedError::internal(message).with_metadata("provider", Value::String(provider.to_string())),
    }
}

/// Send a request and return the parsed JSON body, or a mapped error.
/// 429 responses are returned as errors (with `retry_after_secs` in
/// metadata) instead of being retried here, so the caller decides.

pub(crate) async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and parse the JSON body, tolerating an empty body
/// (some write calls answer 200/204 with no content). Returns
/// `Value::Null` for an empty body.
pub(crate) async fn send_json_or_empty(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    let text = response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and return the raw text body (for the XML query APIs
/// used by AWS EC2 and Redshift), or a mapped error.

pub(crate) async fn send_text(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<String> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and discard the body (for 202 / 204 answers such
/// as deletes and power actions), or a mapped error.

pub(crate) async fn send_no_content(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<()> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    Ok(())
}


pub(crate) fn bearer_header(token: &str) -> UnifiedResult<HeaderMap> {
    let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

/// Normalize a provider-native status string onto the shared set.
pub(crate) fn normalize_status(raw: &str) -> &'static str {
    match raw.to_ascii_lowercase().as_str() {
        "running" | "active" | "on" | "up" | "available" | "ok" => "running",
        "stopped" | "off" | "down" | "terminated" | "deleted" | "paused" | "exited" | "dead"
        | "shutoff" | "suspended" | "shelved_offloaded" | "halted" | "archive" => "stopped",
        "starting" | "pending" | "provisioning" | "initializing" | "restarting"
        | "new" | "build" | "rebuilding" | "rebooting" | "migrating" => "starting",
        "stopping" | "shutting-down" | "deleting" => "stopping",
        "creating" => "creating",
        "error" | "failed" | "crashed" | "unhealthy" => "error",
        _ => "unknown",
    }
}

pub(crate) fn json_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(crate) fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

pub(crate) fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

/// Percent-encode one URL path segment (RFC 3986 unreserved set is
/// kept as-is; everything else is encoded). Provider ids are normally
/// plain alphanumerics, but ids are caller-influenced input and must
/// never be interpolated raw into a URL path.
pub(crate) fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Minimal XML extraction (AWS EC2 / Redshift query APIs).
//
// The EC2 and Redshift query APIs answer XML, and this codebase has no
// XML dependency. This is a deliberately minimal extractor for the
// known response shapes (`DescribeInstances`, `DescribeClusters`):
// it finds `<tag>value</tag>` text and splits repeated `<item>` blocks.
// It is not a general XML parser and must not be used for anything else.
// ---------------------------------------------------------------------------

/// Text inside the first `<tag>...</tag>` at or after `from`, if any.
pub(crate) fn xml_tag_at(text: &str, from: usize, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text[from..].find(&open)? + from + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_string())
}

/// Split `text` into the `<tag>...</tag>` blocks it contains.
pub(crate) fn xml_blocks(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find(&open) {
        let content_start = from + start + open.len();
        let Some(end) = text[content_start..].find(&close) else {
            break;
        };
        blocks.push(text[content_start..content_start + end].to_string());
        from = content_start + end + close.len();
    }
    blocks
}

// ---------------------------------------------------------------------------
// AWS SigV4 signing (EC2, Redshift).
//
// Canonical request, credential scope, and the four-step HMAC key
// derivation, following the AWS Signature Version 4 scheme. Only the
// `host`, `x-amz-date`, and `content-type` headers are signed, and the
// payload hash is always the SHA-256 of the (possibly empty) body.
// ---------------------------------------------------------------------------

pub(crate) struct SigV4Params {
    pub(crate) access_key_id: String,
    pub(crate) secret_access_key: String,
    pub(crate) region: String,
    pub(crate) service: String,
}

/// Build the `Authorization` header value and the `X-Amz-Date` value
/// for one request. `headers_to_sign` are `name: value` pairs, already
/// lower-case sorted; the caller must send exactly those headers.
pub(crate) fn sigv4_authorization(
    params: &SigV4Params,
    method: &str,
    url: &Url,
    headers_to_sign: &[(&str, String)],
    payload: &[u8],
    now: DateTime<Utc>,
) -> (String, String) {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let mut signed: Vec<(&str, String)> = headers_to_sign.to_vec();
    signed.sort_by(|a, b| a.0.cmp(b.0));
    let signed_headers: Vec<&str> = signed.iter().map(|(name, _)| *name).collect();
    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let canonical_query = url.query().unwrap_or("");
    let canonical_request = format!(
        "{method}\n{}\n{canonical_query}\n{canonical_headers}{}\n{}",
        url.path(),
        signed_headers.join(";"),
        sha256_hex(payload),
    );
    let scope = format!("{date_stamp}/{}/{}/aws4_request", params.region, params.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(format!("AWS4{}", params.secret_access_key).as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, params.region.as_bytes());
    let k_service = hmac_sha256(&k_region, params.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
        params.access_key_id,
        signed_headers.join(";"),
    );
    (authorization, amz_date)
}

// ---------------------------------------------------------------------------
// Tencent Cloud TC3-HMAC-SHA256 signing (CVM).
//
// Same HMAC-chain shape as SigV4 with Tencent's own prefixes: the
// signing key starts from `"TC3" + secret key`, the scope terminator
// is `tc3_request`, and the algorithm name is `TC3-HMAC-SHA256`.
// ---------------------------------------------------------------------------


pub(crate) struct Tc3Params {
    pub(crate) secret_id: String,
    pub(crate) secret_key: String,
    pub(crate) region: String,
    pub(crate) service: String,
    pub(crate) action: String,
    pub(crate) version: String,
}

pub(crate) fn tc3_headers(
    params: &Tc3Params,
    host: &str,
    payload: &[u8],
    now: DateTime<Utc>,
) -> UnifiedResult<HeaderMap> {
    let timestamp = now.timestamp();
    let date = now.format("%Y%m%d").to_string();
    let canonical_headers = format!(
        "content-type:application/json\nhost:{host}\nx-tc-action:{}\n",
        params.action.to_ascii_lowercase()
    );
    let signed_headers = "content-type;host;x-tc-action";
    let canonical_request = format!(
        "POST\n/\n\n{canonical_headers}{signed_headers}\n{}",
        sha256_hex(payload),
    );
    let scope = format!("{date}/{}/tc3_request", params.service);
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(format!("TC3{}", params.secret_key).as_bytes(), date.as_bytes());
    let k_service = hmac_sha256(&k_date, params.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"tc3_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "TC3-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        params.secret_id,
    );
    let mut headers = HeaderMap::new();
    let set = |headers: &mut HeaderMap, name: &str, value: &str| -> UnifiedResult<()> {
        let header_value = HeaderValue::from_str(value).map_err(|_| {
            UnifiedError::bad_request("Computed signature contains characters invalid in an HTTP header")
        })?;
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                UnifiedError::internal("Invalid header name while signing a Tencent request")
            })?,
            header_value,
        );
        Ok(())
    };
    set(&mut headers, "authorization", &authorization)?;
    set(&mut headers, "content-type", "application/json")?;
    set(&mut headers, "host", host)?;
    set(&mut headers, "x-tc-action", &params.action)?;
    set(&mut headers, "x-tc-timestamp", &timestamp.to_string())?;
    set(&mut headers, "x-tc-version", &params.version)?;
    set(&mut headers, "x-tc-region", &params.region)?;
    Ok(headers)
}

// ---------------------------------------------------------------------------
// Huawei Cloud SDK-HMAC-SHA256 signing (ECS).
//
// Same key derivation shape as SigV4 with the `"SDK"` prefix and the
// `sdk_request` scope terminator. The signed headers are `host` and
// `x-sdk-date`.
// ---------------------------------------------------------------------------


pub(crate) fn huawei_authorization(
    access_key: &str,
    secret_key: &str,
    region: &str,
    service: &str,
    method: &str,
    url: &Url,
    payload: &[u8],
    now: DateTime<Utc>,
) -> UnifiedResult<(String, String)> {
    let sdk_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let host = url.host_str().unwrap_or("").to_string();
    let canonical_headers = format!("host:{host}\nx-sdk-date:{sdk_date}\n");
    let canonical_request = format!(
        "{method}\n{}\n{}\n{canonical_headers}\nhost;x-sdk-date\n{}",
        url.path(),
        url.query().unwrap_or(""),
        sha256_hex(payload),
    );
    let scope = format!("{date_stamp}/{region}/{service}/sdk_request");
    let string_to_sign = format!(
        "SDK-HMAC-SHA256\n{sdk_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(format!("SDK{secret_key}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"sdk_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "SDK-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders=host;x-sdk-date, Signature={signature}"
    );
    Ok((authorization, sdk_date))
}

// ---------------------------------------------------------------------------
// OVH signature.
//
// `"$1$" + SHA1(app_secret + "+" + consumer_key + "+" + method + "+" +
// url + "+" + body + "+" + timestamp)`, sent as `X-Ovh-Signature`
// alongside `X-Ovh-Application`, `X-Ovh-Consumer`, and
// `X-Ovh-Timestamp`.
// ---------------------------------------------------------------------------


pub(crate) fn ovh_signature(
    application_secret: &str,
    consumer_key: &str,
    method: &str,
    url: &str,
    body: &str,
    timestamp: i64,
) -> String {
    let to_sign = format!("{application_secret}+{consumer_key}+{method}+{url}+{body}+{timestamp}");
    format!("$1${}", hex_encode(&sha1::Sha1::digest(to_sign.as_bytes())))
}

// ---------------------------------------------------------------------------
// Hetzner Cloud adapter.
// ---------------------------------------------------------------------------

/// Hetzner Cloud API (`api.hetzner.cloud`), token auth.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_formats_lowercase_pairs() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0u8, 255, 16]), "00ff10");
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn normalize_status_maps_families() {
        assert_eq!(normalize_status("running"), "running");
        assert_eq!(normalize_status("RUNNING"), "running");
        assert_eq!(normalize_status("active"), "running");
        assert_eq!(normalize_status("stopped"), "stopped");
        assert_eq!(normalize_status("terminated"), "stopped");
        assert_eq!(normalize_status("pending"), "starting");
        assert_eq!(normalize_status("stopping"), "stopping");
        assert_eq!(normalize_status("error"), "error");
        assert_eq!(normalize_status("crashed"), "error");
        assert_eq!(normalize_status("weird"), "unknown");
    }

    #[test]
    fn normalize_status_creating_returns_creating() {
        assert_eq!(normalize_status("creating"), "creating");
        assert_eq!(normalize_status("CREATING"), "creating");
        // The other "starting" family members still map to starting.
        assert_eq!(normalize_status("pending"), "starting");
        assert_eq!(normalize_status("provisioning"), "starting");
    }

    #[test]
    fn parse_retry_after_value_accepts_seconds() {
        let now = Utc::now();
        assert_eq!(
            parse_retry_after_value("5", now),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_retry_after_value(" 30 ", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after_value("0", now),
            Some(Duration::from_secs(0))
        );
    }

    #[test]
    fn parse_retry_after_value_caps_seconds() {
        let now = Utc::now();
        assert_eq!(
            parse_retry_after_value("9999", now),
            Some(MAX_RETRY_AFTER)
        );
    }

    #[test]
    fn parse_retry_after_value_accepts_http_date() {
        // 2026-11-22 is a Sunday; the fixed `now` keeps the delta exact.
        let now = DateTime::parse_from_rfc3339("2026-11-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_retry_after_value("Sun, 22 Nov 2026 00:01:00 GMT", now),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn parse_retry_after_value_caps_http_date_and_clamps_past_dates() {
        let now = DateTime::parse_from_rfc3339("2026-11-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_retry_after_value("Sun, 22 Nov 2026 02:00:00 GMT", now),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            parse_retry_after_value("Sat, 21 Nov 2026 23:59:00 GMT", now),
            Some(Duration::from_secs(0))
        );
    }

    #[test]
    fn parse_retry_after_value_rejects_garbage() {
        let now = Utc::now();
        assert_eq!(parse_retry_after_value("not-a-date", now), None);
        assert_eq!(parse_retry_after_value("", now), None);
        assert_eq!(parse_retry_after_value("-5", now), None);
    }

    #[test]
    fn encode_path_segment_keeps_unreserved() {
        assert_eq!(encode_path_segment("abc-._~"), "abc-._~");
        assert_eq!(encode_path_segment(""), "");
    }

    #[test]
    fn encode_path_segment_percent_encodes_reserved() {
        assert_eq!(encode_path_segment("a b/c?d"), "a%20b%2Fc%3Fd");
    }

    #[test]
    fn xml_tag_at_finds_and_trims() {
        assert_eq!(
            xml_tag_at("<a>  hello </a>", 0, "a"),
            Some("hello".to_string())
        );
        assert_eq!(xml_tag_at("<a>one</a><a>two</a>", 9, "a"), Some("two".to_string()));
        assert_eq!(xml_tag_at("<a>one</a>", 0, "b"), None);
    }

    #[test]
    fn xml_blocks_collects_repeated_tags() {
        assert_eq!(
            xml_blocks("<item>a</item><item>b</item>", "item"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(xml_blocks("<item>a", "item").is_empty());
        assert!(xml_blocks("nothing here", "item").is_empty());
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
