//! Input validation and URL safety checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::StorageProvider;
use crate::http::websocket::global_hub;
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`, `"EST"`):
/// letters, digits, `_`, `-`, `+`, and `/` only, no empty segments.
/// Single-segment names (`EST`, `CET`, `GMT`) are valid IANA names
/// too. This checks the shape only; no timezone conversion is done
/// anywhere (all stored times are UTC).
fn valid_timezone(timezone: &str) -> bool {
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
    })
}

/// A recurrence / schedule string, using the scheduler's own rules
/// (`scheduler.rs` `valid_schedule`): an interval in seconds, or five
/// cron-like fields where each field is `*`, `*/N`, or a plain
/// number. Kept in sync with the scheduler by hand; if the scheduler
/// accepts a new schedule shape, this must accept it too.
///
/// Every numeric field is range-checked (minute 0-59, hour 0-23,
/// day of month 1-31, month 1-12, weekday 0-7); an interval of 0 is
/// rejected. Nonsense such as `99 99 99 99 99` or `0` is not a
/// schedule.
fn valid_schedule(schedule: &str) -> bool {
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
    // Per-field (minimum, maximum): minute, hour, day of month,
    // month, weekday (0-7, where 7 is Sunday).
    const BOUNDS: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields.iter().enumerate().all(|(index, field)| {
        let (min, max) = BOUNDS[index];
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0 && n <= max);
        }
        field
            .parse::<u32>()
            .is_ok_and(|n| n >= min && n <= max)
    })
}

pub(crate) fn validate_calendar(
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    timezone: &str,
    recurrence_rule: Option<&str>,
) -> UnifiedResult<()> {
    validate_calendar_with(starts_at, ends_at, timezone, recurrence_rule, false)
}

/// Calendar validation for room updates. Identical to
/// `validate_calendar`, except `allow_past_start` skips the
/// past-start rule: an update must not be rejected only because the
/// room already started (or because the caller did not touch
/// `starts_at` at all and the stored start is now in the past).
/// A caller-supplied new start in the past is still rejected unless
/// the room already started.
pub(crate) fn validate_calendar_with(
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    timezone: &str,
    recurrence_rule: Option<&str>,
    allow_past_start: bool,
) -> UnifiedResult<()> {
    if !valid_timezone(timezone) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid timezone: {timezone}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
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
                "recurrence_rule requires starts_at: a recurring meeting must have a first occurrence",
            ));
        }
    }
    match (starts_at, ends_at) {
        (Some(start), Some(end)) if end <= start => Err(UnifiedError::bad_request(
            "ends_at must be after starts_at",
        )),
        (None, Some(end)) if end <= Utc::now() => Err(UnifiedError::bad_request(
            "ends_at must be in the future for an instant meeting",
        )),
        (Some(start), None) if start <= Utc::now() && !allow_past_start => Err(
            UnifiedError::bad_request("starts_at must be in the future for a scheduled meeting"),
        ),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Entitlement (meetings as a product)
// ---------------------------------------------------------------------------

/// Check, at join time, whether `user_id` may enter a paid room.
///
/// Free rooms (`product_id` is `None`) skip the check entirely. The
/// room host skips it too, as does a caller whose `role` slug holds
/// the `meetings.host` permission according to the roles module
/// (asked through the `role.check` hook by `has_host_override`).
/// The role slug comes from the `X-Role` header and is only an
/// identifier for that hook: a client claiming `admin` with no hook
/// grant does not bypass anything. Otherwise the store / product
/// module is asked through the hook event
/// `product.entitlement.check` with payload
/// `{"product_id": ..., "user_id": ...}`, and the first hook result
/// must carry `{"entitled": true}`.
///
/// Fail-closed on purpose: when no handler answers the event (no
/// product module registered yet, or the store has not wired the
/// check), joining a paid room answers 503 instead of letting
/// someone in for free. An explicit `{"entitled": false}` (or an
/// `error` in the result) answers 403.
pub(crate) fn check_entitlement(
    state: &MeetingsState,
    room: &MeetingRoom,
    user_id: &str,
    role: Option<&str>,
) -> UnifiedResult<()> {
    let Some(product_id) = &room.product_id else {
        return Ok(());
    };
    if has_host_override(state, room, user_id, role) {
        return Ok(());
    }
    let Some(bus) = state.bus() else {
        return Err(UnifiedError::new(
            "Entitlement service is unavailable: no hook bus is attached",
            503,
        ));
    };
    let answer = bus.emit(
        "product.entitlement.check",
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(
            "A paid product entitlement is required to join this meeting room",
        )),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}

// ---------------------------------------------------------------------------
// Scheduler linkage
// ---------------------------------------------------------------------------

/// Link a scheduled room to the scheduler module.
///
/// The scheduler listens to the `scheduler.enqueue` event with the
/// shape `{"kind", "payload"}` (see `scheduler.rs`
/// `on_scheduler_enqueue`). A meeting start is queued as kind
/// `meeting.start` with the room id, meeting id, start time, and
/// host id in the payload. The result is read back from the first
/// hook result: `{"queued": true, "job_id": ...}` means the scheduler
/// accepted the job.
///
/// `meeting.start` is a known scheduler kind: it is listed in the
/// scheduler's `KNOWN_KINDS` (`scheduler/types.rs`) and dispatched
/// at `scheduler/service.rs`, which emits `meeting.start_requested`
/// when the job fires — the event this module already listens to
/// below. When the scheduler is not registered or declines the job,
/// this function records `scheduler_status: "declined"` instead of
/// pretending the reminder was scheduled. The room is still created
/// and stays usable. Rooms without `starts_at` are never queued and
/// keep `scheduler_status: "not_scheduled"`.
pub(crate) fn schedule_meeting(state: &MeetingsState, room: &mut MeetingRoom) {
    let Some(starts_at) = room.starts_at else {
        room.scheduler_job_id = None;
        room.scheduler_status = "not_scheduled".to_string();
        return;
    };
    let Some(bus) = state.bus() else {
        room.scheduler_job_id = None;
        room.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "meeting.start",
            "payload": {
                "meeting_id": room.id.to_string(),
                "room_id": room.id.to_string(),
                "starts_at": starts_at.to_rfc3339(),
                "host_id": room.host_id,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            room.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            room.scheduler_status = "queued".to_string();
        }
        _ => {
            room.scheduler_job_id = None;
            room.scheduler_status = "declined".to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_timezone_accepts_utc_and_iana() {
        assert!(valid_timezone("UTC"));
        assert!(valid_timezone("Asia/Jakarta"));
        assert!(valid_timezone("America/New_York"));
    }

    #[test]
    fn valid_timezone_accepts_single_segment_iana_names() {
        assert!(valid_timezone("EST"));
        assert!(valid_timezone("CET"));
        assert!(valid_timezone("GMT"));
        assert!(valid_timezone("Jakarta"));
    }

    #[test]
    fn valid_timezone_rejects_bad() {
        assert!(!valid_timezone(""));
        assert!(!valid_timezone("Asia/"));
        assert!(!valid_timezone("Asia//Jakarta"));
        assert!(!valid_timezone("Asia/Jak arta"));
    }

    #[test]
    fn valid_schedule_accepts_interval_and_cron() {
        assert!(valid_schedule("604800"));
        assert!(valid_schedule("0 9 * * 1"));
        assert!(valid_schedule("*/15 * * * *"));
        assert!(valid_schedule("59 23 31 12 7"));
    }

    #[test]
    fn valid_schedule_rejects_bad() {
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("abc"));
        assert!(!valid_schedule("0 9 * *"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn valid_schedule_rejects_out_of_range_fields_and_zero_interval() {
        assert!(!valid_schedule("0"));
        assert!(!valid_schedule("99 99 99 99 99"));
        assert!(!valid_schedule("60 * * * *"));
        assert!(!valid_schedule("0 24 * * *"));
        assert!(!valid_schedule("0 0 32 * *"));
        assert!(!valid_schedule("0 0 0 * *"));
        assert!(!valid_schedule("0 0 * 13 *"));
        assert!(!valid_schedule("0 0 * 0 *"));
        assert!(!valid_schedule("0 0 * * 8"));
        assert!(!valid_schedule("*/60 * * * *"));
    }

    #[test]
    fn validate_calendar_accepts_valid() {
        let start = Utc::now() + chrono::Duration::hours(2);
        let end = Utc::now() + chrono::Duration::hours(3);
        assert!(validate_calendar(Some(start), Some(end), "Asia/Jakarta", None).is_ok());
        assert!(validate_calendar(None, None, "UTC", None).is_ok());
    }

    #[test]
    fn validate_calendar_rejects_bad_timezone() {
        // "Jakarta" is a valid single-segment IANA name (see
        // valid_timezone_accepts_single_segment_iana_names), so a
        // bad timezone needs a genuinely malformed value.
        assert!(validate_calendar(None, None, "Asia/Jak arta", None).is_err());
        assert!(validate_calendar(None, None, "Asia//Jakarta", None).is_err());
    }

    #[test]
    fn validate_calendar_rejects_end_before_start() {
        let start = Utc::now() + chrono::Duration::hours(3);
        let end = Utc::now() + chrono::Duration::hours(2);
        assert!(validate_calendar(Some(start), Some(end), "UTC", None).is_err());
    }

    #[test]
    fn validate_calendar_rejects_recurrence_without_start_and_past_start() {
        assert!(validate_calendar(None, None, "UTC", Some("0 9 * * 1")).is_err());
        let past = Utc::now() - chrono::Duration::hours(1);
        assert!(validate_calendar(Some(past), None, "UTC", None).is_err());
    }

    #[test]
    fn validate_calendar_with_allows_past_start_for_started_rooms() {
        let past = Utc::now() - chrono::Duration::hours(1);
        assert!(validate_calendar_with(Some(past), None, "UTC", None, false).is_err());
        assert!(validate_calendar_with(Some(past), None, "UTC", None, true).is_ok());
        // The other rules still apply when the past-start rule is skipped.
        let end_before_start = Utc::now() - chrono::Duration::hours(2);
        assert!(
            validate_calendar_with(Some(past), Some(end_before_start), "UTC", None, true)
                .is_err()
        );
    }

    fn test_room_with_product(host_id: &str) -> MeetingRoom {
        MeetingRoom {
            id: Uuid::new_v4(),
            title: "Paid room".to_string(),
            description: None,
            host_id: host_id.to_string(),
            status: RoomStatus::Scheduled,
            access_model: AccessModel::Room,
            product_id: Some("prod-1".to_string()),
            max_participants: 10,
            waiting_room_enabled: false,
            join_token: "token".to_string(),
            starts_at: Some(Utc::now() + chrono::Duration::hours(2)),
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at: None,
            ended_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn state_with_hooks(
        role_allowed: bool,
        entitled: bool,
    ) -> MeetingsState {
        let state = MeetingsState::default();
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.set("allowed", Value::Bool(role_allowed));
            }),
        );
        bus.register(
            "product.entitlement.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.add_result(json!({ "entitled": entitled }));
            }),
        );
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        state
    }

    #[test]
    fn check_entitlement_does_not_trust_header_role() {
        let room = test_room_with_product("host-1");
        // The roles module denies the permission, the store denies
        // the entitlement: a caller claiming role "admin" is refused.
        let denied = state_with_hooks(false, false);
        let error = check_entitlement(&denied, &room, "intruder", Some("admin"))
            .expect_err("claimed admin role must not bypass a paid room");
        assert_eq!(error.code, 403);
        // The same caller is admitted when the store grants the
        // entitlement: the paid path itself still works.
        let entitled = state_with_hooks(false, true);
        assert!(check_entitlement(&entitled, &room, "buyer", Some("admin")).is_ok());
    }

    #[test]
    fn check_entitlement_override_comes_from_role_check_hook() {
        let room = test_room_with_product("host-1");
        let granting = state_with_hooks(true, false);
        assert!(
            check_entitlement(&granting, &room, "helper", Some("admin")).is_ok(),
            "a role.check grant for meetings.host bypasses the paid check"
        );
        // The host bypasses without any hook involvement.
        let no_bus = MeetingsState::default();
        assert!(check_entitlement(&no_bus, &room, "host-1", None).is_ok());
    }

    #[test]
    fn schedule_meeting_queues_meeting_start_kind() {
        let mut room = test_room_with_product("host-1");
        let state = MeetingsState::default();
        let bus = Arc::new(HookBus::new());
        bus.register(
            "scheduler.enqueue",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let kind = ctx
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                assert_eq!(kind, "meeting.start");
                ctx.add_result(json!({ "queued": true, "job_id": "job-1" }));
            }),
        );
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        schedule_meeting(&state, &mut room);
        assert_eq!(room.scheduler_status, "queued");
        assert_eq!(room.scheduler_job_id.as_deref(), Some("job-1"));
    }

    #[test]
    fn schedule_meeting_declines_without_bus() {
        let mut room = test_room_with_product("host-1");
        let state = MeetingsState::default();
        schedule_meeting(&state, &mut room);
        assert_eq!(room.scheduler_status, "declined");
        assert_eq!(room.scheduler_job_id, None);
    }
}
