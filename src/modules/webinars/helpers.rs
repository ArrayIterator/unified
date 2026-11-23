// webinars -- Actor, validation, view and scheduling helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    Ok(unified_core::caller::require_caller(req)?.user_id)
}

/// Roles of the server-verified caller, resolved by the auth
/// middleware (never a header).
pub(crate) fn caller_roles(req: &HttpRequest) -> Vec<String> {
    unified_core::caller::caller_from(req).roles
}

pub(crate) fn is_admin(roles: &[String]) -> bool {
    roles
        .iter()
        .any(|role| role == "admin" || role == "webinars.admin")
}

pub(crate) fn is_host(webinar: &Webinar, user_id: &str) -> bool {
    webinar.host_id == user_id
}

pub(crate) fn is_co_host(webinar: &Webinar, user_id: &str) -> bool {
    webinar.co_host_ids.iter().any(|id| id == user_id)
}

pub(crate) fn is_panelist(webinar: &Webinar, user_id: &str) -> bool {
    webinar.panelist_ids.iter().any(|id| id == user_id)
}

/// Host, co-host, or an admin caller. This is the management
/// capability: update, publish, cancel, start, end, link a room,
/// mute, approve/deny/promote registrations, manage polls and
/// recordings, send follow-up, read analytics.
pub(crate) fn can_manage(webinar: &Webinar, user_id: &str, roles: &[String]) -> bool {
    is_admin(roles) || is_host(webinar, user_id) || is_co_host(webinar, user_id)
}

/// Host, co-host, panelist, or admin: staff who may present (create
/// polls, answer questions) without holding a registration.
pub(crate) fn is_staff(webinar: &Webinar, user_id: &str, roles: &[String]) -> bool {
    can_manage(webinar, user_id, roles) || is_panelist(webinar, user_id)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn require_user_ids(values: &[String], field: &str) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in values {
        let id = require_text(raw, field, 128)?;
        if !cleaned.contains(&id) {
            cleaned.push(id);
        }
    }
    Ok(cleaned)
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
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

pub(crate) fn load_webinar(state: &WebinarsState, raw_id: &str) -> UnifiedResult<Webinar> {
    let id = parse_uuid(raw_id, "webinar")?;
    state
        .webinars
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Webinar not found: {id}")))
}

pub(crate) fn load_poll(
    state: &WebinarsState,
    webinar_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WebinarPoll> {
    let id = parse_uuid(raw_id, "poll")?;
    match state.polls.get(&id) {
        Some(entry) if entry.value().webinar_id == webinar_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!("Poll not found: {id}"))),
    }
}

pub(crate) fn load_question(
    state: &WebinarsState,
    webinar_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WebinarQuestion> {
    let id = parse_uuid(raw_id, "question")?;
    match state.questions.get(&id) {
        Some(entry) if entry.value().webinar_id == webinar_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!("Question not found: {id}"))),
    }
}

pub(crate) fn load_recording(
    state: &WebinarsState,
    webinar_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WebinarRecording> {
    let id = parse_uuid(raw_id, "recording")?;
    match state.recordings.get(&id) {
        Some(entry) if entry.value().webinar_id == webinar_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!(
            "Recording not found: {id}"
        ))),
    }
}

/// The caller's registration for one webinar, if any. When several
/// rows exist (a cancelled row plus a fresh one), the newest active
/// row wins; otherwise the newest row of any status.
pub(crate) fn find_registration(
    state: &WebinarsState,
    webinar_id: Uuid,
    user_id: &str,
) -> Option<WebinarRegistration> {
    let mut rows: Vec<WebinarRegistration> = state
        .registrations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.webinar_id == webinar_id && row.user_id == user_id)
        .collect();
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    rows.into_iter()
        .find(|row| {
            matches!(
                row.status.as_str(),
                "registered" | "waitlisted" | "pending_approval"
            )
        })
        .or_else(|| {
            state
                .registrations
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|row| row.webinar_id == webinar_id && row.user_id == user_id)
                .max_by(|a, b| a.created_at.cmp(&b.created_at))
        })
}

/// Registrations that currently hold a seat.
pub(crate) fn registered_count(state: &WebinarsState, webinar_id: Uuid) -> usize {
    state
        .registrations
        .iter()
        .filter(|entry| {
            entry.value().webinar_id == webinar_id && entry.value().status == "registered"
        })
        .count()
}

