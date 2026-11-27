// risk - handlers instances.

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
use super::handlers_plans::*;
use super::handlers_templates::*;
use super::module::*;

pub(crate) async fn create_instance(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    body: web::Json<CreateChecklistInstance>,
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
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let template = state
            .templates
            .get(input.template_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist template not found"))?;
        if let Some(risk_id) = &input.risk_id {
            require_risk_in_scope(&state, risk_id, &scope)?;
        }
        let mut items: Vec<ChecklistTemplateItem> = state
            .template_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.template_id == template.id)
            .collect();
        items.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
        if items.is_empty() {
            return Err(UnifiedError::conflict(
                "Template has no items; add items before creating an instance",
            ));
        }
        let instance = ChecklistInstance {
            id: new_id(),
            template_id: template.id.clone(),
            company_id: require_company_match_for(
                &scope,
                &input.company_id.filter(|value| !value.trim().is_empty()),
            )?, 
            risk_id: input.risk_id.filter(|value| !value.trim().is_empty()),
            title: input
                .title
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| template.title.clone()),
            status: "in_progress".to_string(),
            completed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut instance_items: Vec<ChecklistInstanceItem> = Vec::new();
        for item in &items {
            let instance_item = ChecklistInstanceItem {
                id: new_id(),
                instance_id: instance.id.clone(),
                template_item_id: item.id.clone(),
                title: item.title.clone(),
                required: item.required,
                sort_order: item.sort_order,
                completed: false,
                evidence: String::new(),
                completed_by: None,
                completed_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            state
                .instance_items
                .insert(instance_item.id.clone(), instance_item.clone());
            instance_items.push(instance_item);
        }
        state
            .instances
            .insert(instance.id.clone(), instance.clone());
        Ok(json!({ "instance": instance, "items": instance_items }))
    })();
    respond_created(result)
}

pub(crate) async fn list_instances(
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
    let mut items: Vec<ChecklistInstance> = state
        .instances
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|instance| {
            // Company scope first: a scoped caller sees only
            // their own company's instances; an unresolved
            // caller sees global instances only.
            in_company_scope(&instance.company_id, &scope)
                && params
                    .filters
                    .get("template_id")
                .is_none_or(|template| instance.template_id == *template)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| instance.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("risk_id")
                    .is_none_or(|risk| instance.risk_id.as_deref() == Some(risk.as_str()))
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| instance.status == *status)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_instance(
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
    let result: UnifiedResult<Value> = (|| {
        let instance = state
            .instances
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist instance not found"))?;
        require_record_in_company_scope(&instance.company_id, &scope)?;
        let mut items: Vec<ChecklistInstanceItem> = state
            .instance_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.instance_id == instance.id)
            .collect();
        items.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
        Ok(json!({ "instance": instance, "items": items }))
    })();
    json::respond(result, false)
}

/// When every required item of the instance is complete, mark the
/// instance completed and return it; otherwise return None. An
/// already-completed instance returns None, so the completion
/// event fires exactly once. Optional items never block
/// completion.
pub(crate) fn maybe_complete_instance(
    state: &RiskState,
    instance_id: &str,
) -> Option<ChecklistInstance> {
    let all_required_done = state
        .instance_items
        .iter()
        .filter(|entry| entry.value().instance_id == instance_id)
        .all(|entry| !entry.value().required || entry.value().completed);
    if !all_required_done {
        return None;
    }
    let mut instance = state.instances.get_mut(instance_id)?;
    if instance.status == "completed" {
        return None;
    }
    instance.status = "completed".to_string();
    instance.completed_at = Some(Utc::now());
    instance.updated_at = Utc::now();
    Some(instance.clone())
}

