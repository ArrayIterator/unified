// Properties — Plan handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Plan handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let property = load_property(&state, &path.into_inner())?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items = state.plans_of_property(property.id);
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyPlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_CREATE)?;
        let property = load_property(&state, &path.into_inner())?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        if matches!(property.status, PropertyStatus::Archived | PropertyStatus::SoldOut) {
            return Err(UnifiedError::conflict(format!(
                "Plans cannot be added to a {} property",
                property.status.as_str()
            )));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = input
            .description
            .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
            .transpose()?;
        let (geometry, geometry_hash) = match input.geometry {
            Some(value) => {
                let validated = validate_geometry(&value)?;
                if validated.kind == "LineString" {
                    return Err(UnifiedError::bad_request(
                        "plan boundary must be a Polygon or MultiPolygon, not a LineString",
                    ));
                }
                (Some(value.clone()), Some(geometry_hash(&value)))
            }
            None => (None, None),
        };
        let now = Utc::now();
        let plan = PropertyPlan {
            id: Uuid::now_v7(),
            property_id: property.id,
            name,
            description,
            geometry,
            geometry_hash,
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "properties.plan.created",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => created(plan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok(plan_view(&state, &plan))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyPlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        let mut plan = load_plan(&state, property.id, &plan_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            plan.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            plan.description = description
                .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
                .transpose()?;
        }
        if let Some(geometry) = input.geometry {
            match geometry {
                Some(value) => {
                    let validated = validate_geometry(&value)?;
                    if validated.kind == "LineString" {
                        return Err(UnifiedError::bad_request(
                            "plan boundary must be a Polygon or MultiPolygon, not a LineString",
                        ));
                    }
                    plan.geometry = Some(value.clone());
                    plan.geometry_hash = Some(geometry_hash(&value));
                }
                None => {
                    plan.geometry = None;
                    plan.geometry_hash = None;
                }
            }
        }
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "properties.plan.updated",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => ok(plan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let blocks = state.blocks_of_plan(plan.id);
        if blocks.iter().any(|block| {
            matches!(
                block.status,
                BlockStatus::Reserved | BlockStatus::Sold | BlockStatus::Assigned
            )
        }) {
            return Err(UnifiedError::conflict(
                "Plan cannot be deleted while it has reserved, sold, or assigned blocks",
            ));
        }
        for block in blocks {
            state.blocks.remove(&block.id);
            let history_ids: Vec<Uuid> = state
                .history
                .iter()
                .filter(|entry| entry.value().block_id == block.id)
                .map(|entry| *entry.key())
                .collect();
            for history_id in history_ids {
                state.history.remove(&history_id);
            }
        }
        state.plans.remove(&plan.id);
        state.emit(
            "properties.plan.deleted",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": plan.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
