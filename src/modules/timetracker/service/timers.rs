// service — timers.
// Timer lifecycle: start/stop bookkeeping shared by handlers.
//
// Split from `src/modules/timetracker/service.rs`; moved verbatim, no behaviour changes.

use super::super::*;
use super::payroll::active_rate_for;
use super::*;
use actix_web::HttpRequest;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::StorageRegistry;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Core logic: entries, timers, purge, payroll, reports
// ---------------------------------------------------------------------------

/// Emit `timetracker.time.logged` for one entry.
///
/// Payload shape (the billing module reads this to price billable
/// hours; the integrator adds a `timetracker.time.logged` handler
/// in billing.rs — billing currently listens to `order.created`,
/// `payment.captured`, and `payment.refunded` only):
/// ```json
/// {
///   "entry_id": "<uuid>", "user_id": "<user>", "timer_id": "<uuid>|null",
///   "project_id": "<uuid>|null", "task_id": "<uuid>|null",
///   "description": "<text>", "started_at": "<rfc3339>",
///   "ended_at": "<rfc3339>", "duration_secs": 3600,
///   "hours": 1.0, "billable": true, "approval": "pending",
///   "currency": "USD", "hourly_rate": 25.0,
///   "amount": 25.0, "source": "timer|manual|manual_edit|manual_delete",
///   "void": false,
///   "project_time_entry": { "id": "<uuid>", "task_id": "<uuid>|null",
///     "project_id": "<uuid>|null", "user_id": "<user>",
///     "description": "<text>", "hours": 1.0, "billable": true,
///     "billed": false, "invoice_id": null,
///     "started_at": "<rfc3339>", "ended_at": "<rfc3339>",
///     "timer_running": false, "created_at": "<rfc3339>",
///     "updated_at": "<rfc3339>" }
/// }
/// ```
/// `hours` is `duration_secs / 3600`. `hourly_rate` is the user's
/// active rate at the entry's end (or the buyer's default rate, or
/// 0 when no rate exists) and `amount` is `hours * hourly_rate`
/// rounded to 4 decimals. Non-billable entries still emit the event
/// with `"billable": false` and `amount` 0 so billing can tell
/// tracked time apart from priced time. `project_time_entry` is
/// the shared `ProjectTimeEntry` shape (see
/// `TimeEntry::to_project_time_entry_json`); the projects module
/// stores it, billing never reads it — there is exactly one
/// billing path, this event.
///
/// A manual edit re-emits with the new duration (`source`
/// `"manual_edit"`, `void` false) so billing re-prices the entry.
/// A delete emits a compensating void (`source` `"manual_delete"`,
/// `"void": true`, `amount` 0) carrying the deleted entry's
/// original duration and hours, so billing can reverse the pricing
/// it applied to the earlier emit for the same `entry_id`.
pub(crate) fn emit_time_logged(
    state: &TimeTrackerState,
    entry: &TimeEntry,
    source: &str,
    void: bool,
) {
    let rate = active_rate_for(state, &entry.user_id, entry.ended_at);
    let hours = entry.duration_secs as f64 / 3600.0;
    let amount = if void || !entry.billable {
        0.0
    } else {
        round4(hours * rate.map(|r| r.rate).unwrap_or(0.0))
    };
    state.emit(
        events::TIME_LOGGED,
        json!({
            "entry_id": entry.id.to_string(),
            "user_id": entry.user_id,
            "timer_id": entry.timer_id.map(|id| id.to_string()),
            "project_id": entry.project_id.map(|id| id.to_string()),
            "task_id": entry.task_id.map(|id| id.to_string()),
            "description": entry.description,
            "started_at": entry.started_at.to_rfc3339(),
            "ended_at": entry.ended_at.to_rfc3339(),
            "duration_secs": entry.duration_secs,
            "hours": round4(hours),
            "billable": entry.billable,
            "approval": format!("{:?}", entry.approval).to_lowercase(),
            "currency": rate.map(|r| r.currency.clone()).unwrap_or_else(|| state.global_settings().default_currency),
            "hourly_rate": rate.map(|r| r.rate).unwrap_or(0.0),
            "amount": amount,
            "source": source,
            "void": void,
            "project_time_entry": entry.to_project_time_entry_json(),
        }),
    );
}

