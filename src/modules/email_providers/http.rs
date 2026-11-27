//! email_providers - http.

use super::{
    adapters::*, credentials::*, events::*, handlers::*, hooks::*, module::*, provider_kinds::*,
    registry::*, store::*, worker::*,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use unified_core::error::{UnifiedError, UnifiedResult};
use url::Url;
use uuid::Uuid;

// Shared HTTP helpers (mirroring the cloud providers module).
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

/// Constant-time-ish comparison for webhook signatures (avoids leaking
/// the match position through early exit).
pub(crate) fn signatures_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub(crate) fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

pub(crate) fn check_allowlist(url: &Url, kind: EmailProviderKind) -> UnifiedResult<()> {
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

pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP response to an error. A 429 keeps its status
/// and carries the `Retry-After` delay in metadata; 401/403/404 map to
/// the matching `UnifiedError`; other failures stay generic so provider
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
                error = error.with_metadata("retry_after_secs", Value::from(delay.as_secs()));
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => UnifiedError::not_found(format!(
            "Provider \"{provider}\" resource not found (HTTP 404)"
        )),
        _ if status.is_client_error() => UnifiedError::bad_request(message)
            .with_metadata("provider", Value::String(provider.to_string())),
        _ => UnifiedError::internal(message)
            .with_metadata("provider", Value::String(provider.to_string())),
    }
}

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
        return Err(http_status_error(
            status,
            provider,
            parse_retry_after(&response),
        ));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

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
        return Err(http_status_error(
            status,
            provider,
            parse_retry_after(&response),
        ));
    }
    response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Simulated accepted delivery for test mode on a provider without a
/// native sandbox. The result is always flagged `test_mode: true` so
/// it can never be mistaken for a real delivery.
pub(crate) fn simulated_result(provider: &str) -> SendResult {
    SendResult {
        provider_message_id: Some(format!("test-mode-{}", Uuid::now_v7())),
        accepted: true,
        test_mode: true,
        detail: format!(
            "Test mode: provider \"{provider}\" has no native sandbox, so the message was recorded as a simulated delivery and was not sent"
        ),
    }
}

pub(crate) fn validate_email_address(input: &str) -> UnifiedResult<()> {
    let address = input.trim();
    // The offending input is never echoed back in the error.
    let invalid = || UnifiedError::bad_request("Invalid email address");
    if address.is_empty() || !address.contains('@') {
        return Err(invalid());
    }
    let mut parts = address.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid());
    };
    // The domain must have a non-empty label on both sides of the
    // dot: `user@.com`, `user@com.` and `user@example..com` are all
    // malformed, not just missing a dot entirely.
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain.split('.').any(str::is_empty)
    {
        return Err(invalid());
    }
    Ok(())
}

// AWS SigV4 signing (SES), following the same scheme as the cloud
// providers module: canonical request, credential scope, and the
// four-step HMAC key derivation. Only `host`, `x-amz-date`, and
// `content-type` are signed, and the payload hash is the SHA-256 of
// the request body.
pub(crate) struct SigV4Params {
    pub(crate) access_key_id: String,
    pub(crate) secret_access_key: String,
    pub(crate) region: String,
    pub(crate) service: String,
}

