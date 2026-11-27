//! External provider response parsing and fetch plumbing.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// Provider response parsing (shapes from provider documentation, NOT
// live-verified - see the module header).

fn text_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn string_list(value: &Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a Nager.Date response body: a JSON array of
/// {"date", "localName", "name", "countryCode", "fixed", "global",
/// "counties", "types"}. Items for another country, or dated
/// outside the requested year, are skipped: a response for the
/// wrong country or year must never be stored under the requested
/// (country, year).
pub(crate) fn parse_nager_response(body: &str, country: &str, year: i32) -> UnifiedResult<Vec<FetchedHoliday>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| UnifiedError::internal("Nager.Date returned an unreadable response"))?;
    let items = value
        .as_array()
        .ok_or_else(|| UnifiedError::internal("Nager.Date response was not a JSON array"))?;
    let mut out = Vec::new();
    for item in items {
        if let Some(code) = text_field(item, "countryCode") {
            if !code.eq_ignore_ascii_case(country) {
                continue;
            }
        }
        let Some(date_text) = text_field(item, "date") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(&date_text, "%Y-%m-%d") else {
            continue;
        };
        if date.year() != year {
            continue;
        }
        let name = text_field(item, "localName")
            .or_else(|| text_field(item, "name"))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        out.push(FetchedHoliday {
            date,
            name,
            local_name: text_field(item, "name"),
            global: item.get("global").and_then(Value::as_bool).unwrap_or(true),
            types: string_list(item, "types"),
            counties: string_list(item, "counties"),
        });
    }
    Ok(out)
}

/// Parse a Calendarific response body:
/// {"response": {"holidays": [{"name", "description",
/// "date": {"iso": "2026-01-01"}, "type": [...] }]}}.
pub(crate) fn parse_calendarific_response(body: &str) -> UnifiedResult<Vec<FetchedHoliday>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| UnifiedError::internal("Calendarific returned an unreadable response"))?;
    let holidays = value
        .get("response")
        .and_then(|response| response.get("holidays"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            UnifiedError::internal("Calendarific response had no response.holidays array")
        })?;
    let mut out = Vec::new();
    for item in holidays {
        let date_text = item
            .get("date")
            .and_then(|date| date.get("iso"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| text_field(item, "date"));
        let Some(date_text) = date_text else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
            continue;
        };
        let Some(name) = text_field(item, "name") else {
            continue;
        };
        out.push(FetchedHoliday {
            date,
            name,
            local_name: text_field(item, "description"),
            global: true,
            types: string_list(item, "type"),
            counties: Vec::new(),
        });
    }
    Ok(out)
}