pub(crate) fn registration_counts(
    state: &WebinarsState,
    webinar_id: Uuid,
) -> (u64, u64, u64, u64, u64, u64) {
    let mut total = 0u64;
    let mut registered = 0u64;
    let mut waitlisted = 0u64;
    let mut pending = 0u64;
    let mut cancelled = 0u64;
    let mut denied = 0u64;
    for entry in state.registrations.iter() {
        let row = entry.value();
        if row.webinar_id != webinar_id {
            continue;
        }
        total += 1;
        match row.status.as_str() {
            "registered" => registered += 1,
            "waitlisted" => waitlisted += 1,
            "pending_approval" => pending += 1,
            "cancelled" => cancelled += 1,
            "denied" => denied += 1,
            _ => {}
        }
    }
    (total, registered, waitlisted, pending, cancelled, denied)
}

/// The caller's open attendance session for one webinar, if any.
pub(crate) fn open_session(
    state: &WebinarsState,
    webinar_id: Uuid,
    user_id: &str,
) -> Option<WebinarAttendance> {
    state
        .attendance
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| {
            row.webinar_id == webinar_id && row.user_id == user_id && row.left_at.is_none()
        })
        .max_by(|a, b| a.joined_at.cmp(&b.joined_at))
}

/// A timezone is an IANA name (`"Europe/London"`, `"UTC"`):
/// letters, digits, `_`, `-`, `+`, and `/` only, no empty segments.
/// This checks the shape only; no timezone conversion is done
/// anywhere (all stored times are UTC).
pub(crate) fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.split('/').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
    }) && timezone.contains('/')
}

/// A recurrence / schedule string, using the scheduler's own rules:
/// an interval in seconds, or five cron-like fields where each field
/// is `*`, `*/N`, or a plain number. An interval must be a real
/// delay: zero seconds would recur immediately and forever, and
/// intervals above one year are rejected as almost certainly a typo.
pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        return seconds >= 1 && seconds <= MAX_RECURRENCE_INTERVAL_SECS;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field.parse::<u32>().is_ok()
    })
}

pub(crate) fn validate_calendar(
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    timezone: &str,
    recurrence_rule: Option<&str>,
) -> UnifiedResult<()> {
    if !valid_timezone(timezone) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid timezone: {timezone}. Use an IANA name such as \"Europe/London\" or \"UTC\""
        )));
    }
    if let Some(rule) = recurrence_rule {
        if !valid_schedule(rule) {
            return Err(UnifiedError::bad_request(
                "recurrence_rule must be an interval in seconds or a cron-like expression such as \"0 9 * * 1\"",
            ));
        }
        if starts_at.is_none() {
            return Err(UnifiedError::bad_request(
                "recurrence_rule requires starts_at: a recurring webinar must have a first occurrence",
            ));
        }
    }
    match (starts_at, ends_at) {
        (Some(start), Some(end)) if end <= start => {
            Err(UnifiedError::bad_request("ends_at must be after starts_at"))
        }
        (None, Some(end)) if end <= Utc::now() => Err(UnifiedError::bad_request(
            "ends_at must be in the future for an instant webinar",
        )),
        (Some(start), None) if start <= Utc::now() => Err(UnifiedError::bad_request(
            "starts_at must be in the future for a scheduled webinar",
        )),
        _ => Ok(()),
    }
}

/// A recording / media URL: `http://` or `https://` with a
/// non-empty host and no whitespace. This is the check for URLs
/// that are actually consumed as links (recording `storage_url`);
/// it deliberately does not belong in `validate_calendar`, whose
/// fourth parameter is a recurrence rule, not a URL.
pub(crate) fn valid_url(raw: &str) -> bool {
    let url = raw.trim();
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return false;
    }
    let Some(after_scheme) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return false;
    }
    match host.split_once(':') {
        // Plain host, no port.
        None => true,
        // Host with an explicit port: the port must be numeric.
        Some((name, port)) => {
            !name.is_empty() && (port.is_empty() || port.parse::<u16>().is_ok())
        }
    }
}