/// Stop a timer and turn it into a time entry. Shared by the stop
/// endpoint, the agent stop endpoint, and the
/// `project.task.completed` hook handler. The entry starts
/// `pending` unless the global settings auto-approve timer
/// entries. Emits `timetracker.timer.stopped` and
/// `timetracker.time.logged`.
pub(crate) fn stop_timer_inner(
    state: &TimeTrackerState,
    timer_id: Uuid,
    reason: &str,
) -> UnifiedResult<TimeEntry> {
    let mut timer = state
        .timers
        .get_mut(&timer_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {timer_id}")))?;
    if timer.status == TimerStatus::Stopped {
        return Err(UnifiedError::conflict("Timer is already stopped"));
    }
    let now = Utc::now();
    let duration_secs = timer.elapsed_secs(now);
    timer.status = TimerStatus::Stopped;
    timer.stopped_at = Some(now);
    timer.updated_at = now;
    let timer_snapshot = timer.clone();
    drop(timer);
    let auto_approve = state.global_settings().auto_approve_timer_entries;
    let entry = TimeEntry {
        id: Uuid::now_v7(),
        user_id: timer_snapshot.user_id.clone(),
        project_id: timer_snapshot.project_id,
        task_id: timer_snapshot.task_id,
        timer_id: Some(timer_snapshot.id),
        description: timer_snapshot
            .description
            .clone()
            .unwrap_or_else(|| "Tracked time".to_string()),
        started_at: timer_snapshot.started_at,
        ended_at: now,
        duration_secs,
        billable: timer_snapshot.billable,
        approval: if auto_approve {
            EntryApproval::Approved
        } else {
            EntryApproval::Pending
        },
        approved_by: None,
        approved_at: if auto_approve { Some(now) } else { None },
        rejection_note: None,
        reason: None,
        created_at: now,
        updated_at: now,
    };
    state.entries.insert(entry.id, entry.clone());
    if let Some(mut timer) = state.timers.get_mut(&timer_id) {
        timer.entry_id = Some(entry.id);
    }
    state.emit(
        events::TIMER_STOPPED,
        json!({
            "timer_id": timer_snapshot.id.to_string(),
            "user_id": timer_snapshot.user_id,
            "entry_id": entry.id.to_string(),
            "duration_secs": duration_secs,
            "reason": reason,
        }),
    );
    emit_time_logged(state, &entry, "timer", false);
    Ok(entry)
}

/// Auto-pause a running timer after the caller's idle threshold:
/// the idle seconds are added to the timer's idle total (recorded,
/// never discarded), the timer goes to `paused`, and
/// `timetracker.timer.auto_paused` is emitted with the idle period.
/// Returns true when a pause happened.
pub(crate) fn auto_pause_on_idle(
    state: &TimeTrackerState,
    user_id: &str,
    idle_secs: i64,
) -> bool {
    let Some(timer) = active_timer_for(state, user_id) else {
        return false;
    };
    if timer.status != TimerStatus::Running {
        return false;
    }
    let resolved = state.resolve_settings_for_user(user_id);
    if idle_secs < resolved.idle_threshold_secs {
        return false;
    }
    let now = Utc::now();
    let mut paused = false;
    if let Some(mut entry) = state.timers.get_mut(&timer.id) {
        entry.idle_total_secs += idle_secs;
        entry.status = TimerStatus::Paused;
        entry.paused_at = Some(now);
        entry.updated_at = now;
        paused = true;
    }
    if paused {
        state.emit(
            events::TIMER_AUTO_PAUSED,
            json!({
                "timer_id": timer.id.to_string(),
                "user_id": user_id,
                "idle_secs": idle_secs,
                "idle_threshold_secs": resolved.idle_threshold_secs,
                "idle_total_secs": timer.idle_total_secs + idle_secs,
            }),
        );
    }
    paused
}
