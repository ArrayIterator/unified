// Scheduler - Job kind, schedule and pagination validation.

use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{KNOWN_KINDS};

// Helpers

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

/// Known job kinds as a comma-separated list for error messages.
pub(crate) fn known_kinds_list() -> String {
    KNOWN_KINDS.join(", ")
}

pub(crate) fn valid_kind(kind: &str) -> bool {
    KNOWN_KINDS.contains(&kind)
}

/// A schedule is either an interval in seconds or five cron-like fields
/// where each field is `*`, `*/N`, or a plain number. Intervals must be
/// greater than zero (a zero-second interval would fire continuously),
/// and every cron field must lie inside its real range: minute 0-59,
/// hour 0-23, day-of-month 1-31, month 1-12, weekday 0-7 (where 7 is
/// Sunday, the same day as 0).
pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        return seconds > 0;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields.iter().zip(RANGES).all(|(field, (min, max))| {
        valid_cron_field(field, min, max)
    })
}

fn valid_cron_field(field: &str, min: u32, max: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        return step.parse::<u32>().is_ok_and(|n| n > 0 && n <= max);
    }
    field
        .parse::<u32>()
        .is_ok_and(|n| (min..=max).contains(&n))
}

/// True when one cron field matches `value`: `*` matches anything,
/// `*/N` matches every Nth value counting from the field minimum,
/// and a plain number matches only itself. Weekday 7 is Sunday,
/// the same day as 0.
fn cron_field_matches(field: &str, value: u32, min: u32, max: u32) -> bool {
    if !valid_cron_field(field, min, max) {
        return false;
    }
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        let step: u32 = step.parse().expect("step validated above");
        return (value - min) % step == 0;
    }
    let number: u32 = field.parse().expect("number validated above");
    if max == 7 && number == 7 {
        // Cron weekday 7 is Sunday, the same day as 0.
        return value == 0;
    }
    number == value
}

