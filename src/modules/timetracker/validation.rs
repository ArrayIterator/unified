// timetracker - Validation helpers and small pure utilities.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

// Helpers: validation and small utilities

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Round a money amount to 4 decimal places (NUMERIC(19,4)).
pub(crate) fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != MAX_CURRENCY_LEN
        || !upper.chars().all(|c| c.is_ascii_uppercase())
    {
        return Err(UnifiedError::bad_request(format!(
            "currency must be a 3-letter ISO 4217 code, got \"{currency}\""
        )));
    }
    Ok(upper)
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no timezone conversion is done anywhere (all stored times
/// are UTC; the timezone is a display hint for reports).
pub(crate) fn valid_timezone(timezone: &str) -> UnifiedResult<String> {
    let timezone = timezone.trim();
    if timezone.is_empty() || timezone.len() > 64 {
        return Err(UnifiedError::bad_request("timezone must not be empty"));
    }
    if timezone == "UTC" {
        return Ok("UTC".to_string());
    }
    let shaped = timezone.split('/').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
    }) && timezone.contains('/');
    if !shaped {
        return Err(UnifiedError::bad_request(format!(
            "Invalid timezone: {timezone}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
        )));
    }
    Ok(timezone.to_string())
}

/// A working-hours value is `"HH:MM"` on a 24-hour clock.
pub(crate) fn valid_hhmm(value: &str, field: &str) -> UnifiedResult<String> {
    let value = value.trim();
    let mut parts = value.split(':');
    let hour: u32 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(|| UnifiedError::bad_request(format!("{field} must look like \"09:00\"")))?;
    let minute: u32 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(|| UnifiedError::bad_request(format!("{field} must look like \"09:00\"")))?;
    if parts.next().is_some() || hour > 23 || minute > 59 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must look like \"09:00\" (24-hour clock)"
        )));
    }
    Ok(format!("{hour:02}:{minute:02}"))
}

pub(crate) fn valid_email(email: &str) -> UnifiedResult<String> {
    let email = email.trim().to_lowercase();
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if local.is_empty() || domain.is_empty() || !domain.contains('.') || parts.next().is_some() {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {email}"
        )));
    }
    Ok(email)
}

/// SHA-256 hex digest (used for invite tokens and screenshot bytes).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// A window-title hash must be exactly 64 lowercase hex chars
/// (SHA-256). Anything else is rejected: the raw title is never
/// accepted, so a malformed hash cannot smuggle one in.
pub(crate) fn valid_title_hash(raw: &str) -> UnifiedResult<String> {
    let raw = raw.trim().to_lowercase();
    if raw.len() != 64 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UnifiedError::bad_request(
            "window_title_hash must be a 64-character lowercase hex SHA-256 digest of the window title; the raw title is never accepted",
        ));
    }
    Ok(raw)
}

pub(crate) fn validate_interval(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    max_secs: i64,
) -> UnifiedResult<i64> {
    if end <= start {
        return Err(UnifiedError::bad_request(
            "interval end must be after interval start",
        ));
    }
    let secs = (end - start).num_seconds();
    if secs > max_secs {
        return Err(UnifiedError::bad_request(format!(
            "interval must be at most {max_secs} seconds"
        )));
    }
    Ok(secs)
}

pub(crate) fn team_member_key(team_id: Uuid, user_id: &str) -> String {
    format!("{team_id}:{user_id}")
}

pub(crate) fn team_ids_for_user(state: &TimeTrackerState, user_id: &str) -> Vec<Uuid> {
    state
        .team_members
        .iter()
        .filter(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().team_id)
        .collect()
}

pub(crate) fn active_timer_for(
    state: &TimeTrackerState,
    user_id: &str,
) -> Option<TimeTrackerTimer> {
    state
        .timers
        .iter()
        .map(|entry| entry.value().clone())
        .find(|timer| {
            timer.user_id == user_id
                && (timer.status == TimerStatus::Running || timer.status == TimerStatus::Paused)
        })
}

pub(crate) fn load_timer(
    state: &TimeTrackerState,
    raw_id: &str,
) -> UnifiedResult<TimeTrackerTimer> {
    let id = parse_uuid(raw_id, "timer")?;
    state
        .timers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {id}")))
}

