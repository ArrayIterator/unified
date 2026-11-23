// timetracker — HTTP handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Handlers: status, timers, entries
// ---------------------------------------------------------------------------

pub(crate) async fn get_status(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        Ok(json!({
            "module": "timetracker",
            "timers": state.timers.len(),
            "entries": state.entries.len(),
            "screenshots": state.screenshots.len(),
            "timesheets": state.timesheets.len(),
            "teams": state.teams.len(),
            "staff": state.staff.len(),
            "buyer_settings": state.buyer_settings.len(),
            "desktop_agent": "not_built",
            "desktop_agent_note": "No desktop or mobile agent exists yet. Agent endpoints under /api/v1/timetracker/agent/* are the server API the future agent app will call.",
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_timers(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TimeTrackerTimer>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let status_filter = params.filters.get("status").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let mut items: Vec<TimeTrackerTimer> = state
            .timers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|timer| {
                match &user_filter {
                    Some(wanted) => &timer.user_id == wanted,
                    None => timer.user_id == caller.actor,
                }
            })
            .filter(|timer| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "running" => TimerStatus::Running,
                        "paused" => TimerStatus::Paused,
                        "stopped" => TimerStatus::Stopped,
                        _ => return false,
                    };
                    if timer.status != expected {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if timer.project_id.map(|id| id.to_string()) != Some(project_id.clone()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["started_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "started_at" => a.started_at.cmp(&b.started_at),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn start_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<StartTimerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let timer = TimeTrackerTimer {
            id: Uuid::now_v7(),
            user_id: caller.actor.clone(),
            project_id: input.project_id,
            task_id: input.task_id,
            description,
            billable: input.billable,
            status: TimerStatus::Running,
            started_at: now,
            paused_total_secs: 0,
            paused_at: None,
            idle_total_secs: 0,
            stopped_at: None,
            entry_id: None,
            created_at: now,
            updated_at: now,
        };
        // Atomic check-and-insert: two concurrent starts for this
        // user cannot both pass (see `insert_timer_if_none_active`).
        if !state.insert_timer_if_none_active(timer.clone()) {
            return Err(UnifiedError::conflict(
                "An active timer already exists for this user: stop or pause it before starting another",
            ));
        }
        state.emit(
            events::TIMER_STARTED,
            json!({
                "timer_id": timer.id.to_string(),
                "user_id": timer.user_id,
                "project_id": timer.project_id.map(|id| id.to_string()),
                "task_id": timer.task_id.map(|id| id.to_string()),
                "billable": timer.billable,
                "started_at": timer.started_at.to_rfc3339(),
            }),
        );
        Ok(timer)
    })();
    match result {
        Ok(timer) => json::success_response(timer, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn stop_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let timer = load_timer(&state, &path.into_inner())?;
        if timer.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timer owner can stop their timer",
            ));
        }
        stop_timer_inner(&state, timer.id, "manual_stop")
    })();
    json::respond(result, debug)
}

