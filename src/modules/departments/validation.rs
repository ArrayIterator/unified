//! Slug, email, working-hours and match-type helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpResponse};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Serialize};
use serde_json::{json};
use uuid::Uuid;
use super::types::WorkingHours;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// `My Support Team` -> `my-support-team`.
pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

pub(crate) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

pub(crate) fn valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(local), Some(domain), None)
        if !local.is_empty() && !domain.is_empty() && domain.contains('.'))
}

/// `09:30` -> minutes since midnight. `None` when malformed.
pub(crate) fn parse_hhmm(value: &str) -> Option<u32> {
    let (hour, minute) = value.split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

pub(crate) fn valid_working_hours(hours: &WorkingHours) -> UnifiedResult<()> {
    if hours.days.is_empty() {
        return Err(UnifiedError::bad_request(
            "working_hours.days must list at least one day",
        ));
    }
    if hours.days.iter().any(|day| *day > 6) {
        return Err(UnifiedError::bad_request(
            "working_hours.days entries must be 0 (Sunday) through 6 (Saturday)",
        ));
    }
    let start = parse_hhmm(&hours.start)
        .ok_or_else(|| UnifiedError::bad_request("working_hours.start must be HH:MM"))?;
    let end = parse_hhmm(&hours.end)
        .ok_or_else(|| UnifiedError::bad_request("working_hours.end must be HH:MM"))?;
    if start >= end {
        return Err(UnifiedError::bad_request(
            "working_hours.start must be before working_hours.end (overnight shifts are not supported)",
        ));
    }
    Ok(())
}

/// True when `moment` falls inside the department's working hours.
pub(crate) fn within_working_hours(hours: &WorkingHours, moment: DateTime<Utc>) -> bool {
    let local = moment + chrono::Duration::minutes(hours.utc_offset_minutes as i64);
    let weekday = local.weekday().num_days_from_sunday() as u8;
    if !hours.days.contains(&weekday) {
        return false;
    }
    let (Some(start), Some(end)) = (parse_hhmm(&hours.start), parse_hhmm(&hours.end)) else {
        return false;
    };
    let minute_of_day = local.hour() * 60 + local.minute();
    minute_of_day >= start && minute_of_day < end
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers: routing rules
// ---------------------------------------------------------------------------

pub(crate) fn valid_match_type(match_type: &str) -> bool {
    matches!(match_type, "category" | "product" | "keyword")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::departments::types::WorkingHours;

    fn hours(days: Vec<u8>, start: &str, end: &str, offset: i32) -> WorkingHours {
        WorkingHours {
            days,
            start: start.to_string(),
            end: end.to_string(),
            timezone: "UTC".to_string(),
            utc_offset_minutes: offset,
        }
    }

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("My Support Team"), "my-support-team");
        assert_eq!(slugify("A  B"), "a-b");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn valid_slug_rules() {
        assert!(valid_slug("support-team"));
        assert!(!valid_slug("Support"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a_b"));
    }

    #[test]
    fn valid_email_rules() {
        assert!(valid_email("agent@example.com"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@@b.com"));
        assert!(!valid_email("@b.com"));
        assert!(!valid_email("a@"));
        assert!(!valid_email("plain"));
    }

    #[test]
    fn parse_hhmm_converts_to_minutes() {
        assert_eq!(parse_hhmm("09:30"), Some(570));
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("09:60"), None);
        assert_eq!(parse_hhmm("no-colon"), None);
    }

    #[test]
    fn valid_working_hours_enforces_days_and_order() {
        assert!(valid_working_hours(&hours(vec![1, 2, 3, 4, 5], "09:00", "17:00", 0))
            .is_ok());
        assert!(valid_working_hours(&hours(vec![], "09:00", "17:00", 0)).is_err());
        assert!(valid_working_hours(&hours(vec![7], "09:00", "17:00", 0)).is_err());
        assert!(valid_working_hours(&hours(vec![1], "17:00", "09:00", 0)).is_err());
        assert!(valid_working_hours(&hours(vec![1], "bad", "17:00", 0)).is_err());
    }

    #[test]
    fn within_working_hours_checks_day_and_time() {
        let weekday = hours(vec![1], "09:00", "17:00", 0);
        // Monday 2026-11-23.
        assert!(within_working_hours(&weekday, at("2026-11-23T10:00:00Z")));
        assert!(!within_working_hours(&weekday, at("2026-11-23T08:00:00Z")));
        assert!(!within_working_hours(&weekday, at("2026-11-23T17:00:00Z")));
        // Tuesday 2026-11-24 is not a listed day.
        assert!(!within_working_hours(&weekday, at("2026-11-24T10:00:00Z")));
        // A +60 minute offset moves 08:30 UTC into 09:30 local time.
        let shifted = hours(vec![1], "09:00", "17:00", 60);
        assert!(within_working_hours(&shifted, at("2026-11-23T08:30:00Z")));
        assert!(!within_working_hours(&shifted, at("2026-11-23T07:30:00Z")));
    }
}