/// Percent-encode a query name or value per RFC 3986: only the
/// unreserved characters stay literal; everything else (including a
/// space, which becomes `%20`, never `+`) is uppercase `%XX`.
pub(crate) fn percent_encode_rfc3986(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

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
    // Header names are lowercased before sorting and signing: the
    // caller may pass any case, and AWS signs lowercase names.
    let mut signed: Vec<(String, String)> = headers_to_sign
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    signed.sort_by(|a, b| a.0.cmp(&b.0));
    let signed_headers: Vec<&str> = signed.iter().map(|(name, _)| name.as_str()).collect();
    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    // The canonical query string is built from sorted, RFC 3986
    // percent-encoded pairs; the raw query string is never used
    // verbatim, so parameter order and encoding cannot change the
    // signature.
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    pairs.sort();
    let canonical_query: String = pairs
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                percent_encode_rfc3986(name),
                percent_encode_rfc3986(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let canonical_request = format!(
        "{method}\n{}\n{canonical_query}\n{canonical_headers}\n{}\n{}",
        url.path(),
        signed_headers.join(";"),
        sha256_hex(payload),
    );
    let scope = format!(
        "{date_stamp}/{}/{}/aws4_request",
        params.region, params.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(
        format!("AWS4{}", params.secret_access_key).as_bytes(),
        date_stamp.as_bytes(),
    );
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_renders_lowercase_hex() {
        assert_eq!(hex_encode(&[0xAB, 0x01, 0xFF]).as_str(), "ab01ff");
        assert_eq!(hex_encode(&[]).as_str(), "");
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b"").as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc").as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_test_case_2() {
        let tag = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex_encode(&tag).as_str(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn signatures_equal_is_length_and_content_sensitive() {
        assert!(signatures_equal("abc", "abc"));
        assert!(signatures_equal("", ""));
        assert!(!signatures_equal("abc", "abd"));
        assert!(!signatures_equal("abc", "ab"));
    }

    #[test]
    fn validate_email_address_accepts_single_at_with_dot_domain() {
        assert!(validate_email_address("user@example.com").is_ok());
        assert!(validate_email_address("  user@example.com  ").is_ok());
        assert!(validate_email_address("plain").is_err());
        assert!(validate_email_address("a@b@c.com").is_err());
        assert!(validate_email_address("@example.com").is_err());
        assert!(validate_email_address("user@example").is_err());
        assert!(validate_email_address("").is_err());
    }

    #[test]
    fn validate_email_address_rejects_empty_domain_labels_and_never_echoes_input() {
        assert!(validate_email_address("user@.com").is_err());
        assert!(validate_email_address("user@com.").is_err());
        assert!(validate_email_address("user@.example.com").is_err());
        assert!(validate_email_address("user@example..com").is_err());
        let error = validate_email_address("user@.com").unwrap_err();
        assert!(!error.message.contains("user@.com"));
        assert_eq!(error.message.as_str(), "Invalid email address");
    }

    #[test]
    fn percent_encode_rfc3986_keeps_unreserved_and_encodes_the_rest() {
        assert_eq!(percent_encode_rfc3986("abc-_.~123").as_str(), "abc-_.~123");
        assert_eq!(percent_encode_rfc3986("a b+c/d").as_str(), "a%20b%2Bc%2Fd");
    }

    #[test]
    fn sigv4_authorization_matches_the_aws_example_signature() {
        let params = SigV4Params {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            region: "us-east-1".to_string(),
            service: "ses".to_string(),
        };
        let now = DateTime::parse_from_rfc3339("2024-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // Unsorted query parameters and mixed-case header names must
        // not change the signature: the canonical query is sorted and
        // RFC 3986 encoded, and header names are lowercased before
        // signing. The expected signature below is the AWS SigV4
        // value for these exact inputs (verified against an
        // independent OpenSSL computation of the same canonical
        // request, string to sign, and HMAC key chain).
        let url = Url::parse(
            "https://email.us-east-1.amazonaws.com/?Version=2010-12-01&Action=SendEmail",
        )
        .unwrap();
        let (authorization, amz_date) = sigv4_authorization(
            &params,
            "POST",
            &url,
            &[
                ("Host", "email.us-east-1.amazonaws.com".to_string()),
                ("X-Amz-Date", "20240115T120000Z".to_string()),
                (
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
            ],
            b"Action=SendEmail",
            now,
        );
        assert_eq!(amz_date.as_str(), "20240115T120000Z");
        assert_eq!(
            authorization.as_str(),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240115/us-east-1/ses/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=d6a1af2a55b9987d992fddc3b509f3a700735cac3938af941b4ab70d63668a97"
        );
        // The already-sorted, already-lowercase form signs identically.
        let sorted_url = Url::parse(
            "https://email.us-east-1.amazonaws.com/?Action=SendEmail&Version=2010-12-01",
        )
        .unwrap();
        let (sorted_authorization, _) = sigv4_authorization(
            &params,
            "POST",
            &sorted_url,
            &[
                (
                    "content-type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
                ("host", "email.us-east-1.amazonaws.com".to_string()),
                ("x-amz-date", "20240115T120000Z".to_string()),
            ],
            b"Action=SendEmail",
            now,
        );
        assert_eq!(authorization, sorted_authorization);
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
