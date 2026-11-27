//! Cloudflare connector module. - Cloudflare API client.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use reqwest::Method;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::time::Duration;

/// Validate a Cloudflare zone / record / account id before it is
/// interpolated into a request path. Ids are opaque ASCII tokens, so
/// anything else is rejected rather than sent.
pub(super) fn validate_cf_id(value: &str, what: &str) -> UnifiedResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!("Invalid {what} id")));
    }
    Ok(())
}

pub(super) fn parse_retry_after(response: &reqwest::Response) -> u64 {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(CF_DEFAULT_RETRY_AFTER_SECS)
        .min(CF_MAX_RETRY_AFTER_SECS)
}

/// Call the Cloudflare v4 API and return the full response envelope.
/// Callers read `result` (and `result_info` for lists) from it.
/// Handles the 429 backoff window and maps Cloudflare error objects
/// onto `UnifiedError` without leaking the API token.
pub(super) async fn cf_call(
    state: &CloudflareState,
    method: Method,
    path: &str,
    body: Option<Value>,
    query: &[(String, String)],
) -> UnifiedResult<Value> {
    if let Some(wait) = state.is_rate_limited() {
        return Err(UnifiedError::new(
            format!("Cloudflare API rate limit is active; retry in {wait} seconds"),
            429,
        )
        .with_metadata("retry_after_seconds", json!(wait)));
    }
    let token = state.api_token().ok_or_else(|| {
        UnifiedError::unprocessable("Cloudflare API token is not configured")
    })?;
    if !path.starts_with('/') || path.contains("..") {
        return Err(UnifiedError::bad_request("Invalid Cloudflare API path"));
    }
    let mut url = url::Url::parse(&format!("{CF_API_BASE}{path}"))
        .map_err(|_| UnifiedError::bad_request("Invalid Cloudflare API path"))?;
    // The host is a constant, but verify it anyway: this module must
    // never send the token anywhere else.
    if url.host_str() != Some("api.cloudflare.com") {
        return Err(UnifiedError::internal(
            "Cloudflare API host allow-list violation",
        ));
    }
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let mut request = state
        .client
        .request(method, url)
        .bearer_auth(&token)
        .header(reqwest::header::USER_AGENT, CF_USER_AGENT)
        .timeout(Duration::from_secs(CF_HTTP_TIMEOUT_SECS));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Cloudflare API request failed: {error}"))
    })?;
    let status = response.status();
    state.record_rate_limit_headers(&response);
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = parse_retry_after(&response);
        state.record_429(retry_after);
        return Err(UnifiedError::new(
            format!("Cloudflare API rate limit reached; retry in {retry_after} seconds"),
            429,
        )
        .with_metadata("retry_after_seconds", json!(retry_after)));
    }
    let raw = response.text().await.map_err(|error| {
        UnifiedError::internal(format!("Cannot read Cloudflare API response: {error}"))
    })?;
    let body = parse_cf_response(status, &raw)?;
    state.clear_rate_limit();
    Ok(body)
}

/// Truncate a raw response body for inclusion in an error message.
pub(super) fn truncate_cf_body(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "(empty response body)".to_string();
    }
    if trimmed.chars().count() > CF_MAX_ERROR_BODY_CHARS {
        let head: String = trimmed.chars().take(CF_MAX_ERROR_BODY_CHARS).collect();
        format!("{head}...")
    } else {
        trimmed.to_string()
    }
}

/// Interpret one raw Cloudflare API response body. A JSON envelope
/// keeps the existing behaviour (Cloudflare `errors[0].message` with
/// the HTTP status). A body that is not JSON no longer fails parsing
/// with a generic internal error: on a non-2xx status the raw body
/// text (truncated) is surfaced with the real HTTP status, so the
/// caller sees what Cloudflare actually returned; only a 2xx
/// response with an unparseable body is still an internal error.
pub(super) fn parse_cf_response(status: reqwest::StatusCode, raw: &str) -> UnifiedResult<Value> {
    let body: Value = match serde_json::from_str(raw) {
        Ok(body) => body,
        Err(_) => {
            let snippet = truncate_cf_body(raw);
            if status.is_success() {
                return Err(UnifiedError::internal(format!(
                    "Cannot parse Cloudflare API response (HTTP {}): {snippet}",
                    status.as_u16()
                )));
            }
            return Err(UnifiedError::new(
                format!("Cloudflare API error (HTTP {}): {snippet}", status.as_u16()),
                status.as_u16(),
            ));
        }
    };
    if !status.is_success() || body.get("success").and_then(Value::as_bool) != Some(true) {
        let first_error = body
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|errors| errors.first())
            .cloned()
            .unwrap_or(Value::Null);
        let message = first_error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Cloudflare API request failed");
        let mut error = UnifiedError::new(
            format!("Cloudflare API error: {message}"),
            status.as_u16(),
        );
        if let Some(code) = first_error.get("code").and_then(Value::as_i64) {
            error = error.with_metadata("cloudflare_error_code", json!(code));
        }
        return Err(error);
    }
    Ok(body)
}

