// risk - handlers templates.

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
use super::handlers_instances::*;
use super::module::*;

// Handlers: compliance checklists

pub(crate) async fn list_templates(
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
    let mut items: Vec<ChecklistTemplate> = state
        .templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| {
            matches_search(
                &format!("{} {} {}", template.title, template.description, template.category),
                &params,
            ) && params
                .filters
                .get("category")
                .is_none_or(|category| template.category == *category)
        })
        .collect();
    items.sort_by(|a, b| a.title.cmp(&b.title));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_template(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    body: web::Json<CreateChecklistTemplate>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<ChecklistTemplate> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Template title must not be empty"));
        }
        if input.category.trim().is_empty() {
            return Err(UnifiedError::bad_request("Template category must not be empty"));
        }
        let template = ChecklistTemplate {
            id: new_id(),
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            category: input.category.trim().to_string(),
            created_by: actor_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.templates.insert(template.id.clone(), template.clone());
        Ok(template)
    })();
    respond_created(result)
}

pub(crate) async fn get_template(
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
    let result: UnifiedResult<ChecklistTemplate> = state
        .templates
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Checklist template not found"));
    json::respond(result, false)
}

pub(crate) async fn update_template(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateChecklistTemplate>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<ChecklistTemplate> = (|| {
        let input = body.into_inner();
        let mut template = state
            .templates
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist template not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Template title must not be empty"));
            }
            template.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            template.description = description.trim().to_string();
        }
        if let Some(category) = input.category {
            if category.trim().is_empty() {
                return Err(UnifiedError::bad_request("Template category must not be empty"));
            }
            template.category = category.trim().to_string();
        }
        template.updated_at = Utc::now();
        Ok(template.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_template(
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
    let template_id = path.as_str().to_string();
    if state
        .instances
        .iter()
        .any(|entry| entry.value().template_id == template_id)
    {
        return json::error_response(
            &UnifiedError::conflict(
                "Template still has checklist instances; delete or complete them first",
            ),
            false,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .templates
            .remove(&template_id)
            .map(|_| json!({ "deleted": true, "id": &template_id }))
            .ok_or_else(|| UnifiedError::not_found("Checklist template not found"))
    })();
    if result.is_ok() {
        state
            .template_items
            .retain(|_, item| item.template_id != template_id);
    }
    json::respond(result, false)
}

pub(crate) async fn list_template_items(
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
    let template_id = path.as_str().to_string();
    if !state.templates.contains_key(template_id.as_str()) {
        return json::error_response(
            &UnifiedError::not_found("Checklist template not found"),
            false,
        );
    }
    let mut items: Vec<ChecklistTemplateItem> = state
        .template_items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.template_id == template_id)
        .collect();
    items.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn add_template_item(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateChecklistTemplateItem>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let template_id = path.as_str().to_string();
    let result: UnifiedResult<ChecklistTemplateItem> = (|| {
        if !state.templates.contains_key(template_id.as_str()) {
            return Err(UnifiedError::not_found("Checklist template not found"));
        }
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Item title must not be empty"));
        }
        let item = ChecklistTemplateItem {
            id: new_id(),
            template_id,
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            required: input.required.unwrap_or(true),
            sort_order: input.sort_order.unwrap_or(0),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .template_items
            .insert(item.id.clone(), item.clone());
        Ok(item)
    })();
    respond_created(result)
}

pub(crate) async fn delete_template_item(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let (template_id, item_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        match state.template_items.get(item_id.as_str()) {
            Some(item) if item.template_id == template_id => {
                state.template_items.remove(item_id.as_str());
                Ok(json!({ "deleted": true, "id": item_id }))
            }
            _ => Err(UnifiedError::not_found("Checklist template item not found")),
        }
    })();
    json::respond(result, false)
}