/// Whether a scheduled webinar may go live right now. A webinar
/// can only start once its scheduled start time has arrived: the
/// publish path enforces a future `starts_at`, and this is the
/// matching guard on the start path, so a webinar cannot be started
/// arbitrarily early.
pub(crate) fn can_start_webinar(webinar: &Webinar, now: DateTime<Utc>) -> UnifiedResult<()> {
    if webinar.status != WebinarStatus::Scheduled {
        return Err(UnifiedError::unprocessable(
            "Only a scheduled webinar can be started; publish a draft first",
        ));
    }
    match webinar.starts_at {
        None => Err(UnifiedError::unprocessable(
            "A webinar without starts_at cannot be started; set a start time first",
        )),
        Some(starts_at) if now < starts_at => Err(UnifiedError::unprocessable(format!(
            "The webinar cannot start before its scheduled start time ({starts_at}); it is not yet time",
        ))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn validate_capacity(config: &WebinarConfig, capacity: usize) -> UnifiedResult<()> {
    if capacity < config.min_capacity || capacity > config.max_capacity {
        return Err(UnifiedError::bad_request(format!(
            "capacity must be between {} and {}",
            config.min_capacity, config.max_capacity
        )));
    }
    Ok(())
}

pub(crate) fn status_name(status: WebinarStatus) -> &'static str {
    match status {
        WebinarStatus::Draft => "draft",
        WebinarStatus::Scheduled => "scheduled",
        WebinarStatus::Live => "live",
        WebinarStatus::Ended => "ended",
        WebinarStatus::Cancelled => "cancelled",
    }
}

pub(crate) fn access_name(model: WebinarAccessModel) -> &'static str {
    match model {
        WebinarAccessModel::Free => "free",
        WebinarAccessModel::Paid => "paid",
    }
}

pub(crate) fn poll_view(state: &WebinarsState, poll: &WebinarPoll) -> PollView {
    let mut results: Vec<PollOptionResult> = poll
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| PollOptionResult {
            option_index: index,
            option: option.clone(),
            votes: 0,
            share: 0.0,
        })
        .collect();
    let mut voters: Vec<String> = Vec::new();
    let mut total_votes = 0u64;
    for entry in state.votes.iter() {
        let vote = entry.value();
        if vote.poll_id != poll.id {
            continue;
        }
        if !voters.contains(&vote.user_id) {
            voters.push(vote.user_id.clone());
        }
        for index in &vote.option_indexes {
            if let Some(result) = results.get_mut(*index) {
                result.votes += 1;
                total_votes += 1;
            }
        }
    }
    let unique_voters = voters.len() as u64;
    for result in results.iter_mut() {
        result.share = if unique_voters > 0 {
            result.votes as f64 / unique_voters as f64
        } else {
            0.0
        };
    }
    PollView {
        id: poll.id,
        webinar_id: poll.webinar_id,
        question: poll.question.clone(),
        options: poll.options.clone(),
        allow_multiple: poll.allow_multiple,
        status: poll.status.clone(),
        created_by: poll.created_by.clone(),
        created_at: poll.created_at,
        closed_at: poll.closed_at,
        total_votes,
        unique_voters,
        results,
    }
}

pub(crate) fn question_view(question: &WebinarQuestion, caller: &str) -> QuestionView {
    QuestionView {
        id: question.id,
        webinar_id: question.webinar_id,
        author_id: question.author_id.clone(),
        body: question.body.clone(),
        upvotes: question.upvoted_by.len() as u64,
        upvoted_by_caller: question.upvoted_by.iter().any(|id| id == caller),
        answered: question.answered,
        answered_by: question.answered_by.clone(),
        created_at: question.created_at,
        updated_at: question.updated_at,
    }
}