/// The `result` value of a Cloudflare response envelope.
pub(super) fn cf_result(body: &Value) -> Value {
    body.get("result").cloned().unwrap_or(Value::Null)
}

/// Build a page of results from a Cloudflare list response: the
/// `result` array plus `result_info.total_count` for pagination.
pub(super) fn cf_page<T: Serialize>(
    envelope: &Value,
    items: Vec<T>,
    params: &PaginationParams,
) -> Value {
    let total = envelope
        .get("result_info")
        .and_then(|info| info.get("total_count"))
        .and_then(Value::as_u64)
        .unwrap_or(items.len() as u64);
    PageResult::new(items, total, &params.normalized()).to_json()
}

pub(super) fn cf_query(params: &PaginationParams) -> Vec<(String, String)> {
    let pagination = params.normalized();
    vec![
        ("page".to_string(), pagination.page.to_string()),
        ("per_page".to_string(), pagination.per_page.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cf_response_accepts_a_success_envelope() {
        let body = parse_cf_response(
            reqwest::StatusCode::OK,
            r#"{"success":true,"result":{"id":"zone-1"}}"#,
        )
        .unwrap();
        assert_eq!(body["result"]["id"], "zone-1");
    }

    #[test]
    fn parse_cf_response_keeps_json_error_shape() {
        let error = parse_cf_response(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"success":false,"errors":[{"code":9109,"message":"Invalid access token"}]}"#,
        )
        .unwrap_err();
        assert_eq!(error.code, 403);
        assert!(error.message.contains("Invalid access token"));
        assert_eq!(
            error.metadata.get("cloudflare_error_code"),
            Some(&json!(9109))
        );
    }

    #[test]
    fn parse_cf_response_falls_back_to_raw_text_on_non_json_error_body() {
        let error = parse_cf_response(
            reqwest::StatusCode::FORBIDDEN,
            "error code: 1020\nAccess denied by firewall",
        )
        .unwrap_err();
        // The real status is preserved, not masked as a 500 parse error.
        assert_eq!(error.code, 403);
        assert!(error.message.contains("Access denied by firewall"));
        assert!(!error.message.contains("Cannot parse"));
    }

    #[test]
    fn parse_cf_response_surfaces_html_error_bodies_with_their_status() {
        let error = parse_cf_response(
            reqwest::StatusCode::BAD_GATEWAY,
            "<html><body>Bad Gateway</body></html>",
        )
        .unwrap_err();
        assert_eq!(error.code, 502);
        assert!(error.message.contains("Bad Gateway"));
    }

    #[test]
    fn parse_cf_response_still_fails_on_unparseable_success_body() {
        let error = parse_cf_response(reqwest::StatusCode::OK, "not json at all").unwrap_err();
        assert_eq!(error.code, 500);
        assert!(error
            .message
            .contains("Cannot parse Cloudflare API response"));
        assert!(error.message.contains("not json at all"));
    }

    #[test]
    fn parse_cf_response_truncates_long_non_json_bodies() {
        let long_body = "x".repeat(2000);
        let error =
            parse_cf_response(reqwest::StatusCode::SERVICE_UNAVAILABLE, &long_body).unwrap_err();
        assert_eq!(error.code, 503);
        assert!(error.message.ends_with("..."));
        assert!(error.message.len() < long_body.len());
    }

    #[test]
    fn truncate_cf_body_handles_empty_and_short_bodies() {
        assert_eq!(truncate_cf_body(""), "(empty response body)");
        assert_eq!(truncate_cf_body("  \n "), "(empty response body)");
        assert_eq!(truncate_cf_body("  plain error  "), "plain error");
    }
}
