// service — hooks.
// Hook handlers (project/user/screenshot events).
//
// Split from `src/modules/timetracker/service.rs`; moved verbatim, no behaviour changes.

use super::super::*;
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
// Hook handlers (cross-module events)
// ---------------------------------------------------------------------------

/// Answer `project.task.completed`: stop every running timer
/// linked to that task — the work is done, so the timer must not
/// keep running — and log their entries. Unknown or missing task
/// ids are ignored (a stale event can never stop unrelated
/// timers). The event name is confirmed by the audit module's
/// audited-events list; the projects module file itself was not
/// present in the staging tree when this module was written, so
/// the handler only trusts the `task_id` / `id` fields in the
/// payload.
pub(crate) fn on_project_task_completed(state: TimeTrackerState, ctx: &mut HookContext) {
    let task_id = ctx
        .data
        .get("task_id")
        .or_else(|| ctx.data.get("id"))
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(task_id) = task_id else {
        ctx.add_result(json!({ "stopped_timers": 0, "error": "project.task.completed requires 'task_id'" }));
        return;
    };
    let running: Vec<Uuid> = state
        .timers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|timer| {
            timer.task_id == Some(task_id)
                && (timer.status == TimerStatus::Running || timer.status == TimerStatus::Paused)
        })
        .map(|timer| timer.id)
        .collect();
    let mut stopped = 0usize;
    for timer_id in running {
        if stop_timer_inner(&state, timer_id, "task_completed").is_ok() {
            stopped += 1;
        }
    }
    ctx.add_result(json!({
        "stopped_timers": stopped,
        "task_id": task_id.to_string(),
    }));
}

/// Answer `project.task.created`: acknowledged only. The task id
/// is echoed back so the caller can verify the link; no record is
/// created here — timers and entries link to tasks by id when the
/// user starts tracking against one.
pub(crate) fn on_project_task_created(_state: TimeTrackerState, ctx: &mut HookContext) {
    let task_id = ctx
        .data
        .get("task_id")
        .or_else(|| ctx.data.get("id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    ctx.add_result(json!({ "acknowledged": true, "task_id": task_id }));
}

/// Answer `project.time.logged`: acknowledged only. The projects
/// module keeps its own time entries; importing them here would
/// double-count billable hours, so nothing is recorded. This
/// module's entries are the canonical billable source and are
/// published as `timetracker.time.logged` instead.
pub(crate) fn on_project_time_logged(_state: TimeTrackerState, ctx: &mut HookContext) {
    ctx.add_result(json!({ "acknowledged": true }));
}

/// Answer `user.created`: accept any pending staff invite whose
/// email matches the new user, and create their staff record (and
/// team membership) from the invite. The users module emits
/// `user.created` with `{"id", "email", "name"}`.
pub(crate) fn on_user_created(state: TimeTrackerState, ctx: &mut HookContext) {
    let email = ctx
        .data
        .get("email")
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_lowercase());
    let user_id = ctx
        .data
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(email), Some(user_id)) = (email, user_id) else {
        ctx.add_result(json!({ "accepted": 0, "error": "user.created requires 'id' and 'email'" }));
        return;
    };
    let pending: Vec<StaffInvite> = state
        .invites
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|invite| invite.email == email && invite.status == "pending")
        .collect();
    let now = Utc::now();
    let mut accepted = 0usize;
    for mut invite in pending {
        if invite.expires_at < now {
            invite.status = "expired".to_string();
            invite.updated_at = now;
            state.invites.insert(invite.id, invite);
            continue;
        }
        invite.status = "accepted".to_string();
        invite.accepted_at = Some(now);
        invite.accepted_user_id = Some(user_id.clone());
        invite.updated_at = now;
        state.invites.insert(invite.id, invite.clone());
        // The staff record and the team membership are independent
        // provisions: a user who already has a staff record (from
        // an earlier invite or a manual add) still needs the team
        // membership this invite carries. `accepted` counts only
        // invites that actually produced something new — a staff
        // record or a team membership.
        let mut produced = false;
        if !state.staff.contains_key(&user_id) {
            let staff = StaffSettings {
                user_id: user_id.clone(),
                company_id: invite.company_id,
                role: invite.role,
                manager_id: None,
                screenshots_enabled: None,
                blur_screenshots: None,
                idle_threshold_secs: None,
                activity_level: None,
                timezone: None,
                working_hours_start: None,
                working_hours_end: None,
                created_at: now,
                updated_at: now,
            };
            state.staff.insert(user_id.clone(), staff);
            produced = true;
            state.emit(
                events::STAFF_ADDED,
                json!({
                    "user_id": user_id,
                    "company_id": invite.company_id.to_string(),
                    "role": format!("{:?}", invite.role).to_lowercase(),
                    "source": "invite",
                }),
            );
        }
        if let Some(team_id) = invite.team_id {
            let key = team_member_key(team_id, &user_id);
            if !state.team_members.contains_key(&key) {
                state.team_members.insert(
                    key,
                    TeamMember {
                        team_id,
                        user_id: user_id.clone(),
                        role: invite.role,
                        joined_at: now,
                    },
                );
                produced = true;
            }
        }
        state.emit(
            events::STAFF_INVITE_ACCEPTED,
            json!({
                "invite_id": invite.id.to_string(),
                "user_id": user_id,
                "company_id": invite.company_id.to_string(),
            }),
        );
        if produced {
            accepted += 1;
        }
    }
    ctx.add_result(json!({ "accepted": accepted, "user_id": user_id }));
}

/// Answer `timetracker.screenshots.purge_requested`: run the
/// retention purge. Hook handlers are synchronous, so the purge
/// runs on a spawned worker (the backup module's pattern: the
/// current Tokio runtime when there is one, otherwise a dedicated
/// thread with its own runtime). The result reports the dispatch,
/// not the counts — the counts arrive later in the
/// `timetracker.screenshots.purged` event the purge emits.
pub(crate) fn on_screenshots_purge_requested(state: TimeTrackerState, ctx: &mut HookContext) {
    let registry = state
        .storage_registry
        .read()
        .expect("storage registry lock poisoned")
        .clone();
    let Some(registry) = registry else {
        ctx.add_result(json!({ "purged": false, "error": "storage registry is not attached" }));
        return;
    };
    let future = async move {
        purge_expired_screenshots(&state, &registry).await;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
    ctx.add_result(json!({ "purged": "async", "queued": true }));
}