/// True when a cron-like schedule fires at `at`. Interval schedules
/// (bare seconds) are not minute-anchored, so they never match here;
/// the ticker tests them by elapsed time instead (see `due_jobs`).
/// An invalid schedule never matches.
pub(crate) fn schedule_matches(schedule: &str, at: DateTime<Utc>) -> bool {
    use chrono::{Datelike, Timelike};
    let schedule = schedule.trim();
    if schedule.parse::<u64>().is_ok() {
        return false;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    let values = [
        at.minute(),
        at.hour(),
        at.day(),
        at.month(),
        at.weekday().num_days_from_sunday(),
    ];
    fields
        .iter()
        .zip(RANGES)
        .zip(values)
        .all(|((field, (min, max)), value)| cron_field_matches(field, value, min, max))
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::error::UnifiedError;

    fn params(page: u32, per_page: u32) -> PaginationParams {
        PaginationParams {
            page: Some(page),
            per_page: Some(per_page),
            ..Default::default()
        }
    }

    #[test]
    fn valid_kind_accepts_known_kinds() {
        assert!(valid_kind("mail.send"));
        assert!(valid_kind("backup.run"));
        // Scheduled workflows enqueue through this kind; the
        // scheduler must accept it or the workflow is declined.
        assert!(valid_kind("workflows.run"));
        // Contract expiry alerts enqueue through this kind; the
        // scheduler must accept it or the alert is declined.
        assert!(valid_kind("contracts.expiry_alert"));
        // Asset warranty checks enqueue through this kind; the
        // scheduler must accept it or the check is declined.
        assert!(valid_kind("assets.warranty.check"));
        // Inbox unsnooze jobs enqueue through this kind; the
        // scheduler must accept it or the snoozed thread never
        // re-appears on its own.
        assert!(valid_kind("inbox.unsnooze"));
    }

    #[test]
    fn valid_kind_rejects_unknown() {
        assert!(!valid_kind("anything.goes"));
    }

    #[test]
    fn known_kinds_list_is_not_empty_and_contains_mail_send() {
        let kinds = known_kinds_list();
        assert!(!kinds.is_empty());
        assert!(kinds.contains(&"mail.send".to_string()));
    }

    #[test]
    fn valid_schedule_accepts_interval_seconds() {
        // `valid_schedule` returns a bool, not a Result: a bare
        // number of seconds is the interval format the
        // implementation accepts (there is no "interval: N" form).
        assert!(valid_schedule("3600"));
        assert!(valid_schedule("300"));
    }

    #[test]
    fn valid_schedule_rejects_non_numeric_interval() {
        assert!(!valid_schedule("interval: fast"));
        assert!(!valid_schedule("fast"));
    }

    #[test]
    fn valid_schedule_rejects_zero_interval() {
        assert!(!valid_schedule("0"));
        assert!(!valid_schedule("interval: 0"));
    }

    #[test]
    fn valid_schedule_accepts_wildcard_cron() {
        assert!(valid_schedule("* * * * *"));
    }

    #[test]
    fn valid_schedule_accepts_step_cron() {
        assert!(valid_schedule("*/15 * * * *"));
    }

    #[test]
    fn valid_schedule_rejects_step_zero() {
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn valid_schedule_rejects_wrong_field_count() {
        assert!(!valid_schedule("* * *"));
    }

    #[test]
    fn valid_schedule_rejects_unknown_text() {
        assert!(!valid_schedule("every day at noon"));
    }

    #[test]
    fn valid_schedule_rejects_out_of_range_cron_fields() {
        // Minute 0-59, hour 0-23, day 1-31, month 1-12, weekday 0-7.
        assert!(!valid_schedule("60 * * * *"));
        assert!(!valid_schedule("* 24 * * *"));
        assert!(!valid_schedule("* * 0 * *"));
        assert!(!valid_schedule("* * 32 * *"));
        assert!(!valid_schedule("* * * 0 *"));
        assert!(!valid_schedule("* * * 13 *"));
        assert!(!valid_schedule("* * * * 8"));
        assert!(!valid_schedule("99 99 99 99 99"));
    }

    #[test]
    fn valid_schedule_accepts_cron_field_boundaries() {
        assert!(valid_schedule("59 23 31 12 7"));
        assert!(valid_schedule("0 0 1 1 0"));
        assert!(valid_schedule("*/59 * * * *"));
    }

    #[test]
    fn valid_schedule_rejects_step_above_field_range() {
        assert!(!valid_schedule("*/60 * * * *"));
        assert!(!valid_schedule("* */24 * * *"));
    }

    #[test]
    fn schedule_matches_wildcard_cron_at_any_time() {
        let at = Utc::now();
        assert!(schedule_matches("* * * * *", at));
        assert!(!schedule_matches("300", at));
        assert!(!schedule_matches("not a schedule", at));
    }

    #[test]
    fn schedule_matches_exact_minute_and_hour() {
        use chrono::Timelike;
        let at = Utc::now();
        let schedule = format!("{} {} * * *", at.minute(), at.hour());
        assert!(schedule_matches(&schedule, at));
        let other_minute = (at.minute() + 1) % 60;
        let schedule = format!("{other_minute} {} * * *", at.hour());
        assert!(!schedule_matches(&schedule, at));
    }

    #[test]
    fn schedule_matches_step_cron() {
        use chrono::Timelike;
        let at = Utc::now();
        // */15 matches only when the current minute is a multiple
        // of 15; the test asserts the rule either way.
        assert_eq!(
            schedule_matches("*/15 * * * *", at),
            at.minute() % 15 == 0
        );
    }

    #[test]
    fn new_id_is_a_valid_uuid() {
        let id = new_id();
        assert!(Uuid::parse_str(&id).is_ok());
        assert_ne!(new_id(), id);
    }

    #[test]
    fn paginate_slices_items() {
        let page = paginate(vec![1, 2, 3, 4], &params(2, 2));
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total_pages, 2);
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        // `matches_search` takes pagination params, not a bare
        // needle: the search text rides on `PaginationParams.search`.
        let backup = PaginationParams {
            search: Some("backup".to_string()),
            ..Default::default()
        };
        let mail = PaginationParams {
            search: Some("mail".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Nightly Backup", &backup));
        assert!(!matches_search("Nightly Backup", &mail));
    }

    #[test]
    fn respond_created_returns_201_on_success() {
        let response = respond_created(Ok(42));
        assert_eq!(response.status().as_u16(), 201);
    }

    #[test]
    fn respond_created_returns_error_status_on_failure() {
        let response = respond_created(Err(UnifiedError::bad_request("bad")));
        assert_eq!(response.status().as_u16(), 400);
    }
}
