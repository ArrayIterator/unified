// Projects module — hook handlers (ticket bridge, billing, progress).
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.
//
// Pending-integration note: the three listeners in this file —
// `project.task_from_ticket_requested`,
// `project.ticket_from_task_requested`, and
// `project.billable_hours_requested` — have no emitter anywhere in
// the codebase today. The tickets and billing modules own those
// emitters, so wiring them is integrator work outside this module;
// the listeners are kept so the wiring works once they land.

use super::*;
use unified_core::hooks::HookContext;
use unified_core::json;
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Create a task from a support ticket (`project.task_from_ticket_requested`).
/// No default project is created: a missing or unknown `project_id`
/// is reported back as an error result.
pub(crate) fn on_task_from_ticket_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "error": "project_id is required" }));
        return;
    }
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
            return;
        }
    };
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let title = if title.is_empty() {
        "Untitled task".to_string()
    } else {
        title
    };
    let body = data
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = data
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let status = methodology_profile(&project.methodology)
        .and_then(|profile| profile.default_statuses.first().copied())
        .unwrap_or("backlog")
        .to_string();
    let ticket_id = data
        .get("ticket_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let task = ProjectTask {
        id: new_id(),
        project_id: project.id.clone(),
        board_id: None,
        parent_id: None,
        epic_id: None,
        title,
        description: body,
        task_type: "task".to_string(),
        status,
        priority: "normal".to_string(),
        assignee_id: None,
        reporter_id: user_id.clone(),
        estimate_points: None,
        estimate_hours: None,
        labels: Vec::new(),
        custom_values: Value::Object(Default::default()),
        start_date: None,
        due_date: None,
        completed_at: None,
        sprint_id: None,
        phase_id: None,
        cycle_id: None,
        pi_id: None,
        milestone_id: None,
        process_group: None,
        knowledge_area: None,
        waste_flag: None,
        pair_partner_id: None,
        test_first: false,
        dmaic_phase: None,
        appetite_weeks: None,
        created_at: now(),
        updated_at: now(),
    };
    state.tasks.insert(task.id.clone(), task.clone());
    state.log_activity(
        &project.id,
        &user_id,
        "task.created_from_ticket",
        "task",
        Some(task.id.clone()),
        json!({ "ticket_id": ticket_id }),
    );
    let value = serde_json::to_value(&task).unwrap_or(Value::Null);
    state.emit("project.task.created", value);
    ctx.add_result(json!({ "task_id": task.id, "project_id": project.id }));
}

/// Ask the Tickets module to open a ticket for a task
/// (`project.ticket_from_task_requested`).
pub(crate) fn on_ticket_from_task_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let task_id = data
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if task_id.is_empty() {
        ctx.add_result(json!({ "error": "task_id is required" }));
        return;
    }
    let task = match state.get_task(&task_id) {
        Ok(task) => task,
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
            return;
        }
    };
    state.emit(
        "ticket.create_requested",
        json!({
            "title": task.title,
            "body": task.description,
            "user_id": task.reporter_id,
            "category": "project-task",
            "task_id": task.id,
        }),
    );
    ctx.add_result(json!({ "requested": true, "task_id": task.id }));
}

/// Answer the billing module with unbilled billable hours
/// (`project.billable_hours_requested`). Both filters are optional.
pub(crate) fn on_billable_hours_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let user_id = data
        .get("user_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut hours: f64 = 0.0;
    let mut entries: Vec<Value> = Vec::new();
    for entry in state.time_entries.iter() {
        let record = entry.value();
        if !record.billable || record.billed {
            continue;
        }
        if let Some(project_id) = &project_id {
            if &record.project_id != project_id {
                continue;
            }
        }
        if let Some(user_id) = &user_id {
            if &record.user_id != user_id {
                continue;
            }
        }
        hours += record.hours;
        entries.push(json!({
            "entry_id": record.id,
            "task_id": record.task_id,
            "hours": record.hours,
        }));
    }
    ctx.add_result(json!({
        "project_id": project_id,
        "user_id": user_id,
        "hours": hours,
        "entries": entries,
    }));
}

/// Answer the supervision module's project lookup
/// (`project.get_requested`). A missing project answers
/// `{"exists": false}` — never silence, or supervision fails
/// closed with an internal error instead of a clean "not found".
pub(crate) fn on_project_get_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "error": "project_id is required" }));
        return;
    }
    match state.get_project(&project_id) {
        Ok(project) => {
            ctx.add_result(json!({
                "exists": true,
                "project": {
                    "id": project.id,
                    "name": project.name,
                    "status": project.status,
                    "archived": project.archived,
                },
            }));
        }
        Err(_) => {
            ctx.add_result(json!({ "exists": false, "project_id": project_id }));
        }
    }
}