pub(crate) async fn pause_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let timer = load_timer(&state, &path.into_inner())?;
        if timer.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timer owner can pause their timer",
            ));
        }
        if timer.status != TimerStatus::Running {
            return Err(UnifiedError::conflict("Only a running timer can be paused"));
        }
        let now = Utc::now();
        let mut updated = timer.clone();
        updated.status = TimerStatus::Paused;
        updated.paused_at = Some(now);
        updated.updated_at = now;
        state.timers.insert(updated.id, updated.clone());
        state.emit(
            events::TIMER_PAUSED,
            json!({
                "timer_id": updated.id.to_string(),
                "user_id": updated.user_id,
                "paused_at": now.to_rfc3339(),
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn resume_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let timer = load_timer(&state, &path.into_inner())?;
        if timer.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timer owner can resume their timer",
            ));
        }
        if timer.status != TimerStatus::Paused {
            return Err(UnifiedError::conflict("Only a paused timer can be resumed"));
        }
        let now = Utc::now();
        let mut updated = timer.clone();
        if let Some(paused_at) = updated.paused_at {
            updated.paused_total_secs += (now - paused_at).num_seconds().max(0);
        }
        updated.status = TimerStatus::Running;
        updated.paused_at = None;
        updated.updated_at = now;
        state.timers.insert(updated.id, updated.clone());
        state.emit(
            events::TIMER_RESUMED,
            json!({
                "timer_id": updated.id.to_string(),
                "user_id": updated.user_id,
                "paused_total_secs": updated.paused_total_secs,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_active_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let timer = active_timer_for(&state, &caller.actor);
        let elapsed_secs = timer.as_ref().map(|t| t.elapsed_secs(Utc::now()));
        Ok(json!({
            "active": timer.is_some(),
            "timer": timer,
            "elapsed_secs": elapsed_secs,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_entries(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TimeEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let approval_filter = params.filters.get("approval").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let billable_filter = params.filters.get("billable").cloned();
        let mut items: Vec<TimeEntry> = state
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                match &user_filter {
                    Some(wanted) => &entry.user_id == wanted,
                    None => entry.user_id == caller.actor,
                }
            })
            .filter(|entry| {
                if let Some(approval) = &approval_filter {
                    let expected = match approval.as_str() {
                        "pending" => EntryApproval::Pending,
                        "approved" => EntryApproval::Approved,
                        "rejected" => EntryApproval::Rejected,
                        _ => return false,
                    };
                    if entry.approval != expected {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if entry.project_id.map(|id| id.to_string()) != Some(project_id.clone()) {
                        return false;
                    }
                }
                if let Some(billable) = &billable_filter {
                    let wanted = billable == "true";
                    if entry.billable != wanted {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["started_at", "created_at", "duration_secs"], "started_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                "duration_secs" => a.duration_secs.cmp(&b.duration_secs),
                _ => a.started_at.cmp(&b.started_at),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let duration_secs = validate_interval(input.started_at, input.ended_at, MAX_ENTRY_SECONDS)?;
        let now = Utc::now();
        let entry = TimeEntry {
            id: Uuid::now_v7(),
            user_id: caller.actor.clone(),
            project_id: input.project_id,
            task_id: input.task_id,
            timer_id: None,
            description,
            started_at: input.started_at,
            ended_at: input.ended_at,
            duration_secs,
            billable: input.billable,
            approval: EntryApproval::Pending,
            approved_by: None,
            approved_at: None,
            rejection_note: None,
            reason,
            created_at: now,
            updated_at: now,
        };
        state.entries.insert(entry.id, entry.clone());
        emit_time_logged(&state, &entry, "manual", false);
        Ok(entry)
    })();
    match result {
        Ok(entry) => json::success_response(entry, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id != caller.actor {
            can_view_user(&ctx, &caller, &entry.user_id)?;
        }
        Ok(entry)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let mut entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the entry owner can edit their entry",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict(
                "Only a pending entry can be edited; approved or rejected entries are final",
            ));
        }
        let reason = require_text(
            input.reason.as_deref().unwrap_or(""),
            "reason",
            MAX_REASON_LEN,
        )?;
        if let Some(description) = &input.description {
            entry.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(project_id) = input.project_id {
            entry.project_id = project_id;
        }
        if let Some(task_id) = input.task_id {
            entry.task_id = task_id;
        }
        if let Some(billable) = input.billable {
            entry.billable = billable;
        }
        let started_at = input.started_at.unwrap_or(entry.started_at);
        let ended_at = input.ended_at.unwrap_or(entry.ended_at);
        entry.duration_secs = validate_interval(started_at, ended_at, MAX_ENTRY_SECONDS)?;
        entry.started_at = started_at;
        entry.ended_at = ended_at;
        entry.reason = Some(reason.clone());
        entry.updated_at = Utc::now();
        state.entries.insert(entry.id, entry.clone());
        state.emit(
            events::ENTRY_UPDATED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "reason": reason,
                "duration_secs": entry.duration_secs,
            }),
        );
        // Billing prices `timetracker.time.logged` only, so an edit
        // must re-emit it with the new duration or the edited time
        // is never re-priced.
        emit_time_logged(&state, &entry, "manual_edit", false);
        Ok(entry)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DeleteEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the entry owner can delete their entry",
            ));
        }
        if entry.approval == EntryApproval::Approved {
            return Err(UnifiedError::conflict(
                "An approved entry cannot be deleted; ask an approver to reject it first",
            ));
        }
        state.entries.remove(&entry.id);
        state.emit(
            events::ENTRY_DELETED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "reason": reason,
                "duration_secs": entry.duration_secs,
            }),
        );
        // Compensating void: billing priced the original
        // `timetracker.time.logged` emit, so the delete must tell
        // it to reverse that pricing for this entry id.
        emit_time_logged(&state, &entry, "manual_delete", true);
        Ok(json!({ "id": entry.id.to_string(), "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn approve_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "An entry cannot be approved by its own owner",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict("Only a pending entry can be approved"));
        }
        entry.approval = EntryApproval::Approved;
        entry.approved_by = Some(caller.actor.clone());
        entry.approved_at = Some(Utc::now());
        entry.rejection_note = None;
        entry.updated_at = Utc::now();
        state.entries.insert(entry.id, entry.clone());
        state.emit(
            events::ENTRY_APPROVED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "approved_by": caller.actor,
                "note": note,
            }),
        );
        Ok(entry)
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "An entry cannot be rejected by its own owner",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict("Only a pending entry can be rejected"));
        }
        entry.approval = EntryApproval::Rejected;
        entry.approved_by = Some(caller.actor.clone());
        entry.approved_at = Some(Utc::now());
        entry.rejection_note = note.clone();
        entry.updated_at = Utc::now();
        state.entries.insert(entry.id, entry.clone());
        state.emit(
            events::ENTRY_REJECTED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "rejected_by": caller.actor,
                "note": note,
            }),
        );
        Ok(entry)
    })();
    json::respond(result, debug)
}
