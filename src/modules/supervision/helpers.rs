// supervision module -- Shared helpers: callers, permissions, responses, and lookups.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use unified_core::json;
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group B).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only: no per-record company check is applied.
/// Real partitioning needs a `tenant_id` on the records, a C11
/// follow-up.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

// Projects-module hook queries
// ---------------------------------------------------------------------------

/// Minimal project data returned by the `project.get_requested`
/// query. Only the fields this module needs: identity, name,
/// status, and whether the project is archived. Progress lives in
/// the projects module and is read separately (see
/// `query_project_progress`).
#[derive(Debug, Clone)]
pub(crate) struct ProjectRef {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) archived: bool,
}

/// Ask the projects module whether a project exists, through the
/// `project.get_requested` hook query. Fails closed: when no
/// handler answers (the projects module has not registered the
/// query handler yet), the call errors instead of assuming the
/// project exists. Callers surface this as an internal error so
/// operators can see the missing wiring.
pub(crate) fn query_project(ctx: &AppContext, project_id: &str) -> UnifiedResult<ProjectRef> {
    let context = ctx.hooks.emit(
        PROJECT_GET_REQUESTED,
        json!({ "project_id": project_id }),
    );
    let Some(answer) = context.results.into_iter().next() else {
        return Err(UnifiedError::internal(
            "project.get_requested was not answered: the projects module has not registered a project lookup handler yet",
        ));
    };
    let exists = answer
        .get("exists")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !exists {
        return Err(UnifiedError::not_found(format!(
            "Project not found: {project_id}"
        )));
    }
    let project = answer.get("project").cloned().unwrap_or(Value::Null);
    Ok(ProjectRef {
        id: project
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or(project_id)
            .to_string(),
        name: project
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        status: project
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        archived: project
            .get("archived")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    })
}

/// Ask the projects module for the current progress of a project
/// (optionally one milestone) through `project.progress_requested`.
/// Returns `None` when no handler answers -- the caller records
/// the snapshot as unverified instead of inventing a number.
pub(crate) fn query_project_progress(
    ctx: &AppContext,
    project_id: &str,
    milestone_id: Option<&str>,
) -> Option<f64> {
    let context = ctx.hooks.emit(
        PROJECT_PROGRESS_REQUESTED,
        json!({ "project_id": project_id, "milestone_id": milestone_id }),
    );
    let answer = context.results.into_iter().next()?;
    let percent = answer.get("percent_complete")?.as_f64()?;
    if percent.is_finite() && (0.0..=100.0).contains(&percent) {
        Some((percent * 100.0).round() / 100.0)
    } else {
        None
    }
}

/// Ask the projects module whether a task exists, through
/// `project.task.get_requested`. Fails closed like `query_project`.

pub(crate) fn query_task_exists(ctx: &AppContext, task_id: &str) -> UnifiedResult<()> {
    let context = ctx.hooks.emit(
        PROJECT_TASK_GET_REQUESTED,
        json!({ "task_id": task_id }),
    );
    let Some(answer) = context.results.into_iter().next() else {
        return Err(UnifiedError::internal(
            "project.task.get_requested was not answered: the projects module has not registered a task lookup handler yet",
        ));
    };
    let exists = answer
        .get("exists")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(UnifiedError::not_found(format!("Task not found: {task_id}")))
    }
}

/// Ask the projects module to create a follow-up task. Returns the
/// new task id when the projects module answers with one, `None`
/// when it answers that nothing was created, and an error when no
/// handler answers at all (the caller records the request as
/// pending instead of claiming success).
pub(crate) fn request_follow_up_task(
    ctx: &AppContext,
    project_id: &str,
    title: &str,
    description: &str,
    inspection_id: Option<Uuid>,
    defect_id: Option<Uuid>,
    requested_by: &str,
) -> UnifiedResult<Option<String>> {
    let context = ctx.hooks.emit(
        PROJECT_TASK_CREATE_REQUESTED,
        json!({
            "project_id": project_id,
            "title": title,
            "description": description,
            "source": "supervision",
            "inspection_id": inspection_id.map(|id| id.to_string()),
            "defect_id": defect_id.map(|id| id.to_string()),
            "requested_by": requested_by,
        }),
    );
    let Some(answer) = context.results.into_iter().next() else {
        return Err(UnifiedError::internal(
            "project.task.create_requested was not answered: the projects module has not registered a follow-up task handler yet",
        ));
    };
    if answer
        .get("created")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(answer
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(str::to_string))
    } else {
        Ok(None)
    }
}