/// Complete one checklist instance item with evidence. When every
/// required item of the instance is complete, the instance
/// completes and emits `risk.checklist_completed`.
pub(crate) async fn complete_instance_item(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CompleteChecklistItem>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let (instance_id, item_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        // Items carry no company of their own: they are scoped
        // through their parent instance.
        require_instance_in_scope(&state, &instance_id, &scope)?;
        let mut item = state
            .instance_items
            .get_mut(item_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.instance_id != instance_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        item.completed = true;
        item.evidence = body
            .evidence
            .clone()
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        item.completed_by = actor_id.clone();
        item.completed_at = Some(Utc::now());
        item.updated_at = Utc::now();
        let updated_item = item.clone();
        drop(item);
        // Completing the last required item completes the instance.
        let completed_instance = maybe_complete_instance(&state, &instance_id);
        if let Some(instance) = completed_instance {
            state.emit(
                "risk.checklist_completed",
                json!({
                    "instance_id": instance.id,
                    "template_id": instance.template_id,
                    "company_id": instance.company_id,
                    "risk_id": instance.risk_id,
                    "title": instance.title,
                    "completed_by": actor_id,
                }),
            );
        }
        Ok(json!({ "item": updated_item }))
    })();
    json::respond(result, false)
}

pub(crate) async fn reopen_instance_item(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
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
    let (instance_id, item_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        // Items carry no company of their own: they are scoped
        // through their parent instance.
        require_instance_in_scope(&state, &instance_id, &scope)?;
        let mut item = state
            .instance_items
            .get_mut(item_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.instance_id != instance_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        item.completed = false;
        item.evidence = String::new();
        item.completed_by = None;
        item.completed_at = None;
        item.updated_at = Utc::now();
        let updated_item = item.clone();
        drop(item);
        if let Some(mut instance) = state.instances.get_mut(instance_id.as_str()) {
            if instance.status == "completed" {
                instance.status = "in_progress".to_string();
                instance.completed_at = None;
                instance.updated_at = Utc::now();
            }
        }
        Ok(json!({ "item": updated_item }))
    })();
    json::respond(result, false)
}

// Hook handlers

/// Answer `risk.review_check_requested` by running the review-date
/// scan and reporting how many alerts were raised.
pub(crate) fn on_review_check_requested(state: Arc<RiskState>, ctx: &mut HookContext) {
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let raised = state.run_review_check(company_id.as_deref());
    ctx.add_result(json!({
        "company_id": company_id,
        "alerts_raised": raised,
    }));
}
#[cfg(test)]
mod tests {
    use super::*;

    fn instance(instance_id: &str) -> ChecklistInstance {
        ChecklistInstance {
            id: instance_id.to_string(),
            template_id: "template-1".to_string(),
            company_id: None,
            risk_id: None,
            title: "Checklist".to_string(),
            status: "in_progress".to_string(),
            completed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn instance_item(
        instance_id: &str,
        item_id: &str,
        required: bool,
        completed: bool,
    ) -> ChecklistInstanceItem {
        ChecklistInstanceItem {
            id: item_id.to_string(),
            instance_id: instance_id.to_string(),
            template_item_id: "template-item-1".to_string(),
            title: "Item".to_string(),
            required,
            sort_order: 0,
            completed,
            evidence: String::new(),
            completed_by: None,
            completed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn checklist_completes_when_all_required_items_are_done() {
        let state = RiskState::new();
        state
            .instances
            .insert("inst-1".to_string(), instance("inst-1"));
        state.instance_items.insert(
            "item-1".to_string(),
            instance_item("inst-1", "item-1", true, false),
        );
        state.instance_items.insert(
            "item-2".to_string(),
            instance_item("inst-1", "item-2", false, false),
        );
        // A required item still open: the instance stays open.
        assert!(maybe_complete_instance(&state, "inst-1").is_none());
        // Completing the required item completes the instance even
        // though the optional item is still open.
        if let Some(mut item) = state.instance_items.get_mut("item-1") {
            item.completed = true;
        }
        let completed =
            maybe_complete_instance(&state, "inst-1").expect("instance completes");
        assert_eq!(completed.status, "completed");
        assert!(completed.completed_at.is_some());
        // A second call does not complete (or re-emit) again.
        assert!(maybe_complete_instance(&state, "inst-1").is_none());
    }
}
