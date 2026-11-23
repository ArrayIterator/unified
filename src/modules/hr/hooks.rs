//! Hook-bus handlers: answering requests from other modules.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Consume TimeTracker entries and record one attendance row per entry.
/// Accepts BOTH payload shapes (see the module header):
///   - documented: `{user_id, date, minutes, source}`
///   - already in this tree (`timetracker.time.logged`):
///     `{entry_id, user_id, started_at, ended_at, duration_secs,
///     hours, source, ...}`
/// Unknown payloads (missing user id, unparsable date, zero minutes)
/// and users with no employee record are ignored silently: a
/// TimeTracker event must never fail HR, and attendance for a user
/// who is not an employee is not HR data.
/// Answer `user.manager.query` with the manager of the given user.
///
/// The approvals module asks for `{"user_id": "..."}` and reads the
/// `manager_id` key from the answer, comparing it against the
/// deciding caller's user id — so the answer must be the manager's
/// USER id, not the manager's employee id. The employee record
/// stores `manager_id` as an employee id, so this handler resolves
/// it one step further: employee (by `user_id`) -> manager employee
/// (by `manager_id`) -> that employee's `user_id`.
///
/// When HR holds no employee for the user, no manager is recorded,
/// or the manager's employee record is missing, the handler answers
/// `resolved: false` with `manager_id: null` and a reason. It never
/// fabricates a manager and never guesses: an unresolved answer
/// leaves the approvals slot unresolved for admin reassignment.
pub(crate) fn on_user_manager_query(state: Arc<HrState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let user_id = data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let Some(user_id) = user_id else {
        return;
    };
    let answer = |resolved: bool, manager_id: Option<String>, reason: Option<&str>| {
        let mut answer = json!({
            "user_id": user_id,
            "resolved": resolved,
            "manager_id": manager_id,
        });
        if let Some(reason) = reason {
            answer["reason"] = json!(reason);
        }
        answer
    };
    let Some(employee) = state.employee_by_user(&user_id) else {
        let answer = answer(false, None, Some("no employee record for user"));
        ctx.set("resolved", json!(false));
        ctx.set("manager_id", Value::Null);
        ctx.add_result(answer);
        return;
    };
    let Some(manager_employee_id) = employee.manager_id.clone() else {
        let answer = answer(false, None, Some("no manager recorded for employee"));
        ctx.set("resolved", json!(false));
        ctx.set("manager_id", Value::Null);
        ctx.add_result(answer);
        return;
    };
    let Some(manager) = state
        .employees
        .get(manager_employee_id.as_str())
        .map(|entry| entry.value().clone())
    else {
        let answer = answer(
            false,
            None,
            Some("manager employee record not found"),
        );
        ctx.set("resolved", json!(false));
        ctx.set("manager_id", Value::Null);
        ctx.add_result(answer);
        return;
    };
    let answer = answer(true, Some(manager.user_id.clone()), None);
    ctx.set("resolved", json!(true));
    ctx.set("manager_id", json!(manager.user_id));
    ctx.add_result(answer);
}

