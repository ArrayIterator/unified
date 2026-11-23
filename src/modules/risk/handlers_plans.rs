// risk — handlers plans.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_risks::*;
use super::handlers_templates::*;
use super::handlers_instances::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Handlers: mitigation plans and tasks
// ---------------------------------------------------------------------------

pub(crate) async fn list_plans_for_risk(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_viewer_id, viewer_role) = viewer_of(&req);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let risk_id = path.as_str().to_string();
    if let Err(error) = require_risk_in_scope(&state, &risk_id, &scope) {
        return json::error_response(&error, false);
    }
    let mut items: Vec<MitigationPlan> = state
        .plans
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|plan| {
            plan.risk_id == risk_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| plan.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_plan(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateMitigationPlan>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let risk_id = path.as_str().to_string();
    let result: UnifiedResult<MitigationPlan> = (|| {
        require_risk_in_scope(&state, &risk_id, &scope)?;
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Plan title must not be empty"));
        }
        let status = input.status.unwrap_or_else(|| "draft".to_string());
        if !PLAN_STATUSES.contains(&status.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "status must be one of: {}",
                PLAN_STATUSES.join(", ")
            )));
        }
        let plan = MitigationPlan {
            id: new_id(),
            risk_id,
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            target_date: input.target_date,
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.plans.insert(plan.id.clone(), plan.clone());
        state.emit(
            "risk.mitigation_plan_created",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        Ok(plan)
    })();
    respond_created(result)
}

pub(crate) async fn get_plan(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<MitigationPlan> = (|| {
        let plan = state.get_plan(path.as_str())?;
        require_risk_in_scope(&state, &plan.risk_id, &scope)?;
        Ok(plan)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_plan(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateMitigationPlan>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<MitigationPlan> = (|| {
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !PLAN_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "status must be one of: {}",
                    PLAN_STATUSES.join(", ")
                )));
            }
        }
        let mut plan = state
            .plans
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Mitigation plan not found"))?;
        require_risk_in_scope(&state, &plan.risk_id, &scope)?;
        // Validate the transition before mutating anything: the
        // guard writes straight into the stored entry, so a
        // rejected move must not leave a half-applied update.
        if let Some(status) = &input.status {
            if !valid_plan_transition(&plan.status, status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Mitigation plan status cannot move from \"{}\" to \"{status}\"",
                    plan.status
                )));
            }
        }
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Plan title must not be empty"));
            }
            plan.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            plan.description = description.trim().to_string();
        }
        if let Some(target_date) = input.target_date {
            plan.target_date = Some(target_date);
        }
        if let Some(status) = input.status {
            plan.status = status;
        }
        plan.updated_at = Utc::now();
        let updated = plan.clone();
        drop(plan);
        state.emit(
            "risk.mitigation_plan_updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_plan(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let plan_id = path.as_str().to_string();
    let result: UnifiedResult<Value> = (|| {
        require_plan_risk_in_scope(&state, &plan_id, &scope)?;
        state
            .plans
            .remove(&plan_id)
            .map(|_| json!({ "deleted": true, "id": &plan_id }))
            .ok_or_else(|| UnifiedError::not_found("Mitigation plan not found"))
    })();
    if result.is_ok() {
        state.tasks.retain(|_, task| task.plan_id != plan_id);
        state.emit(
            "risk.mitigation_plan_deleted",
            json!({ "plan_id": plan_id }),
        );
    }
    json::respond(result, false)
}