/// Parse an Abstract API response body: a JSON array of
/// {"date": "2026-01-01", "name": ...} (also accepts
/// {"holidays": [...]} and a nested {"date": {"iso": ...}} date).
pub(crate) fn parse_abstract_response(body: &str) -> UnifiedResult<Vec<FetchedHoliday>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| UnifiedError::internal("Abstract API returned an unreadable response"))?;
    let items: &Vec<Value> = match value.as_array() {
        Some(items) => items,
        None => value
            .get("holidays")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                UnifiedError::internal("Abstract API response was not a JSON array")
            })?,
    };
    let mut out = Vec::new();
    for item in items {
        let date_text = text_field(item, "date").or_else(|| {
            item.get("date")
                .and_then(|date| date.get("iso"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let Some(date_text) = date_text else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
            continue;
        };
        let Some(name) = text_field(item, "name") else {
            continue;
        };
        out.push(FetchedHoliday {
            date,
            name,
            local_name: None,
            global: true,
            types: string_list(item, "type"),
            counties: Vec::new(),
        });
    }
    Ok(out)
}

// Fetching: rate limits, retries, SSRF-safe client.

/// Minimum-interval wait for one fetch, given the last reserved
/// request instant. A `last_request` in the future (reserved by a
/// concurrent caller) waits until that instant plus the gap:
/// `checked_duration_since` returns `None` for a future instant,
/// which must never be read as "no wait".
fn min_interval_wait(last_request: Option<Instant>, now: Instant, gap: Duration) -> Duration {
    let Some(last) = last_request else {
        return Duration::from_secs(0);
    };
    if last > now {
        (last - now) + gap
    } else if let Some(elapsed) = now.checked_duration_since(last) {
        gap.saturating_sub(elapsed)
    } else {
        Duration::from_secs(0)
    }
}

/// Wait until the provider's rate limits allow one more request.
/// Returns an error when the wait would exceed
/// MAX_RATE_LIMIT_WAIT instead of sleeping indefinitely.
async fn wait_for_rate_limit(state: &HolidaysState, provider: &HolidayProvider) -> UnifiedResult<()> {
    let entry = state
        .rate_state
        .entry(provider.id.clone())
        .or_insert_with(|| Arc::new(Mutex::new(RateState::default())));
    let guard = entry.value().clone();
    let wait = {
        let mut rate = guard.lock().expect("rate limit lock poisoned");
        let now = Instant::now();
        let mut wait = Duration::from_secs(0);
        if provider.min_interval_secs > 0 {
            let gap = Duration::from_secs(provider.min_interval_secs);
            wait = wait.max(min_interval_wait(rate.last_request, now, gap));
        }
        let window_start = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        rate.window.retain(|instant| *instant >= window_start);
        if rate.window.len() as u32 >= provider.max_requests_per_minute {
            if let Some(oldest) = rate.window.front() {
                let until = *oldest + Duration::from_secs(60);
                if until > now {
                    wait = wait.max(until - now);
                }
            }
        }
        if wait > MAX_RATE_LIMIT_WAIT {
            return Err(UnifiedError::internal(format!(
                "Rate limit wait for provider \"{}\" would exceed {} seconds; try again later",
                provider.id,
                MAX_RATE_LIMIT_WAIT.as_secs()
            )));
        }
        // Reserve the slot now, so concurrent fetches serialize
        // instead of all sleeping the same short wait and then
        // firing together.
        rate.last_request = Some(now + wait);
        rate.window.push_back(now + wait);
        wait
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    Ok(())
}

/// Parse a `Retry-After` header value: seconds, or nothing (HTTP
/// dates are not parsed - no httpdate crate - and fall back to the
/// exponential backoff). Capped at MAX_RETRY_AFTER.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

/// Append one streamed chunk to the buffered body, refusing the
/// whole response as soon as the cap is exceeded. The cap is
/// enforced while downloading, never after the full body was
/// already held in memory.
fn append_capped(body: &mut Vec<u8>, chunk: &[u8], provider_id: &str) -> UnifiedResult<()> {
    if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
        return Err(UnifiedError::internal(format!(
            "Provider \"{provider_id}\" response exceeded the {MAX_RESPONSE_BYTES} byte limit",
        )));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

/// One HTTP GET with retries: 429 honours `Retry-After`, 408 / 5xx
/// and transport errors retry with exponential backoff, other 4xx
/// fail fast. The returned body is capped at MAX_RESPONSE_BYTES.
/// The URL is never logged and never appears in an error message
/// (it may carry an API key as a query parameter).
pub(crate) async fn fetch_with_retries(
    state: &HolidaysState,
    provider: &HolidayProvider,
    url: &str,
) -> UnifiedResult<Vec<u8>> {
    let allowed_hosts: Arc<Vec<String>> = Arc::new(provider.allowed_hosts.clone());
    let client = build_fetch_client(allowed_hosts)?;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        wait_for_rate_limit(state, provider).await?;
        let response = client.get(url).send().await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                if attempt < MAX_FETCH_ATTEMPTS {
                    let backoff = RETRY_BASE_DELAY
                        .mul_f64(2_f64.powi((attempt - 1) as i32))
                        .min(RETRY_MAX_DELAY);
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                return Err(UnifiedError::internal(format!(
                    "Provider \"{}\" request failed: {error}",
                    provider.id
                )));
            }
        };
        let status = response.status();
        if status.is_success() {
            // Stream the body in chunks and enforce the cap as the
            // bytes arrive: a huge response is aborted mid-download
            // instead of being fully buffered first.
            let mut body: Vec<u8> = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| {
                    UnifiedError::internal(format!(
                        "Provider \"{}\" response could not be read: {error}",
                        provider.id
                    ))
                })?
            {
                append_capped(&mut body, &chunk, &provider.id)?;
            }
            return Ok(body);
        }
        if status.as_u16() == 429 {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after);
            if attempt < MAX_FETCH_ATTEMPTS {
                let backoff = retry_after.unwrap_or_else(|| {
                    RETRY_BASE_DELAY
                        .mul_f64(2_f64.powi((attempt - 1) as i32))
                        .min(RETRY_MAX_DELAY)
                });
                tokio::time::sleep(backoff).await;
                continue;
            }
            return Err(UnifiedError::internal(format!(
                "Provider \"{}\" rate-limited the request (429)",
                provider.id
            )));
        }
        if status.as_u16() == 408 || status.is_server_error() {
            if attempt < MAX_FETCH_ATTEMPTS {
                let backoff = RETRY_BASE_DELAY
                    .mul_f64(2_f64.powi((attempt - 1) as i32))
                    .min(RETRY_MAX_DELAY);
                tokio::time::sleep(backoff).await;
                continue;
            }
            return Err(UnifiedError::internal(format!(
                "Provider \"{}\" answered with status {status}",
                provider.id
            )));
        }
        return Err(UnifiedError::internal(format!(
            "Provider \"{}\" answered with status {status}",
            provider.id
        )));
    }
}