pub(crate) fn on_timetracker_entry(state: Arc<HrState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let user_id = user_id.to_string();
    let work_date = match data.get("date").and_then(|v| v.as_str()) {
        Some(raw) => match NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => return,
        },
        None => {
            // Fall back to the date part of `started_at` (RFC 3339),
            // then `ended_at`. Parsed, never byte-sliced: slicing
            // the first 10 bytes panics when the 10th byte falls
            // inside a multibyte character.
            let timestamp = data
                .get("started_at")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("ended_at").and_then(|v| v.as_str()));
            let Some(timestamp) = timestamp else {
                return;
            };
            let Some(work_date) = timetracker_entry_date(timestamp) else {
                return;
            };
            work_date
        }
    };
    let minutes: u64 = if let Some(minutes) = data.get("minutes").and_then(|v| v.as_u64()) {
        minutes
    } else if let Some(secs) = data.get("duration_secs").and_then(|v| v.as_u64()) {
        secs.div_ceil(60)
    } else if let Some(hours) = data.get("hours").and_then(|v| v.as_f64()) {
        (hours * 60.0).round() as u64
    } else {
        return;
    };
    if minutes == 0 || minutes > MAX_MINUTES_PER_DAY {
        return;
    }
    let source = data
        .get("source")
        .and_then(|v| v.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("timetracker")
        .to_string();
    let entry_id = data
        .get("entry_id")
        .and_then(|v| v.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(employee) = state.employee_by_user(&user_id) else {
        return;
    };
    // De-duplicate repeated delivery of the same TimeTracker entry.
    if let Some(entry_id) = &entry_id {
        if state
            .attendance
            .iter()
            .any(|entry| entry.value().entry_id.as_deref() == Some(entry_id.as_str()))
        {
            return;
        }
    } else if state.attendance.iter().any(|entry| {
        let record = entry.value();
        record.employee_id == employee.id && record.work_date == work_date
    }) {
        // Without an entry id there is no per-entry key, so a
        // repeated delivery is recognised by (employee, date): one
        // attendance row per employee per day from this hook.
        return;
    }
    let record = AttendanceRecord {
        id: new_id(),
        employee_id: employee.id.clone(),
        user_id: employee.user_id,
        work_date,
        minutes_worked: minutes,
        source,
        entry_id,
        created_at: now(),
        updated_at: now(),
    };
    state.attendance.insert(record.id.clone(), record);
}

/// The work date of a TimeTracker timestamp (RFC 3339 `started_at`
/// / `ended_at`, or a plain `YYYY-MM-DD` date). Parses instead of
/// byte-slicing the first 10 bytes: slicing panics when the 10th
/// byte falls inside a multibyte character.
pub(crate) fn timetracker_entry_date(timestamp: &str) -> Option<NaiveDate> {
    let timestamp = timestamp.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(timestamp) {
        return Some(parsed.date_naive());
    }
    let prefix: String = timestamp.chars().take(10).collect();
    NaiveDate::parse_from_str(&prefix, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn employee() -> Employee {
        Employee {
            id: "emp-1".to_string(),
            user_id: "user-1".to_string(),
            employee_code: "EMP-1".to_string(),
            position_id: None,
            department_id: None,
            department_verified: false,
            manager_id: None,
            status: "active".to_string(),
            hire_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn entry_event(data: Value) -> HookContext {
        HookContext::new("timetracker.time.logged", data)
    }

    #[test]
    fn timetracker_entry_date_parses_rfc3339_and_plain_dates() {
        assert_eq!(
            timetracker_entry_date("2026-11-22T10:00:00+07:00"),
            Some(NaiveDate::from_ymd_opt(2026, 11, 22).unwrap())
        );
        assert_eq!(
            timetracker_entry_date("2026-11-22"),
            Some(NaiveDate::from_ymd_opt(2026, 11, 22).unwrap())
        );
        assert_eq!(timetracker_entry_date("not-a-date"), None);
    }

    #[test]
    fn timetracker_entry_date_survives_multibyte_timestamp() {
        // Full-width digits: byte-slicing the first 10 bytes would
        // panic mid-character. Parsing must return None, not panic.
        assert_eq!(timetracker_entry_date("２０２６-11-22T10:00:00Z"), None);
        assert_eq!(timetracker_entry_date("éééé-11-22"), None);
    }

    #[test]
    fn timetracker_entry_without_id_dedups_by_employee_and_date() {
        let state = Arc::new(HrState::new());
        state.employees.insert("emp-1".to_string(), employee());
        let payload = json!({
            "user_id": "user-1",
            "started_at": "2026-11-22T09:00:00Z",
            "duration_secs": 3600,
        });
        on_timetracker_entry(state.clone(), &mut entry_event(payload.clone()));
        on_timetracker_entry(state.clone(), &mut entry_event(payload));
        assert_eq!(state.attendance.len(), 1);
    }

    #[test]
    fn timetracker_entry_with_id_still_dedups_by_entry_id() {
        let state = Arc::new(HrState::new());
        state.employees.insert("emp-1".to_string(), employee());
        let payload = json!({
            "user_id": "user-1",
            "entry_id": "entry-1",
            "started_at": "2026-11-22T09:00:00Z",
            "duration_secs": 3600,
        });
        on_timetracker_entry(state.clone(), &mut entry_event(payload.clone()));
        on_timetracker_entry(state.clone(), &mut entry_event(payload));
        assert_eq!(state.attendance.len(), 1);
    }
}
