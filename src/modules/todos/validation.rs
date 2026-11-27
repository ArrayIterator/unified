// todos - validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::access::*;
use super::service::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. This
/// checks the shape only; no timezone conversion is done anywhere
/// (all stored times are UTC).
pub(crate) fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone
        .split('/')
        .all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
        && timezone.contains('/')
}

pub(crate) fn validate_color(color: &str) -> UnifiedResult<String> {
    let value = color.trim().to_lowercase();
    if LIST_COLORS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "color must be one of: {}",
            LIST_COLORS.join(", ")
        )))
    }
}

pub(crate) fn validate_icon(icon: &str) -> UnifiedResult<String> {
    let value = icon.trim().to_lowercase();
    if LIST_ICONS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "icon must be one of: {}",
            LIST_ICONS.join(", ")
        )))
    }
}

pub(crate) fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > MAX_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_TAGS} tags are allowed"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() {
            return Err(UnifiedError::bad_request("Tags must not be empty"));
        }
        if tag.chars().count() > MAX_TAG_LEN {
            return Err(UnifiedError::bad_request(format!(
                "Each tag must be at most {MAX_TAG_LEN} characters long"
            )));
        }
        if !cleaned.contains(&tag) {
            cleaned.push(tag);
        }
    }
    Ok(cleaned)
}

pub(crate) fn validate_reminder_offset(offset: i64) -> UnifiedResult<i64> {
    if offset < 0 || offset > MAX_REMINDER_OFFSET_MINUTES {
        return Err(UnifiedError::bad_request(format!(
            "offset_minutes must be between 0 and {MAX_REMINDER_OFFSET_MINUTES}"
        )));
    }
    Ok(offset)
}

pub(crate) fn validate_recurrence(
    frequency: Option<RecurrenceFrequency>,
    interval: u32,
    count: Option<u32>,
    until: Option<DateTime<Utc>>,
    due_at: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if frequency.is_none() {
        if interval != 1 || count.is_some() || until.is_some() {
            return Err(UnifiedError::bad_request(
                "recurrence_interval, recurrence_count, and recurrence_until require recurrence_frequency",
            ));
        }
        return Ok(());
    }
    if interval == 0 || interval > 365 {
        return Err(UnifiedError::bad_request(
            "recurrence_interval must be between 1 and 365",
        ));
    }
    if let Some(count) = count {
        if count == 0 || count > 366 {
            return Err(UnifiedError::bad_request(
                "recurrence_count must be between 1 and 366",
            ));
        }
    }
    if count.is_some() && until.is_some() {
        return Err(UnifiedError::bad_request(
            "recurrence_count and recurrence_until are mutually exclusive: set at most one",
        ));
    }
    if due_at.is_none() {
        return Err(UnifiedError::bad_request(
            "A recurring todo requires due_at: the next occurrence is computed from it",
        ));
    }
    if let (Some(until), Some(due)) = (until, due_at) {
        if until <= due {
            return Err(UnifiedError::bad_request(
                "recurrence_until must be after due_at",
            ));
        }
    }
    Ok(())
}

/// Advance a due time by one recurrence step. Monthly steps clamp
/// the day to the target month's length (31 January -> 28/29
/// February), computed by hand so no extra date crate is needed.
pub(crate) fn advance_due(
    due: DateTime<Utc>,
    frequency: RecurrenceFrequency,
    interval: u32,
) -> DateTime<Utc> {
    match frequency {
        RecurrenceFrequency::Daily => due + chrono::Duration::days(i64::from(interval)),
        RecurrenceFrequency::Weekly => due + chrono::Duration::days(i64::from(interval) * 7),
        RecurrenceFrequency::Monthly => {
            let total_months = due.month() as i64 + i64::from(interval) - 1;
            let year = due.year() + (total_months.div_euclid(12)) as i32;
            let month = (total_months.rem_euclid(12) + 1) as u32;
            let last_day = last_day_of_month(year, month);
            let day = due.day().min(last_day);
            let date = NaiveDate::from_ymd_opt(year, month, day)
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month, last_day).expect("valid month end"));
            let time = due.time();
            Utc.from_utc_datetime(&date.and_time(time))
        }
    }
}