/// Build the fetch URL for one (provider, country, year) and the
/// allowlist that fetch must satisfy. For `generic_ical` the URL is
/// the config's `feed_url` and the allowlist is that URL's single
/// host; for every other kind the URL is built from the provider's
/// `base_url` and the allowlist is the provider definition's. The
/// API key (when the kind needs one) is decrypted here and appended
/// as a query parameter over https only; it is never logged.
pub(crate) fn build_fetch_url(
    provider: &HolidayProvider,
    config: &HolidaySyncConfig,
    country: &str,
    year: i32,
    secret: &str,
) -> UnifiedResult<(String, Vec<String>)> {
    match provider.kind {
        ProviderKind::NagerDate => {
            let base = provider.base_url.trim_end_matches('/');
            Ok((
                format!("{base}/api/v3/publicholidays/{year}/{country}"),
                provider.allowed_hosts.clone(),
            ))
        }
        ProviderKind::Calendarific => {
            let key = provider_key(provider, secret)?;
            let base = provider.base_url.trim_end_matches('/');
            let mut url = url::Url::parse(&format!("{base}/api/v2/holidays"))
                .map_err(|_| UnifiedError::internal("Calendarific base URL is invalid"))?;
            url.query_pairs_mut()
                .append_pair("api_key", &key)
                .append_pair("country", country)
                .append_pair("year", &year.to_string());
            Ok((url.to_string(), provider.allowed_hosts.clone()))
        }
        ProviderKind::AbstractApi => {
            let key = provider_key(provider, secret)?;
            let base = provider.base_url.trim_end_matches('/');
            let mut url = url::Url::parse(base)
                .map_err(|_| UnifiedError::internal("Abstract API base URL is invalid"))?;
            url.query_pairs_mut()
                .append_pair("api_key", &key)
                .append_pair("country", country)
                .append_pair("year", &year.to_string());
            Ok((url.to_string(), provider.allowed_hosts.clone()))
        }
        ProviderKind::GoogleIcal => {
            let Some(calendar_id) = provider.calendar_ids.get(country) else {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"google-ical\" has no public calendar id configured for country \"{country}\""
                )));
            };
            let base = provider.base_url.trim_end_matches('/');
            // Build the URL from path segments so the calendar id
            // is percent-encoded: a raw `#` would start a URL
            // fragment that is never sent to the server, breaking
            // every seeded id such as
            // `en.usa#holiday@group.v.calendar.google.com`.
            let mut url = url::Url::parse(base)
                .map_err(|_| UnifiedError::internal("Google Calendar base URL is invalid"))?;
            {
                let mut segments = url.path_segments_mut().map_err(|_| {
                    UnifiedError::internal("Google Calendar base URL cannot take path segments")
                })?;
                segments.push("calendar");
                segments.push("ical");
                segments.push(calendar_id);
                segments.push("public");
                segments.push("basic.ics");
            }
            Ok((url.to_string(), provider.allowed_hosts.clone()))
        }
        ProviderKind::GenericIcal => {
            let Some(feed_url) = config.feed_url.as_deref() else {
                return Err(UnifiedError::bad_request(
                    "Generic iCal provider needs a feed_url on the sync config",
                ));
            };
            let parsed = url::Url::parse(feed_url.trim())
                .map_err(|_| UnifiedError::bad_request("Sync config feed_url is invalid"))?;
            let Some(host) = parsed.host_str() else {
                return Err(UnifiedError::bad_request(
                    "Sync config feed_url must have a host",
                ));
            };
            Ok((feed_url.trim().to_string(), vec![host.to_string()]))
        }
    }
}

