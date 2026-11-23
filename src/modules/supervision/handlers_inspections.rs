// supervision module -- HTTP handlers for inspections and item results.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

pub(crate) async fn list_inspections(
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
        let template_filter = params.filters.get("template_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let inspector_filter = params.filters.get("inspector_id").cloned();
        let overdue_only = params
            .filters
            .get("overdue")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .inspections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|inspection| {
                if let Some(project_id) = &project_filter {
                    if &inspection.project_id != project_id {
                        return false;
                    }
                }
                if let Some(template_id) = &template_filter {
                    if inspection.template_id.map(|id| id.to_string()) != Some(template_id.clone()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if inspection.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(inspector_id) = &inspector_filter {
                    if &inspection.inspector_id != inspector_id {
                        return false;
                    }
                }
                if overdue_only {
                    if inspection.scheduled_for >= now
                        || !matches!(
                            inspection.status,
                            InspectionStatus::Scheduled | InspectionStatus::InProgress
                        )
                    {
                        return false;
                    }
                }
                true
            })
            .map(|inspection| inspection_view(&state, &inspection))
            .collect();
        items.sort_by(|a, b| {
            a.get("scheduled_for")
                .and_then(Value::as_str)
                .cmp(&b.get("scheduled_for").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateInspectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        let template = match input.template_id {
            Some(id) => Some(load_template(&state, &id.to_string())?),
            None => None,
        };
        if let Some(template) = &template {
            if let Some(scope) = &template.project_id {
                if scope != &project_id {
                    return Err(UnifiedError::bad_request(
                        "template belongs to a different project",
                    ));
                }
            }
        }
        let inspector_id = require_id(&input.inspector_id, "inspector_id")?;
        let now = Utc::now();
        let inspection = Inspection {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            template_id: template.as_ref().map(|template| template.id),
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            location: opt_text(&input.location, "location", MAX_LOCATION_LEN)?,
            inspector_id,
            status: InspectionStatus::Scheduled,
            scheduled_for: input.scheduled_for,
            conducted_at: None,
            verdict: None,
            summary: opt_text(&input.summary, "summary", MAX_TEXT_LEN)?,
            signed_by: None,
            signed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        // Copy the template checklist into the inspection as
        // pending item results, so later template edits cannot
        // rewrite a conducted inspection.
        if let Some(template) = &template {
            for item in &template.items {
                let result = InspectionItemResult {
                    id: Uuid::now_v7(),
                    inspection_id: inspection.id,
                    template_item_id: Some(item.id),
                    title: item.title.clone(),
                    result: ItemResult::Pending,
                    notes: None,
                    follow_up_requested: false,
                    follow_up_task_id: None,
                    created_at: now,
                    updated_at: now,
                };
                state.inspection_items.insert(result.id, result);
            }
        }
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.created",
            json!({
                "inspection_id": inspection.id.to_string(),
                "project_id": inspection.project_id,
                "template_id": inspection.template_id.map(|id| id.to_string()),
                "inspector_id": inspection.inspector_id,
            }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let inspection = load_inspection(&state, &path.into_inner())?;
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Whether an inspection in this status accepts PATCH edits
/// (field edits and cancellation). Submitted, approved, and
/// cancelled inspections are terminal for edits.
pub(crate) fn inspection_is_editable(status: InspectionStatus) -> bool {
    matches!(
        status,
        InspectionStatus::Scheduled | InspectionStatus::InProgress | InspectionStatus::Returned
    )
}

/// Whether a PATCH `status` value is accepted. Only `cancelled`
/// may be set through PATCH; the other transitions have their
/// own endpoints (submit / complete / approve / return).
pub(crate) fn inspection_patch_status_allowed(status: InspectionStatus) -> bool {
    status == InspectionStatus::Cancelled
}

pub(crate) async fn update_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInspectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !inspection_is_editable(inspection.status) {
            return Err(UnifiedError::conflict(format!(
                "only a scheduled, in-progress, or returned inspection can be updated (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(status) = input.status {
            if !inspection_patch_status_allowed(status) {
                return Err(UnifiedError::conflict(
                    "inspection status cannot be changed through PATCH; cancel an inspection by sending status \"cancelled\"",
                ));
            }
            inspection.status = InspectionStatus::Cancelled;
        }
        if let Some(title) = input.title {
            inspection.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(location) = input.location {
            inspection.location = match location {
                Some(raw) => Some(require_text(&raw, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(inspector_id) = input.inspector_id {
            inspection.inspector_id = require_id(&inspector_id, "inspector_id")?;
        }
        if let Some(scheduled_for) = input.scheduled_for {
            inspection.scheduled_for = scheduled_for;
        }
        if let Some(summary) = input.summary {
            inspection.summary = match summary {
                Some(raw) => Some(require_text(&raw, "summary", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        inspection.updated_at = Utc::now();
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.updated",
            json!({ "inspection_id": inspection.id.to_string() }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn submit_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !matches!(
            inspection.status,
            InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "only an in-progress or returned inspection can be submitted (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        // Every checklist item must have a result before submit;
        // a pending item means the inspection is not finished.
        let pending = items_for_inspection(&state, inspection.id)
            .iter()
            .filter(|item| item.result == ItemResult::Pending)
            .count();
        if pending > 0 {
            return Err(UnifiedError::conflict(format!(
                "{pending} checklist item(s) still have no result"
            )));
        }
        if inspection.verdict.is_none() {
            return Err(UnifiedError::conflict(
                "an inspection must be completed with a verdict before it can be submitted",
            ));
        }
        inspection.status = InspectionStatus::Submitted;
        inspection.updated_at = Utc::now();
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.submitted",
            json!({
                "inspection_id": inspection.id.to_string(),
                "project_id": inspection.project_id,
                "inspector_id": inspection.inspector_id,
            }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn complete_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CompleteInspectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !matches!(
            inspection.status,
            InspectionStatus::Scheduled | InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "only a scheduled, in-progress, or returned inspection can be completed (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        let input = body.into_inner();
        // The signature must come from the caller themselves: the
        // inspector attests their own inspection.
        if input.signed_by.trim() != caller.actor {
            return Err(UnifiedError::forbidden(
                "signed_by must match the caller (X-User-Id)",
            ));
        }
        if let Some(summary) = input.summary {
            inspection.summary = Some(require_text(&summary, "summary", MAX_TEXT_LEN)?);
        }
        let now = Utc::now();
        inspection.verdict = Some(input.verdict);
        inspection.conducted_at = Some(now);
        inspection.signed_by = Some(input.signed_by.trim().to_string());
        inspection.signed_at = Some(now);
        inspection.status = InspectionStatus::InProgress;
        inspection.updated_at = now;
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.completed",
            json!({
                "inspection_id": inspection.id.to_string(),
                "project_id": inspection.project_id,
                "verdict": input.verdict.as_str(),
                "signed_by": inspection.signed_by,
            }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn decide_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
    decision: ApprovalDecision,
) -> UnifiedResult<Value> {
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    let caller = caller(&req)?;
    require_permission(&ctx, &caller.role, PERM_SUPERVISION_APPROVE)?;
    let mut inspection = load_inspection(&state, &path.into_inner())?;
    if inspection.status != InspectionStatus::Submitted {
        return Err(UnifiedError::conflict(format!(
            "only a submitted inspection can be decided (status is \"{}\")",
            inspection.status.as_str()
        )));
    }
    let input = body.into_inner();
    let comment = opt_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
    inspection.status = match decision {
        ApprovalDecision::Approved => InspectionStatus::Approved,
        ApprovalDecision::Returned => InspectionStatus::Returned,
    };
    inspection.updated_at = Utc::now();
    state.inspections.insert(inspection.id, inspection.clone());
    let approval = record_approval(
        &state,
        ApprovalTargetKind::Inspection,
        inspection.id,
        &inspection.project_id,
        &caller.actor,
        decision,
        comment,
    );
    state.emit(
        "supervision.inspection.decided",
        json!({
            "inspection_id": inspection.id.to_string(),
            "project_id": inspection.project_id,
            "decision": decision.as_str(),
            "approval_id": approval.id.to_string(),
        }),
    );
    let mut view = inspection_view(&state, &inspection);
    if let Value::Object(map) = &mut view {
        map.insert(
            "approval".to_string(),
            serde_json::to_value(approval).unwrap_or(Value::Null),
        );
    }
    Ok(view)
}


pub(crate) async fn approve_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_inspection(ctx, state, req, path, body, ApprovalDecision::Approved).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn return_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_inspection(ctx, state, req, path, body, ApprovalDecision::Returned).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn record_item_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ItemResultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionItemResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let (inspection_id, item_id) = path.into_inner();
        let inspection = load_inspection(&state, &inspection_id)?;
        if !matches!(
            inspection.status,
            InspectionStatus::Scheduled | InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "results can only be recorded on a scheduled, in-progress, or returned inspection (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        let id = parse_uuid(&item_id, "inspection item")?;
        let mut item = state
            .inspection_items
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|item| item.inspection_id == inspection.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Inspection item not found: {id}")))?;
        let input = body.into_inner();
        item.result = input.result;
        item.notes = opt_text(&input.notes, "notes", MAX_TEXT_LEN)?;
        item.updated_at = Utc::now();
        state.inspection_items.insert(item.id, item.clone());
        if inspection.status == InspectionStatus::Scheduled {
            let mut updated = inspection.clone();
            updated.status = InspectionStatus::InProgress;
            updated.updated_at = Utc::now();
            state.inspections.insert(updated.id, updated);
        }
        state.emit(
            "supervision.inspection.item_resulted",
            json!({
                "inspection_id": inspection.id.to_string(),
                "item_id": item.id.to_string(),
                "result": item.result.as_str(),
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn follow_up_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionItemResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let (inspection_id, item_id) = path.into_inner();
        let inspection = load_inspection(&state, &inspection_id)?;
        let id = parse_uuid(&item_id, "inspection item")?;
        let mut item = state
            .inspection_items
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|item| item.inspection_id == inspection.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Inspection item not found: {id}")))?;
        if item.result != ItemResult::Fail {
            return Err(UnifiedError::conflict(
                "a follow-up task can only be requested for a failed item",
            ));
        }
        if item.follow_up_requested && item.follow_up_task_id.is_some() {
            return Err(UnifiedError::conflict(
                "a follow-up task was already created for this item",
            ));
        }
        let title = format!("Follow-up: {}", item.title);
        let description = format!(
            "Failed checklist item \"{}\" in inspection \"{}\" (project {}).{}",
            item.title,
            inspection.title,
            inspection.project_id,
            item.notes
                .as_ref()
                .map(|notes| format!(" Notes: {notes}"))
                .unwrap_or_default()
        );
        let task_id = match request_follow_up_task(
            &ctx,
            &inspection.project_id,
            &title,
            &description,
            Some(inspection.id),
            None,
            &caller.actor,
        ) {
            Ok(task_id) => task_id,
            Err(error) => {
                // No handler answered: record the request as
                // pending instead of failing the supervision
                // record -- the follow-up itself is owned by the
                // projects module, not by this call.
                item.follow_up_requested = true;
                item.updated_at = Utc::now();
                state.inspection_items.insert(item.id, item.clone());
                state.emit(
                    "supervision.inspection.follow_up_unanswered",
                    json!({
                        "inspection_id": inspection.id.to_string(),
                        "item_id": item.id.to_string(),
                        "error": error.to_string(),
                    }),
                );
                return Ok(item);
            }
        };
        item.follow_up_requested = true;
        item.follow_up_task_id = task_id;
        item.updated_at = Utc::now();
        state.inspection_items.insert(item.id, item.clone());
        state.emit(
            "supervision.inspection.follow_up_requested",
            json!({
                "inspection_id": inspection.id.to_string(),
                "item_id": item.id.to_string(),
                "task_id": item.follow_up_task_id,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Progress report handlers
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Progress report handlers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_statuses_are_scheduled_in_progress_and_returned() {
        assert!(inspection_is_editable(InspectionStatus::Scheduled));
        assert!(inspection_is_editable(InspectionStatus::InProgress));
        assert!(inspection_is_editable(InspectionStatus::Returned));
        assert!(!inspection_is_editable(InspectionStatus::Submitted));
        assert!(!inspection_is_editable(InspectionStatus::Approved));
        assert!(!inspection_is_editable(InspectionStatus::Cancelled));
    }

    #[test]
    fn only_cancelled_is_accepted_as_patch_status() {
        assert!(inspection_patch_status_allowed(
            InspectionStatus::Cancelled
        ));
        for status in [
            InspectionStatus::Scheduled,
            InspectionStatus::InProgress,
            InspectionStatus::Submitted,
            InspectionStatus::Approved,
            InspectionStatus::Returned,
        ] {
            assert!(
                !inspection_patch_status_allowed(status),
                "{status:?} must not be settable through PATCH"
            );
        }
    }

    #[test]
    fn cancelled_is_reachable_from_every_editable_status() {
        // The Cancelled variant must be settable from each status
        // update_inspection accepts, so the documented lifecycle
        // ("cancelled is reachable from scheduled / in_progress /
        // returned") is real and not just a doc comment.
        for status in [
            InspectionStatus::Scheduled,
            InspectionStatus::InProgress,
            InspectionStatus::Returned,
        ] {
            assert!(inspection_is_editable(status));
            assert!(inspection_patch_status_allowed(
                InspectionStatus::Cancelled
            ));
        }
    }
}