pub(crate) fn load_entry(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<TimeEntry> {
    let id = parse_uuid(raw_id, "entry")?;
    state
        .entries
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Time entry not found: {id}")))
}

pub(crate) fn validate_list(
    values: &[String],
    field: &str,
) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in values {
        let value = require_text(raw, field, MAX_APP_NAME_LEN)?.to_lowercase();
        if !cleaned.contains(&value) {
            cleaned.push(value);
        }
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_text_and_optional_text() {
        assert_eq!(require_text(" x ", "name", 10).unwrap(), "x");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text(&"x".repeat(11), "name", 10).is_err());
        assert_eq!(optional_text(&None, "name", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some(" y ".to_string()), "name", 10).unwrap(),
            Some("y".to_string())
        );
        assert!(optional_text(&Some("".to_string()), "name", 10).is_err());
    }

    #[test]
    fn parse_uuid_accepts_only_uuids() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "timer").unwrap(), id);
        assert!(parse_uuid("not-a-uuid", "timer").is_err());
    }

    #[test]
    fn round4_rounds_to_four_places() {
        assert_eq!(round4(1.23456), 1.2346);
        assert_eq!(round4(1.23454), 1.2345);
        assert_eq!(round4(2.5), 2.5);
        assert_eq!(round4(0.0), 0.0);
    }

    #[test]
    fn valid_currency_codes() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency(" EUR ").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("U1D").is_err());
    }

    #[test]
    fn valid_timezone_names() {
        assert_eq!(valid_timezone("UTC").unwrap(), "UTC");
        assert_eq!(valid_timezone("Asia/Jakarta").unwrap(), "Asia/Jakarta");
        assert_eq!(valid_timezone("Europe/London").unwrap(), "Europe/London");
        assert!(valid_timezone("").is_err());
        assert!(valid_timezone("Asia//Jakarta").is_err());
        assert!(valid_timezone("Asia/Jakarta/").is_err());
        assert!(valid_timezone("Bad Timezone").is_err());
        assert!(valid_timezone("America/New York").is_err());
    }

    #[test]
    fn valid_hhmm_values() {
        assert_eq!(valid_hhmm("09:00", "start").unwrap(), "09:00");
        assert_eq!(valid_hhmm("9:00", "start").unwrap(), "09:00");
        assert_eq!(valid_hhmm("23:59", "end").unwrap(), "23:59");
        assert!(valid_hhmm("24:00", "start").is_err());
        assert!(valid_hhmm("12:60", "start").is_err());
        assert!(valid_hhmm("ab:cd", "start").is_err());
        assert!(valid_hhmm("09:00:00", "start").is_err());
        assert!(valid_hhmm("0900", "start").is_err());
    }

    #[test]
    fn valid_email_normalises() {
        assert_eq!(
            valid_email("User@Example.COM").unwrap(),
            "user@example.com"
        );
        assert!(valid_email("nope").is_err());
        assert!(valid_email("a@b").is_err());
        assert!(valid_email("a@@b.com").is_err());
        assert!(valid_email("@b.com").is_err());
    }

    #[test]
    fn valid_title_hash_requires_sha256_hex() {
        assert_eq!(valid_title_hash(&"a".repeat(64)).unwrap(), "a".repeat(64));
        assert_eq!(valid_title_hash(&"A".repeat(64)).unwrap(), "a".repeat(64));
        assert!(valid_title_hash(&"a".repeat(63)).is_err());
        assert!(valid_title_hash(&"g".repeat(64)).is_err());
        assert!(valid_title_hash("raw title").is_err());
    }

    #[test]
    fn validate_interval_bounds() {
        let start = "2026-11-16T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-11-16T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(validate_interval(start, end, 86_400).unwrap(), 3600);
        assert!(validate_interval(end, start, 86_400).is_err());
        assert!(validate_interval(start, start, 86_400).is_err());
        let far = "2026-11-17T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(validate_interval(start, far, 86_400).is_err());
    }

    #[test]
    fn team_member_key_format() {
        let team = Uuid::nil();
        assert_eq!(
            team_member_key(team, "u1"),
            "00000000-0000-0000-0000-000000000000:u1"
        );
    }

    #[test]
    fn validate_list_normalises_and_dedupes() {
        assert_eq!(
            validate_list(&[" Work ".to_string(), "WORK".to_string()], "targets")
                .unwrap(),
            vec!["work".to_string()]
        );
        assert_eq!(
            validate_list(&[], "targets").unwrap(),
            Vec::<String>::new()
        );
        assert!(validate_list(&["".to_string()], "targets").is_err());
    }
    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

}