pub(crate) fn record_approval(
    state: &SupervisionState,
    target_kind: ApprovalTargetKind,
    target_id: Uuid,
    project_id: &str,
    approver_id: &str,
    decision: ApprovalDecision,
    comment: Option<String>,
) -> Approval {
    let now = Utc::now();
    let approval = Approval {
        id: Uuid::now_v7(),
        target_kind,
        target_id,
        project_id: project_id.to_string(),
        approver_id: approver_id.to_string(),
        decision,
        comment,
        decided_at: now,
        created_at: now,
    };
    state.approvals.insert(approval.id, approval.clone());
    state.emit(
        "supervision.approval.recorded",
        json!({
            "approval_id": approval.id.to_string(),
            "target_kind": target_kind.as_str(),
            "target_id": target_id.to_string(),
            "project_id": project_id,
            "approver_id": approver_id,
            "decision": decision.as_str(),
        }),
    );
    approval
}


pub(crate) fn items_for_inspection(state: &SupervisionState, inspection_id: Uuid) -> Vec<InspectionItemResult> {
    let mut items: Vec<InspectionItemResult> = state
        .inspection_items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.inspection_id == inspection_id)
        .collect();
    items.sort_by_key(|item| item.created_at);
    items
}

pub(crate) fn load_assignment(state: &SupervisionState, raw_id: &str) -> UnifiedResult<SupervisionAssignment> {
    let id = parse_uuid(raw_id, "assignment")?;
    state
        .assignments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Assignment not found: {id}")))
}

pub(crate) fn load_template(state: &SupervisionState, raw_id: &str) -> UnifiedResult<InspectionTemplate> {
    let id = parse_uuid(raw_id, "template")?;
    state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Template not found: {id}")))
}

pub(crate) fn load_inspection(state: &SupervisionState, raw_id: &str) -> UnifiedResult<Inspection> {
    let id = parse_uuid(raw_id, "inspection")?;
    state
        .inspections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Inspection not found: {id}")))
}

pub(crate) fn load_report(state: &SupervisionState, raw_id: &str) -> UnifiedResult<ProgressReport> {
    let id = parse_uuid(raw_id, "progress report")?;
    state
        .progress_reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Progress report not found: {id}")))
}

pub(crate) fn load_defect(state: &SupervisionState, raw_id: &str) -> UnifiedResult<Defect> {
    let id = parse_uuid(raw_id, "defect")?;
    state
        .defects
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Defect not found: {id}")))
}

pub(crate) fn load_daily_log(state: &SupervisionState, raw_id: &str) -> UnifiedResult<DailyLog> {
    let id = parse_uuid(raw_id, "daily log")?;
    state
        .daily_logs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Daily log not found: {id}")))
}

pub(crate) fn load_photo(state: &SupervisionState, raw_id: &str) -> UnifiedResult<Photo> {
    let id = parse_uuid(raw_id, "photo")?;
    state
        .photos
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Photo not found: {id}")))
}

pub(crate) fn build_template_items(inputs: &[TemplateItemInput]) -> UnifiedResult<Vec<TemplateItem>> {
    if inputs.len() > 200 {
        return Err(UnifiedError::bad_request(
            "a template may hold at most 200 items",
        ));
    }
    let mut items = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        items.push(TemplateItem {
            id: Uuid::now_v7(),
            title: require_text(&input.title, "items[].title", MAX_TITLE_LEN)?,
            description: opt_text(&input.description, "items[].description", MAX_TEXT_LEN)?,
            position: (index + 1) as u32,
            required: input.required,
        });
    }
    Ok(items)
}

/// Full inspection view: the inspection plus its item results.
pub(crate) fn inspection_view(state: &SupervisionState, inspection: &Inspection) -> Value {
    let mut value = serde_json::to_value(inspection).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "items".to_string(),
            serde_json::to_value(items_for_inspection(state, inspection.id))
                .unwrap_or(Value::Null),
        );
    }
    value
}

/// Full progress-report view: the report plus the derived
/// variance (reported minus planned), when a planned percent is
/// recorded. The variance is computed here, on read, and is never
/// stored as a second truth.
pub(crate) fn report_view(report: &ProgressReport) -> Value {
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let variance = report
            .planned_percent_complete
            .map(|planned| (report.reported_percent_complete - planned))
            .map(|value| (value * 100.0).round() / 100.0);
        map.insert(
            "variance_points".to_string(),
            variance
                .map(|value| Value::from(value))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "source".to_string(),
            Value::String(report.source.as_str().to_string()),
        );
    }
    value
}