pub(crate) fn last_day_of_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn colors_and_icons_accept_only_the_allow_list() {
        assert_eq!(validate_color("Blue").unwrap(), "blue");
        assert_eq!(validate_color("gray").unwrap(), "gray");
        assert!(validate_color("magenta").is_err());
        assert_eq!(validate_icon("STAR").unwrap(), "star");
        assert!(validate_icon("rocket").is_err());
    }

    #[test]
    fn tags_are_cleaned_deduplicated_and_capped() {
        assert_eq!(
            validate_tags(&[
                "Work".to_string(),
                " work ".to_string(),
                "Home".to_string()
            ])
            .unwrap(),
            vec!["work".to_string(), "home".to_string()]
        );
        assert!(validate_tags(&["".to_string()]).is_err());
        assert!(validate_tags(&["x".repeat(65)]).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("tag{i}")).collect();
        assert!(validate_tags(&many).is_err());
    }

    #[test]
    fn reminder_offsets_stay_within_thirty_days() {
        assert_eq!(validate_reminder_offset(0).unwrap(), 0);
        assert_eq!(validate_reminder_offset(43_200).unwrap(), 43_200);
        assert!(validate_reminder_offset(-1).is_err());
        assert!(validate_reminder_offset(43_201).is_err());
    }

    #[test]
    fn recurrence_rules_reject_inconsistent_combinations() {
        assert!(validate_recurrence(None, 1, None, None, None).is_ok());
        assert!(validate_recurrence(None, 2, None, None, None).is_err());
        assert!(validate_recurrence(
            Some(RecurrenceFrequency::Daily),
            0,
            None,
            None,
            Some(utc(2026, 11, 22))
        )
        .is_err());
        assert!(
            validate_recurrence(Some(RecurrenceFrequency::Weekly), 1, None, None, None).is_err()
        );
        assert!(validate_recurrence(
            Some(RecurrenceFrequency::Monthly),
            2,
            Some(3),
            None,
            Some(utc(2026, 11, 22))
        )
        .is_ok());
        assert!(validate_recurrence(
            Some(RecurrenceFrequency::Monthly),
            1,
            Some(3),
            Some(utc(2026, 12, 22)),
            Some(utc(2026, 11, 22))
        )
        .is_err());
    }

    #[test]
    fn due_dates_advance_by_frequency() {
        let due = utc(2026, 11, 22);
        assert_eq!(
            advance_due(due, RecurrenceFrequency::Daily, 2),
            utc(2026, 11, 24)
        );
        assert_eq!(
            advance_due(due, RecurrenceFrequency::Weekly, 2),
            utc(2026, 12, 6)
        );
        // January 31 plus one month clamps to the end of February.
        assert_eq!(
            advance_due(utc(2026, 1, 31), RecurrenceFrequency::Monthly, 1),
            utc(2026, 2, 28)
        );
        assert_eq!(
            advance_due(utc(2026, 11, 22), RecurrenceFrequency::Monthly, 2),
            utc(2027, 1, 22)
        );
    }

    #[test]
    fn month_lengths_follow_the_leap_year_rule() {
        assert_eq!(last_day_of_month(2026, 2), 28);
        assert_eq!(last_day_of_month(2024, 2), 29);
        assert_eq!(last_day_of_month(1900, 2), 28);
        assert_eq!(last_day_of_month(2000, 2), 29);
        assert_eq!(last_day_of_month(2026, 4), 30);
        assert_eq!(last_day_of_month(2026, 12), 31);
        assert_eq!(last_day_of_month(2026, 13), 30);
    }

    #[test]
    fn timezones_must_look_like_region_city() {
        assert!(valid_timezone("UTC"));
        assert!(valid_timezone("Asia/Jakarta"));
        assert!(valid_timezone("America/New_York"));
        assert!(!valid_timezone(""));
        assert!(!valid_timezone("Jakarta"));
        assert!(!valid_timezone("Asia/"));
    }
}
