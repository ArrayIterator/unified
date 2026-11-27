// supervision module -- HTTP handlers for defects and approvals.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

/// Whether an update request edits anything other than the
/// defect status. Used to keep verified defects immutable: a
/// verified defect may only be reopened to open, never have its
/// title, severity, or assignee rewritten in place.
pub(crate) fn defect_non_status_edits_present(input: &UpdateDefectRequest) -> bool {
    input.title.is_some()
        || input.description.is_some()
        || input.location.is_some()
        || input.severity.is_some()
        || input.assigned_to.is_some()
}

/// Apply a validated defect status change, keeping `resolved_at`
/// in step with the status: stamped when the defect enters
/// Resolved, cleared when it leaves Resolved for any other
/// status, and left alone otherwise.
pub(crate) fn apply_defect_status_change(
    defect: &mut Defect,
    status: DefectStatus,
    now: DateTime<Utc>,
) {
    if status == DefectStatus::Resolved && defect.resolved_at.is_none() {
        defect.resolved_at = Some(now);
    }
    if defect.status == DefectStatus::Resolved && status != DefectStatus::Resolved {
        defect.resolved_at = None;
    }
    defect.status = status;
}

pub(crate) async fn list_defects(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let inspection_filter = params.filters.get("inspection_id").cloned();
        let severity_filter = params.filters.get("severity").cloned();
        let status_filter = params.filters.get("status").cloned();
        let task_filter = params.filters.get("task_id").cloned();
        let mut items: Vec<Defect> = state
            .defects
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|defect| {
                if let Some(project_id) = &project_filter {
                    if &defect.project_id != project_id {
                        return false;
                    }
                }
                if let Some(inspection_id) = &inspection_filter {
                    if defect.inspection_id.map(|id| id.to_string()) != Some(inspection_id.clone()) {
                        return false;
                    }
                }
                if let Some(severity) = &severity_filter {
                    if defect.severity.as_str() != severity {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if defect.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(task_id) = &task_filter {
                    if defect.task_id.as_deref() != Some(task_id.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|defect| defect.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateDefectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        let inspection_id = match input.inspection_id {
            Some(id) => {
                let inspection = load_inspection(&state, &id.to_string())?;
                if inspection.project_id != project_id {
                    return Err(UnifiedError::bad_request(
                        "inspection belongs to a different project",
                    ));
                }
                Some(id)
            }
            None => None,
        };
        let task_id = match &input.task_id {
            Some(raw) => {
                let id = require_id(raw, "task_id")?;
                query_task_exists(&ctx, &id)?;
                Some(id)
            }
            None => None,
        };
        let assigned_to = match &input.assigned_to {
            Some(raw) => Some(require_id(raw, "assigned_to")?),
            None => None,
        };
        let now = Utc::now();
        let defect = Defect {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            inspection_id,
            task_id,
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            description: opt_text(&input.description, "description", MAX_TEXT_LEN)?,
            location: opt_text(&input.location, "location", MAX_LOCATION_LEN)?,
            severity: input.severity,
            status: DefectStatus::Open,
            reported_by: caller.actor.clone(),
            assigned_to,
            follow_up_requested: false,
            follow_up_task_id: None,
            resolved_at: None,
            verified_by: None,
            verified_at: None,
            created_at: now,
            updated_at: now,
        };
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.created",
            json!({
                "defect_id": defect.id.to_string(),
                "project_id": defect.project_id,
                "inspection_id": defect.inspection_id.map(|id| id.to_string()),
                "severity": defect.severity.as_str(),
            }),
        );
        if defect.severity == DefectSeverity::Critical {
            state.notify(
                "supervision.defect.critical",
                &defect.reported_by,
                "Critical defect logged",
                &format!(
                    "Critical defect \"{}\" was logged on project {}.",
                    defect.title, defect.project_id
                ),
                Some(format!("/api/v1/supervision/defects/{}", defect.id)),
            );
        }
        Ok(defect)
    })();
    match result {
        Ok(defect) => created(defect),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_defect(&state, &path.into_inner())
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDefectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut defect = load_defect(&state, &path.into_inner())?;
        let input = body.into_inner();
        // A verified defect is a closed record: only reopening
        // it to open is allowed, never rewriting its fields.
        if defect.status == DefectStatus::Verified
            && defect_non_status_edits_present(&input)
        {
            return Err(UnifiedError::conflict(
                "a verified defect cannot be edited; reopen it to open before changing it",
            ));
        }
        if let Some(title) = input.title {
            defect.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            defect.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(location) = input.location {
            defect.location = match location {
                Some(raw) => Some(require_text(&raw, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(severity) = input.severity {
            defect.severity = severity;
        }
        if let Some(status) = input.status {
            // Forward-only transitions; a verified defect may be
            // reopened to open. Jumping straight from open to
            // verified is rejected -- resolution must be verified
            // by a human through the verify endpoint.
            let allowed = match (defect.status, status) {
                (DefectStatus::Open, DefectStatus::InProgress) => true,
                (DefectStatus::Open, DefectStatus::Resolved) => true,
                (DefectStatus::InProgress, DefectStatus::Resolved) => true,
                (DefectStatus::InProgress, DefectStatus::Open) => true,
                (DefectStatus::Resolved, DefectStatus::InProgress) => true,
                (DefectStatus::Verified, DefectStatus::Open) => true,
                (current, next) => current == next,
            };
            if !allowed {
                return Err(UnifiedError::conflict(format!(
                    "defect cannot move from \"{}\" to \"{}\"",
                    defect.status.as_str(),
                    status.as_str()
                )));
            }
            apply_defect_status_change(&mut defect, status, Utc::now());
        }
        if let Some(assigned_to) = input.assigned_to {
            defect.assigned_to = match assigned_to {
                Some(raw) => Some(require_id(&raw, "assigned_to")?),
                None => None,
            };
        }
        defect.updated_at = Utc::now();
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.updated",
            json!({
                "defect_id": defect.id.to_string(),
                "project_id": defect.project_id,
                "status": defect.status.as_str(),
            }),
        );
        Ok(defect)
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn follow_up_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut defect = load_defect(&state, &path.into_inner())?;
        if defect.status == DefectStatus::Verified {
            return Err(UnifiedError::conflict(
                "a verified defect needs no follow-up task",
            ));
        }
        if defect.follow_up_requested && defect.follow_up_task_id.is_some() {
            return Err(UnifiedError::conflict(
                "a follow-up task was already created for this defect",
            ));
        }
        let title = format!("Fix defect: {}", defect.title);
        let description = format!(
            "Defect \"{}\" on project {} (severity {}).{}",
            defect.title,
            defect.project_id,
            defect.severity.as_str(),
            defect
                .description
                .as_ref()
                .map(|text| format!(" {text}"))
                .unwrap_or_default()
        );
        let task_id = match request_follow_up_task(
            &ctx,
            &defect.project_id,
            &title,
            &description,
            defect.inspection_id,
            Some(defect.id),
            &caller.actor,
        ) {
            Ok(task_id) => task_id,
            Err(error) => {
                defect.follow_up_requested = true;
                defect.updated_at = Utc::now();
                state.defects.insert(defect.id, defect.clone());
                state.emit(
                    "supervision.defect.follow_up_unanswered",
                    json!({
                        "defect_id": defect.id.to_string(),
                        "error": error.to_string(),
                    }),
                );
                return Ok(defect);
            }
        };
        defect.follow_up_requested = true;
        defect.follow_up_task_id = task_id.clone();
        // Link the defect to the task the projects module just
        // created, so later reads join the two without duplicating
        // task data here.
        if defect.task_id.is_none() {
            defect.task_id = task_id;
        }
        defect.updated_at = Utc::now();
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.follow_up_requested",
            json!({
                "defect_id": defect.id.to_string(),
                "task_id": defect.follow_up_task_id,
            }),
        );
        Ok(defect)
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn verify_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut defect = load_defect(&state, &path.into_inner())?;
        if defect.status != DefectStatus::Resolved {
            return Err(UnifiedError::conflict(format!(
                "only a resolved defect can be verified (status is \"{}\")",
                defect.status.as_str()
            )));
        }
        let now = Utc::now();
        defect.status = DefectStatus::Verified;
        defect.verified_by = Some(caller.actor.clone());
        defect.verified_at = Some(now);
        defect.updated_at = now;
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.verified",
            json!({
                "defect_id": defect.id.to_string(),
                "project_id": defect.project_id,
                "verified_by": defect.verified_by,
            }),
        );
        Ok(defect)
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}

// Approval handlers


pub(crate) async fn list_approvals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let target_kind_filter = params.filters.get("target_kind").cloned();
        let target_id_filter = params.filters.get("target_id").cloned();
        let approver_filter = params.filters.get("approver_id").cloned();
        let decision_filter = params.filters.get("decision").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let mut items: Vec<Approval> = state
            .approvals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|approval| {
                if let Some(kind) = &target_kind_filter {
                    if approval.target_kind.as_str() != kind {
                        return false;
                    }
                }
                if let Some(target_id) = &target_id_filter {
                    if approval.target_id.to_string() != *target_id {
                        return false;
                    }
                }
                if let Some(approver_id) = &approver_filter {
                    if &approval.approver_id != approver_id {
                        return false;
                    }
                }
                if let Some(decision) = &decision_filter {
                    if approval.decision.as_str() != decision {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if &approval.project_id != project_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|approval| approval.decided_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Daily log handlers


#[cfg(test)]
mod tests {
    use super::*;

    fn sample_defect(status: DefectStatus) -> Defect {
        let now = Utc::now();
        Defect {
            id: Uuid::now_v7(),
            project_id: "project-1".to_string(),
            inspection_id: None,
            task_id: None,
            title: "Crack in beam".to_string(),
            description: None,
            location: None,
            severity: DefectSeverity::High,
            status,
            reported_by: "user-1".to_string(),
            assigned_to: None,
            follow_up_requested: false,
            follow_up_task_id: None,
            resolved_at: None,
            verified_by: None,
            verified_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn empty_update() -> UpdateDefectRequest {
        UpdateDefectRequest {
            title: None,
            description: None,
            location: None,
            severity: None,
            status: None,
            assigned_to: None,
        }
    }

    #[test]
    fn every_non_status_field_counts_as_an_edit() {
        let mut input = empty_update();
        assert!(!defect_non_status_edits_present(&input));
        input.title = Some("New title".to_string());
        assert!(defect_non_status_edits_present(&input));
        input = empty_update();
        input.description = Some(None);
        assert!(defect_non_status_edits_present(&input));
        input = empty_update();
        input.location = Some(Some("Block B".to_string()));
        assert!(defect_non_status_edits_present(&input));
        input = empty_update();
        input.severity = Some(DefectSeverity::Low);
        assert!(defect_non_status_edits_present(&input));
        input = empty_update();
        input.assigned_to = Some(Some("user-2".to_string()));
        assert!(defect_non_status_edits_present(&input));
    }

    #[test]
    fn status_only_update_is_not_a_field_edit() {
        let mut input = empty_update();
        input.status = Some(DefectStatus::Open);
        assert!(!defect_non_status_edits_present(&input));
    }

    #[test]
    fn leaving_resolved_clears_resolved_at() {
        // The S4 staleness defect: Resolved -> InProgress kept
        // the old resolved_at timestamp.
        let mut defect = sample_defect(DefectStatus::Resolved);
        let stamped = Utc::now();
        defect.resolved_at = Some(stamped);
        apply_defect_status_change(&mut defect, DefectStatus::InProgress, Utc::now());
        assert_eq!(defect.status, DefectStatus::InProgress);
        assert_eq!(defect.resolved_at, None);
    }

    #[test]
    fn entering_resolved_stamps_resolved_at() {
        let mut defect = sample_defect(DefectStatus::InProgress);
        let now = Utc::now();
        apply_defect_status_change(&mut defect, DefectStatus::Resolved, now);
        assert_eq!(defect.status, DefectStatus::Resolved);
        assert_eq!(defect.resolved_at, Some(now));
    }

    #[test]
    fn staying_resolved_keeps_the_original_stamp() {
        let mut defect = sample_defect(DefectStatus::Resolved);
        let stamped = Utc::now();
        defect.resolved_at = Some(stamped);
        apply_defect_status_change(&mut defect, DefectStatus::Resolved, Utc::now());
        assert_eq!(defect.resolved_at, Some(stamped));
    }

    #[test]
    fn unrelated_transitions_leave_resolved_at_alone() {
        let mut defect = sample_defect(DefectStatus::Open);
        apply_defect_status_change(&mut defect, DefectStatus::InProgress, Utc::now());
        assert_eq!(defect.resolved_at, None);
    }
}