// Tests for the pure validation and naming helpers in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timezone_requires_region_and_city() {
        assert!(valid_timezone("UTC"));
        assert!(valid_timezone("Europe/London"));
        assert!(!valid_timezone(""));
        assert!(!valid_timezone("NoSlash"));
        assert!(!valid_timezone("A//B"));
    }

    #[test]
    fn schedule_accepts_seconds_or_cron() {
        assert!(valid_schedule("3600"));
        assert!(valid_schedule("0 9 * * 1"));
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("1 2 3"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn schedule_rejects_zero_and_huge_intervals() {
        // A zero-second recurrence would recur immediately and
        // forever; it is not a real interval.
        assert!(!valid_schedule("0"));
        assert!(!valid_schedule("00"));
        // One year is the cap; anything above it is a typo, not a
        // schedule. The cap itself is still accepted.
        assert!(valid_schedule("31536000"));
        assert!(!valid_schedule("31536001"));
        assert!(!valid_schedule("99999999999"));
        // The smallest real interval is one second.
        assert!(valid_schedule("1"));
    }

    #[test]
    fn recording_url_is_validated_as_url_not_recurrence() {
        assert!(valid_url("https://cdn.example.com/rec.mp4"));
        assert!(valid_url("http://cdn.example.com:8080/rec.mp4"));
        assert!(!valid_url("ftp://cdn.example.com/rec.mp4"));
        assert!(!valid_url("https://"));
        assert!(!valid_url("cdn.example.com/rec.mp4"));
        assert!(!valid_url("https://cdn.example.com/rec ord.mp4"));
    }

    fn test_webinar(status: WebinarStatus, starts_at: Option<DateTime<Utc>>) -> Webinar {
        let now = Utc::now();
        Webinar {
            id: Uuid::new_v4(),
            title: "Test webinar".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            co_host_ids: Vec::new(),
            panelist_ids: Vec::new(),
            status,
            access_model: WebinarAccessModel::Free,
            product_id: None,
            capacity: 10,
            requires_approval: false,
            allow_waitlist: true,
            chat_enabled: true,
            qa_enabled: true,
            polls_enabled: true,
            recording_enabled: true,
            meeting_room_id: None,
            meeting_link_status: "unlinked".to_string(),
            chat_channel_id: None,
            survey_id: None,
            survey_distribution_id: None,
            starts_at,
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            reminder_scheduler_status: "not_scheduled".to_string(),
            started_at: None,
            ended_at: None,
            follow_up_sent_at: None,
            follow_up_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn can_start_rejects_too_early_start() {
        let future = Utc::now() + chrono::Duration::hours(2);
        let webinar = test_webinar(WebinarStatus::Scheduled, Some(future));
        assert!(can_start_webinar(&webinar, Utc::now()).is_err());
    }

    #[test]
    fn can_start_accepts_start_time_reached() {
        let past = Utc::now() - chrono::Duration::minutes(1);
        let webinar = test_webinar(WebinarStatus::Scheduled, Some(past));
        assert!(can_start_webinar(&webinar, Utc::now()).is_ok());
        // Exactly at the start time is on time, not early.
        let at_start = test_webinar(WebinarStatus::Scheduled, Some(past));
        assert!(can_start_webinar(&at_start, past).is_ok());
    }

    #[test]
    fn can_start_rejects_wrong_status_and_missing_start() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let draft = test_webinar(WebinarStatus::Draft, Some(future));
        assert!(can_start_webinar(&draft, Utc::now()).is_err());
        let no_start = test_webinar(WebinarStatus::Scheduled, None);
        assert!(can_start_webinar(&no_start, Utc::now()).is_err());
    }

    #[test]
    fn status_names_are_stable() {
        assert_eq!(status_name(WebinarStatus::Draft), "draft");
        assert_eq!(status_name(WebinarStatus::Scheduled), "scheduled");
        assert_eq!(status_name(WebinarStatus::Live), "live");
        assert_eq!(status_name(WebinarStatus::Ended), "ended");
        assert_eq!(status_name(WebinarStatus::Cancelled), "cancelled");
    }

    #[test]
    fn access_names_are_stable() {
        assert_eq!(access_name(WebinarAccessModel::Free), "free");
        assert_eq!(access_name(WebinarAccessModel::Paid), "paid");
    }

    #[test]
    fn capacity_enforces_config_bounds() {
        let config = WebinarConfig::default();
        assert!(validate_capacity(&config, 0).is_err());
        assert!(validate_capacity(&config, 1).is_ok());
        assert!(validate_capacity(&config, 10_000).is_ok());
        assert!(validate_capacity(&config, 10_001).is_err());
    }

    #[test]
    fn calendar_accepts_empty_schedule() {
        assert!(validate_calendar(None, None, "UTC", None).is_ok());
    }

    #[test]
    fn calendar_accepts_real_recurrence_rule() {
        let start = Utc::now() + chrono::Duration::hours(1);
        assert!(validate_calendar(Some(start), None, "UTC", Some("3600")).is_ok());
        assert!(validate_calendar(Some(start), None, "UTC", Some("0 9 * * 1")).is_ok());
    }

    #[test]
    fn calendar_rejects_recording_url_as_recurrence_rule() {
        // The fourth parameter is a recurrence rule (seconds or a
        // cron-like expression), not a recording URL: a URL must be
        // rejected here, and validated where it is actually consumed
        // (the recording's storage_url) instead.
        let start = Utc::now() + chrono::Duration::hours(1);
        assert!(
            validate_calendar(
                Some(start),
                None,
                "UTC",
                Some("https://cdn.example.com/rec.mp4")
            )
            .is_err()
        );
        assert!(
            validate_calendar(None, None, "UTC", Some("ftp://cdn.example.com/rec.mp4")).is_err()
        );
    }

    #[test]
    fn calendar_rejects_past_start() {
        let past = Utc::now() - chrono::Duration::hours(1);
        assert!(validate_calendar(Some(past), None, "UTC", None).is_err());
    }

    #[test]
    fn calendar_rejects_end_before_start() {
        let start = Utc::now() + chrono::Duration::hours(2);
        let end = Utc::now() + chrono::Duration::hours(1);
        assert!(validate_calendar(Some(start), Some(end), "UTC", None).is_err());
    }

    #[test]
    fn calendar_rejects_bad_timezone_and_bad_recurrence() {
        assert!(validate_calendar(None, None, "Bogus", None).is_err());
        let start = Utc::now() + chrono::Duration::hours(1);
        assert!(validate_calendar(Some(start), None, "UTC", Some("not-a-schedule")).is_err());
        assert!(validate_calendar(Some(start), None, "UTC", Some("0")).is_err());
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group C).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Webinar (webinars, registrations, polls, questions, recordings) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