/// Answer the supervision module's progress query
/// (`project.progress_requested`). The percent is derived live
/// from this module's own tasks: completed tasks / total tasks *
/// 100 (0 tasks -> 0.0), rounded to two decimals. With a
/// `milestone_id`, only that milestone's tasks count, and
/// `milestone_reached` comes from the milestone's own `reached`
/// flag; without one, the whole project counts and
/// `milestone_reached` is false. An unknown project or an unknown
/// milestone answers an error — supervision records the snapshot
/// as unverified rather than inventing a number.
pub(crate) fn on_project_progress_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "error": "project_id is required" }));
        return;
    }
    if state.get_project(&project_id).is_err() {
        ctx.add_result(json!({ "error": "Project not found" }));
        return;
    }
    let milestone_id = data
        .get("milestone_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut milestone_reached = false;
    if let Some(milestone_id) = &milestone_id {
        match state.milestones.get(milestone_id) {
            Some(entry) if entry.value().project_id == project_id => {
                milestone_reached = entry.value().reached;
            }
            _ => {
                ctx.add_result(json!({ "error": "Milestone not found" }));
                return;
            }
        }
    }
    let mut total = 0usize;
    let mut done = 0usize;
    for entry in state.tasks.iter() {
        let task = entry.value();
        if task.project_id != project_id {
            continue;
        }
        if let Some(milestone_id) = &milestone_id {
            if task.milestone_id.as_deref() != Some(milestone_id.as_str()) {
                continue;
            }
        }
        total += 1;
        if s2_task_is_done(&task.status) {
            done += 1;
        }
    }
    let percent = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0 * 100.0).round() / 100.0
    };
    ctx.add_result(json!({
        "project_id": project_id,
        "milestone_id": milestone_id,
        "percent_complete": percent,
        "milestone_reached": milestone_reached,
    }));
}

/// Create a follow-up task for supervision
/// (`project.task.create_requested`). Mirrors the ticket
/// follow-up shape (default status from the project's
/// methodology profile, `reporter_id` = `requested_by`), with
/// the supervision trace kept in `custom_values` (`source`,
/// `inspection_id`, `defect_id`). An unknown project answers
/// `{"created": false, "error": ...}` — supervision records the
/// follow-up as requested-but-not-created, never as created.
pub(crate) fn on_project_task_create_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "created": false, "error": "project_id is required" }));
        return;
    }
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => {
            ctx.add_result(json!({ "created": false, "error": error.message }));
            return;
        }
    };
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let title = if title.is_empty() {
        "Untitled task".to_string()
    } else {
        title
    };
    let description = data
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let requested_by = data
        .get("requested_by")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let status = methodology_profile(&project.methodology)
        .and_then(|profile| profile.default_statuses.first().copied())
        .unwrap_or("backlog")
        .to_string();
    let mut trace = serde_json::Map::new();
    trace.insert(
        "source".to_string(),
        Value::String("supervision".to_string()),
    );
    if let Some(value) = data.get("inspection_id") {
        trace.insert("inspection_id".to_string(), value.clone());
    }
    if let Some(value) = data.get("defect_id") {
        trace.insert("defect_id".to_string(), value.clone());
    }
    let task = ProjectTask {
        id: new_id(),
        project_id: project.id.clone(),
        board_id: None,
        parent_id: None,
        epic_id: None,
        title,
        description,
        task_type: "task".to_string(),
        status,
        priority: "normal".to_string(),
        assignee_id: None,
        reporter_id: requested_by.clone(),
        estimate_points: None,
        estimate_hours: None,
        labels: Vec::new(),
        custom_values: Value::Object(trace),
        start_date: None,
        due_date: None,
        completed_at: None,
        sprint_id: None,
        phase_id: None,
        cycle_id: None,
        pi_id: None,
        milestone_id: None,
        process_group: None,
        knowledge_area: None,
        waste_flag: None,
        pair_partner_id: None,
        test_first: false,
        dmaic_phase: None,
        appetite_weeks: None,
        created_at: now(),
        updated_at: now(),
    };
    state.tasks.insert(task.id.clone(), task.clone());
    state.log_activity(
        &project.id,
        &requested_by,
        "task.created_from_supervision",
        "task",
        Some(task.id.clone()),
        json!({ "inspection_id": data.get("inspection_id"), "defect_id": data.get("defect_id") }),
    );
    let value = serde_json::to_value(&task).unwrap_or(Value::Null);
    state.emit("project.task.created", value);
    ctx.add_result(json!({ "created": true, "task_id": task.id, "project_id": project.id }));
}

/// Answer the supervision module's task lookup
/// (`project.task.get_requested`). Used to validate defect task
/// links. A missing task answers `{"exists": false}`.
pub(crate) fn on_project_task_get_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let task_id = data
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if task_id.is_empty() {
        ctx.add_result(json!({ "error": "task_id is required" }));
        return;
    }
    match state.get_task(&task_id) {
        Ok(task) => {
            let value = serde_json::to_value(&task).unwrap_or(Value::Null);
            ctx.add_result(json!({ "exists": true, "task": value }));
        }
        Err(_) => {
            ctx.add_result(json!({ "exists": false, "task_id": task_id }));
        }
    }
}