/// Decrypt the stored key for a keyed provider. Fails closed when
/// the provider needs a key and none is stored, and when no
/// encryption secret is configured at all: decrypting (or
/// authenticating) with an empty secret must never silently
/// succeed or fall back to a known value.
fn provider_key(provider: &HolidayProvider, secret: &str) -> UnifiedResult<String> {
    if !provider.kind.key_required() {
        return Ok(String::new());
    }
    if secret.trim().is_empty() {
        return Err(UnifiedError::internal(format!(
            "Provider \"{}\" needs an API key but no encryption secret is configured (app.secret_key); the stored key cannot be decrypted",
            provider.id
        )));
    }
    let Some(stored) = provider.api_key_encrypted.as_deref() else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{}\" needs an API key and none is configured",
            provider.id
        )));
    };
    decrypt_api_key(secret, stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_field_returns_trimmed_string() {
        let value = json!({"name": "  Christmas Day  ", "n": 3});
        assert_eq!(text_field(&value, "name").as_deref(), Some("Christmas Day"));
    }

    #[test]
    fn text_field_rejects_missing_empty_and_non_string() {
        let value = json!({"name": "   ", "n": 3});
        assert_eq!(text_field(&value, "name"), None);
        assert_eq!(text_field(&value, "missing"), None);
        assert_eq!(text_field(&value, "n"), None);
    }

    #[test]
    fn string_list_returns_strings() {
        let value = json!({"types": ["Public", "  National  ", "", 7]});
        assert_eq!(string_list(&value, "types"), vec!["Public", "National"]);
        assert_eq!(string_list(&value, "missing"), Vec::<String>::new());
    }

    #[test]
    fn parse_nager_response_parses_holidays() {
        let body = r#"[
            {"date": "2026-01-01", "localName": "Tahun Baru", "name": "New Year's Day", "countryCode": "ID", "fixed": true, "global": true, "types": ["Public"]},
            {"date": "2026-12-25", "localName": "Natal", "name": "Christmas Day", "global": false, "types": ["Public", "Religious"]},
            {"date": "not-a-date", "localName": "Bad", "name": "Bad"},
            {"date": "2026-05-01", "localName": "", "name": ""}
        ]"#;
        let holidays = parse_nager_response(body, "ID", 2026).unwrap();
        assert_eq!(holidays.len(), 2);
    }

    #[test]
    fn parse_nager_response_skips_wrong_country_and_year() {
        let body = r#"[
            {"date": "2026-01-01", "localName": "ID New Year", "name": "New Year", "countryCode": "ID"},
            {"date": "2026-01-01", "localName": "US New Year", "name": "New Year", "countryCode": "US"},
            {"date": "2025-12-25", "localName": "Last Year", "name": "Last Year", "countryCode": "ID"},
            {"date": "2026-05-01", "localName": "No Code", "name": "No Code"}
        ]"#;
        let holidays = parse_nager_response(body, "ID", 2026).unwrap();
        assert_eq!(holidays.len(), 2);
        assert!(holidays.iter().all(|holiday| holiday.date.year() == 2026));
    }

    #[test]
    fn parse_nager_response_rejects_bad_json_and_non_array() {
        assert!(parse_nager_response("not json", "ID", 2026).is_err());
        assert!(parse_nager_response(r#"{"date": "2026-01-01"}"#, "ID", 2026).is_err());
        assert_eq!(parse_nager_response("[]", "ID", 2026).unwrap().len(), 0);
    }

    #[test]
    fn parse_calendarific_response_parses_nested_shape() {
        let body = r#"{"response": {"holidays": [
            {"name": "New Year", "description": "First day", "date": {"iso": "2026-01-01"}, "type": ["National holiday"]},
            {"name": "No Date"}
        ]}}"#;
        let holidays = parse_calendarific_response(body).unwrap();
        assert_eq!(holidays.len(), 1);
        assert!(parse_calendarific_response(r#"{"response": {}}"#).is_err());
    }

    #[test]
    fn parse_abstract_response_accepts_array_and_wrapper() {
        let array = r#"[{"date": "2026-01-01", "name": "New Year"}]"#;
        assert_eq!(parse_abstract_response(array).unwrap().len(), 1);
        let wrapper = r#"{"holidays": [{"date": "2026-01-01", "name": "New Year"}]}"#;
        assert_eq!(parse_abstract_response(wrapper).unwrap().len(), 1);
        assert!(parse_abstract_response("{}").is_err());
        assert!(parse_abstract_response("nope").is_err());
    }

    fn google_provider() -> HolidayProvider {
        let now = Utc::now();
        HolidayProvider {
            id: "google-ical".to_string(),
            name: "Google Calendar Public Holidays".to_string(),
            kind: ProviderKind::GoogleIcal,
            base_url: "https://calendar.google.com".to_string(),
            allowed_hosts: vec!["calendar.google.com".to_string()],
            api_key_required: false,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs: 2,
            max_requests_per_minute: 30,
            calendar_ids: HashMap::from([(
                "US".to_string(),
                "en.usa#holiday@group.v.calendar.google.com".to_string(),
            )]),
            enabled: true,
            builtin: true,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_config() -> HolidaySyncConfig {
        let now = Utc::now();
        HolidaySyncConfig {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            company_id: String::new(),
            provider_id: "google-ical".to_string(),
            fallback_provider_ids: Vec::new(),
            countries: vec!["US".to_string()],
            years: vec![2026],
            schedule: DEFAULT_SCHEDULE.to_string(),
            feed_url: None,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn build_fetch_url_encodes_google_calendar_id() {
        let provider = google_provider();
        let config = test_config();
        let (url, _) = build_fetch_url(&provider, &config, "US", 2026, "").unwrap();
        // A raw `#` would start a fragment that is never sent to
        // the server; the id must reach the server percent-encoded.
        assert!(!url.contains('#'), "raw # must not appear in the URL: {url}");
        assert!(
            url.contains("en.usa%23holiday@group.v.calendar.google.com"),
            "{url}"
        );
        assert!(url.ends_with("/public/basic.ics"), "{url}");
    }

    #[test]
    fn provider_key_refuses_an_empty_secret() {
        let mut provider = google_provider();
        provider.kind = ProviderKind::Calendarific;
        provider.id = "calendarific".to_string();
        provider.api_key_encrypted = Some(encrypt_api_key("real-secret", "sk-test-key"));
        assert!(provider_key(&provider, "").is_err());
        assert!(provider_key(&provider, "   ").is_err());
        assert_eq!(
            provider_key(&provider, "real-secret").unwrap(),
            "sk-test-key"
        );
        // A keyless kind never needs the secret.
        assert_eq!(provider_key(&google_provider(), "").unwrap(), "");
    }

    #[test]
    fn append_capped_refuses_once_the_cap_is_exceeded() {
        let mut body = vec![0u8; MAX_RESPONSE_BYTES - 1];
        append_capped(&mut body, &[0u8; 1], "test-provider").unwrap();
        assert_eq!(body.len(), MAX_RESPONSE_BYTES);
        assert!(append_capped(&mut body, &[0u8; 1], "test-provider").is_err());
        let mut empty = Vec::new();
        assert!(append_capped(&mut empty, &[0u8; MAX_RESPONSE_BYTES + 1], "test-provider").is_err());
        assert!(empty.is_empty());
    }

    #[test]
    fn min_interval_wait_waits_past_a_future_reservation() {
        let now = Instant::now();
        let gap = Duration::from_secs(2);
        // Reserved 5s in the future by a concurrent caller: wait
        // until then, plus the gap - never zero.
        assert_eq!(
            min_interval_wait(Some(now + Duration::from_secs(5)), now, gap),
            Duration::from_secs(7)
        );
        // No previous request: no wait.
        assert_eq!(min_interval_wait(None, now, gap), Duration::from_secs(0));
        // Last request long ago: no wait.
        assert_eq!(
            min_interval_wait(Some(now - Duration::from_secs(10)), now, gap),
            Duration::from_secs(0)
        );
        // Last request 1s ago with a 2s gap: about 1s left.
        let wait = min_interval_wait(Some(now - Duration::from_secs(1)), now, gap);
        assert!(wait > Duration::from_secs(0) && wait <= Duration::from_secs(1));
    }
}