pub(crate) async fn list_tasks_for_plan(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_viewer_id, viewer_role) = viewer_of(&req);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let plan_id = path.as_str().to_string();
    if let Err(error) = require_plan_risk_in_scope(&state, &plan_id, &scope) {
        return json::error_response(&error, false);
    }
    let mut items: Vec<MitigationTask> = state
        .tasks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|task| {
            task.plan_id == plan_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| task.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_task(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateMitigationTask>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let plan_id = path.as_str().to_string();
    let result: UnifiedResult<MitigationTask> = (|| {
        let plan = require_plan_risk_in_scope(&state, &plan_id, &scope)?;
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Task title must not be empty"));
        }
        let task = MitigationTask {
            id: new_id(),
            plan_id: plan.id.clone(),
            risk_id: plan.risk_id.clone(),
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            assignee_id: input
                .assignee_id
                .filter(|value| !value.trim().is_empty()),
            status: "requested".to_string(),
            due_date: input.due_date,
            external_project_task_id: None,
            awaiting_projects_handler: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // Ask the Projects module to create the backing task. When
        // it answers with a project task id, the task is linked;
        // when it does not (no handler wired yet), the task stays
        // `requested` with `awaiting_projects_handler: true`.
        let project_task_id = state.request_project_task(&task, input.project_id.as_deref());
        let mut task = task;
        if let Some(project_task_id) = project_task_id {
            task.external_project_task_id = Some(project_task_id);
            task.awaiting_projects_handler = false;
        }
        state.tasks.insert(task.id.clone(), task.clone());
        Ok(task)
    })();
    respond_created(result)
}

pub(crate) async fn update_task(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateMitigationTask>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<MitigationTask> = (|| {
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !TASK_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "status must be one of: {}",
                    TASK_STATUSES.join(", ")
                )));
            }
        }
        let mut task = state
            .tasks
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Mitigation task not found"))?;
        require_risk_in_scope(&state, &task.risk_id, &scope)?;
        // Validate the transition before mutating anything: the
        // guard writes straight into the stored entry, so a
        // rejected move must not leave a half-applied update.
        if let Some(status) = &input.status {
            if !valid_task_transition(&task.status, status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Mitigation task status cannot move from \"{}\" to \"{status}\"",
                    task.status
                )));
            }
        }
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Task title must not be empty"));
            }
            task.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            task.description = description.trim().to_string();
        }
        if let Some(assignee_id) = input.assignee_id {
            task.assignee_id = if assignee_id.trim().is_empty() {
                None
            } else {
                Some(assignee_id.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            task.status = status;
        }
        if let Some(due_date) = input.due_date {
            task.due_date = Some(due_date);
        }
        task.updated_at = Utc::now();
        let updated = task.clone();
        drop(task);
        state.emit(
            "risk.mitigation_task_updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_task(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let task_id = path.as_str().to_string();
    let result: UnifiedResult<Value> = (|| {
        require_task_risk_in_scope(&state, &task_id, &scope)?;
        state
            .tasks
            .remove(&task_id)
            .map(|_| json!({ "deleted": true, "id": &task_id }))
            .ok_or_else(|| UnifiedError::not_found("Mitigation task not found"))
    })();
    if result.is_ok() {
        state.emit(
            "risk.mitigation_task_deleted",
            json!({ "task_id": task_id }),
        );
    }
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: review check and alerts history
// ---------------------------------------------------------------------------

pub(crate) async fn review_check(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    body: Option<web::Json<Value>>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    // `run_review_check` treats a missing company as "all
    // companies" — its own tests pin that shape and a system path
    // may rely on it — so the gate lives HERE, at the handler: the
    // company to scan is resolved from the caller scope first, and
    // an unresolved caller may never run the scan at all.
    let requested_company = body
        .and_then(|body| {
            body.get("company_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty());
    let company_id = match require_company_match_for(&scope, &requested_company) {
        Ok(Some(company)) => company,
        Ok(None) => {
            return json::error_response(
                &UnifiedError::forbidden(
                    "A company scope is required to run a review check",
                ),
                false,
            );
        }
        Err(error) => return json::error_response(&error, false),
    };
    let raised = state.run_review_check(Some(&company_id));
    json::success_response(
        json!({
            "company_id": company_id,
            "alerts_raised": raised,
        }),
        200,
    )
}

pub(crate) async fn list_alerts(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_viewer_id, viewer_role) = viewer_of(&req);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let mut items: Vec<RiskAlert> = state
        .alerts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|alert| {
            // Company scope first: a scoped caller sees only
            // their own company's alerts; an unresolved caller
            // sees global alerts only.
            in_company_scope(&alert.company_id, &scope)
                && params
                    .filters
                    .get("risk_id")
                .is_none_or(|risk| alert.risk_id == *risk)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| alert.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| alert.kind == *kind)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}
