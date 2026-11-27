// Projects module - phase handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// Phases

/// A phase may only complete from `active`: completing from planned
/// skips the work, and completing an already-completed phase is an
/// error. The PRINCE2 gate check in the handler still applies after
/// this.
fn phase_complete_requires_active(phase: &ProjectPhase) -> UnifiedResult<()> {
    if phase.status == "completed" {
        return Err(UnifiedError::bad_request("Phase is already completed"));
    }
    if phase.status != "active" {
        return Err(UnifiedError::bad_request(
            "Only an active phase can be completed",
        ));
    }
    Ok(())
}

pub(crate) async fn list_project_phases(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<PageResult<ProjectPhase>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
            &_scope,
        )?;
        let sort = params.allowed_sort(
            &["position", "created_at", "name", "updated_at"],
            "position",
        );
        let mut items: Vec<ProjectPhase> = state
            .phases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|phase| {
                phase.project_id == path.as_str()
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| phase.kind == *kind)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| phase.status == *status)
                    && matches_search(&phase.name, &params)
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then(a.created_at.cmp(&b.created_at))
            }),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_project_phase(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectPhase>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectPhase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Phase name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_phases) {
            return Err(UnifiedError::bad_request("methodology does not use phases"));
        }
        let kind = input
            .kind
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "waterfall".to_string());
        if !s4_valid_phase_kind(&kind) {
            return Err(UnifiedError::bad_request(
                "Phase kind must be one of: waterfall, prince2_stage, dmaic, hybrid",
            ));
        }
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "planned".to_string());
        if !s4_valid_phase_status(&status) {
            return Err(UnifiedError::bad_request(
                "Phase status must be one of: planned, active, completed",
            ));
        }
        s4_validate_date_range(input.start_date, input.end_date, "Phase")?;
        let position = input
            .position
            .unwrap_or_else(|| s4_max_phase_position(&state, &project.id) + 1);
        let current = now();
        let phase = ProjectPhase {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            kind,
            position,
            status,
            gate_status: "pending".to_string(),
            gate_decision: None,
            start_date: input.start_date,
            end_date: input.end_date,
            created_at: current,
            updated_at: current,
        };
        state.phases.insert(phase.id.clone(), phase.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.created",
            "phase",
            Some(phase.id.clone()),
            json!({ "name": phase.name, "kind": phase.kind, "position": phase.position }),
        );
        Ok(phase)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_phase(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
            &_scope,
        )?;
        s4_get_phase(&state, &project_id, &phase_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_phase(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectPhase>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        if let Some(kind) = &input.kind {
            if !s4_valid_phase_kind(kind.trim()) {
                return Err(UnifiedError::bad_request(
                    "Phase kind must be one of: waterfall, prince2_stage, dmaic, hybrid",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !s4_valid_phase_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Phase status must be one of: planned, active, completed",
                ));
            }
            // Status may only change via the gate/complete handlers,
            // which enforce the PRINCE2 gate check and the activation
            // of the next planned phase.
            s4_reject_direct_status_change(Some(status.trim()), "Phase status")?;
        }
        if let Some(gate_status) = &input.gate_status {
            if !s4_valid_gate_status(gate_status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Gate status must be one of: pending, approved, rejected",
                ));
            }
            // The gate may only change via the gate decision handler.
            s4_reject_direct_status_change(Some(gate_status.trim()), "Phase gate_status")?;
        }
        let mut phase = state
            .phases
            .get_mut(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        if phase.project_id != project_id {
            return Err(UnifiedError::not_found("Phase not found"));
        }
        let start_date = input.start_date.or(phase.start_date);
        let end_date = input.end_date.or(phase.end_date);
        s4_validate_date_range(start_date, end_date, "Phase")?;
        if let Some(name) = input.name {
            phase.name = s4_require_text(&name, "Phase name")?;
        }
        if let Some(kind) = input.kind {
            phase.kind = kind.trim().to_string();
        }
        if let Some(position) = input.position {
            phase.position = position;
        }
        phase.start_date = start_date;
        phase.end_date = end_date;
        phase.updated_at = now();
        let updated = phase.clone();
        drop(phase);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.updated",
            "phase",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_phase(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let phase = s4_get_phase(&state, &project_id, &phase_id)?;
        let has_tasks = state
            .tasks
            .iter()
            .any(|entry| entry.value().phase_id.as_deref() == Some(phase_id.as_str()));
        if has_tasks {
            return Err(UnifiedError::bad_request(
                "Cannot delete a phase that has tasks",
            ));
        }
        state
            .phases
            .remove(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.deleted",
            "phase",
            Some(phase.id.clone()),
            json!({ "name": phase.name }),
        );
        Ok(json!({ "deleted": true, "id": phase_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn decide_phase_gate(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<DecidePhaseGate>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        let decision = input.decision.trim().to_string();
        if decision != "approved" && decision != "rejected" {
            return Err(UnifiedError::bad_request(
                "Gate decision must be one of: approved, rejected",
            ));
        }
        let note = s4_clean_text(input.note);
        let phase = s4_get_phase(&state, &project_id, &phase_id)?;
        let was_completed = phase.status == "completed";
        let position = phase.position;
        let mut phase = state
            .phases
            .get_mut(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        phase.gate_status = decision.clone();
        phase.gate_decision = note.clone();
        phase.updated_at = now();
        let decided = phase.clone();
        drop(phase);
        // An approved gate on a completed phase opens the next phase.
        if decision == "approved" && was_completed {
            if let Some(next_id) = s4_next_phase_id(&state, &project.id, position) {
                if let Some(mut next) = state.phases.get_mut(next_id.as_str()) {
                    if next.status == "planned" {
                        next.status = "active".to_string();
                        next.updated_at = now();
                    }
                }
            }
        }
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.gate_decided",
            "phase",
            Some(decided.id.clone()),
            json!({ "name": decided.name, "decision": decision, "note": note }),
        );
        state.emit(
            "project.phase.gate_decided",
            json!({
                "project_id": project.id,
                "phase_id": decided.id,
                "name": decided.name,
                "decision": decision,
                "note": note,
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Phase gate {}: {}", decision, decided.name),
            format!("The gate for phase {} was {}.", decided.name, decision),
        );
        Ok(decided)
    })();
    json::respond(result, false)
}

pub(crate) async fn complete_project_phase(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let phase = s4_get_phase(&state, &project_id, &phase_id)?;
        phase_complete_requires_active(&phase)?;
        if phase.kind == "prince2_stage" && phase.gate_status != "approved" {
            return Err(UnifiedError::bad_request(
                "A PRINCE2 stage can only be completed once its gate is approved",
            ));
        }
        let current = now();
        let mut phase = state
            .phases
            .get_mut(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        phase.status = "completed".to_string();
        if phase.end_date.is_none() {
            phase.end_date = Some(current);
        }
        phase.updated_at = current;
        let completed = phase.clone();
        drop(phase);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.completed",
            "phase",
            Some(completed.id.clone()),
            json!({ "name": completed.name, "kind": completed.kind }),
        );
        state.notify(
            &project.owner_id,
            format!("Phase completed: {}", completed.name),
            format!("Phase {} is now complete.", completed.name),
        );
        Ok(completed)
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn phase_with_status(status: &str) -> ProjectPhase {
        ProjectPhase {
            id: "ph1".to_string(),
            project_id: "p1".to_string(),
            name: "Phase".to_string(),
            kind: "waterfall".to_string(),
            position: 1,
            status: status.to_string(),
            gate_status: "pending".to_string(),
            gate_decision: None,
            start_date: None,
            end_date: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn update_rejects_direct_status_and_gate_changes() {
        assert!(s4_reject_direct_status_change(None, "Phase status").is_ok());
        assert!(s4_reject_direct_status_change(Some("completed"), "Phase status").is_err());
        assert!(s4_reject_direct_status_change(Some("approved"), "Phase gate_status").is_err());
    }

    #[test]
    fn complete_requires_the_active_state() {
        assert!(phase_complete_requires_active(&phase_with_status("active")).is_ok());
        assert!(phase_complete_requires_active(&phase_with_status("planned")).is_err());
        assert!(phase_complete_requires_active(&phase_with_status("completed")).is_err());
    }
}
