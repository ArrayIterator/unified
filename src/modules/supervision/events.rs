// supervision module -- Hook event handlers.

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::json;

use super::*;

pub(crate) fn on_project_changed(_state: SupervisionState, ctx: &mut HookContext) {
    ctx.add_result(json!({ "acknowledged": true }));
}

/// Answer `project.archived`: suspend active assignments for the
/// archived project so no new supervision work starts on it.
pub(crate) fn on_project_archived(state: SupervisionState, ctx: &mut HookContext) {
    let project_id = ctx
        .data
        .get("id")
        .or_else(|| ctx.data.get("project_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut suspended = 0usize;
    if let Some(project_id) = &project_id {
        let now = Utc::now();
        let active: Vec<SupervisionAssignment> = state
            .assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|assignment| {
                assignment.project_id == *project_id
                    && assignment.status == AssignmentStatus::Active
            })
            .collect();
        for mut assignment in active {
            assignment.status = AssignmentStatus::Suspended;
            assignment.updated_at = now;
            state
                .assignments
                .insert(assignment.id, assignment);
            suspended += 1;
        }
    }
    ctx.add_result(json!({
        "acknowledged": true,
        "project_id": project_id,
        "assignments_suspended": suspended,
    }));
}

/// Acknowledge project task events: a completed task never
/// auto-resolves a defect -- resolution needs human verification
/// through `POST /defects/{id}/verify`.
pub(crate) fn on_project_task_event(_state: SupervisionState, ctx: &mut HookContext) {
    ctx.add_result(json!({ "acknowledged": true }));
}
