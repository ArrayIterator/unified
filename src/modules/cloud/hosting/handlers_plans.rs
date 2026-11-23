// hosting module — Plan handlers.
//
// Split from the former single-file module `src/modules/hosting.rs`.
// The module root (`hosting.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{HostingPlan, HostingState};
use super::requests::{CreatePlanRequest, UpdatePlanRequest};
use super::helpers::{PERM_HOSTING_MANAGE, PERM_HOSTING_VIEW, caller, paginate, require_permission, require_tenant, validate_billing_cycle, validate_currency, validate_plan_resources};

// ---------------------------------------------------------------------------
// Plan handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Plans are a shared catalogue (no company field): the guard
    // is a fail-closed gate only.
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_HOSTING_VIEW))
        .and_then(|_| crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req))
    {
        return json::error_response(&error, debug);
    }
    let mut items: Vec<HostingPlan> = state
        .plans
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|plan| {
            plan.name.to_lowercase().contains(&needle)
                || plan.description.to_lowercase().contains(&needle)
        });
    }
    if let Some(cycle) = params.filters.get("billing_cycle") {
        items.retain(|plan| plan.billing_cycle == *cycle);
    }
    if let Some(active) = params.filters.get("is_active") {
        if let Ok(wanted) = active.parse::<bool>() {
            items.retain(|plan| plan.is_active == wanted);
        }
    }
    if let Some(currency) = params.filters.get("currency") {
        items.retain(|plan| plan.currency.eq_ignore_ascii_case(currency));
    }
    if let Some(product_id) = params.filters.get("store_product_id") {
        items.retain(|plan| plan.store_product_id.as_deref() == Some(product_id.as_str()));
    }
    let sort = params.allowed_sort(&["name", "price", "created_at"], "created_at");
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "price" => items.sort_by(|a, b| a.price.cmp(&b.price)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params).to_json(), 200)
}

pub(crate) async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingPlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let name = body.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        validate_plan_resources(
            body.disk_gb,
            body.cpu_cores,
            body.ram_mb,
            body.bandwidth_gb,
            body.price,
        )?;
        validate_billing_cycle(&body.billing_cycle)?;
        let currency = validate_currency(body.currency.as_deref().unwrap_or("USD"))?;
        if state
            .plans
            .iter()
            .any(|entry| entry.value().name.eq_ignore_ascii_case(&name))
        {
            return Err(UnifiedError::conflict(format!(
                "Hosting plan \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let plan = HostingPlan {
            id: Uuid::now_v7(),
            name,
            description: body.description.clone().unwrap_or_default(),
            disk_gb: body.disk_gb,
            cpu_cores: body.cpu_cores,
            ram_mb: body.ram_mb,
            bandwidth_gb: body.bandwidth_gb,
            price: body.price,
            currency,
            billing_cycle: body.billing_cycle.clone(),
            store_product_id: body
                .store_product_id
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            is_active: body.is_active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        ctx.hooks.emit(
            "hosting.plan.created",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => json::success_response(plan, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingPlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_VIEW)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        state
            .plans
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingPlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let id = id.into_inner();
        let mut plan = state
            .plans
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))?;
        if let Some(name) = &body.name {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            if state
                .plans
                .iter()
                .any(|entry| *entry.key() != id && entry.value().name.eq_ignore_ascii_case(&name))
            {
                return Err(UnifiedError::conflict(format!(
                    "Hosting plan \"{name}\" already exists"
                )));
            }
            plan.name = name;
        }
        if let Some(description) = &body.description {
            plan.description = description.clone();
        }
        let disk_gb = body.disk_gb.unwrap_or(plan.disk_gb);
        let cpu_cores = body.cpu_cores.unwrap_or(plan.cpu_cores);
        let ram_mb = body.ram_mb.unwrap_or(plan.ram_mb);
        let bandwidth_gb = body.bandwidth_gb.unwrap_or(plan.bandwidth_gb);
        let price = body.price.unwrap_or(plan.price);
        validate_plan_resources(disk_gb, cpu_cores, ram_mb, bandwidth_gb, price)?;
        plan.disk_gb = disk_gb;
        plan.cpu_cores = cpu_cores;
        plan.ram_mb = ram_mb;
        plan.bandwidth_gb = bandwidth_gb;
        plan.price = price;
        if let Some(cycle) = &body.billing_cycle {
            validate_billing_cycle(cycle)?;
            plan.billing_cycle = cycle.clone();
        }
        if let Some(currency) = &body.currency {
            plan.currency = validate_currency(currency)?;
        }
        if let Some(product_id) = &body.store_product_id {
            plan.store_product_id = product_id
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(is_active) = body.is_active {
            plan.is_active = is_active;
        }
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        ctx.hooks.emit(
            "hosting.plan.updated",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        Ok(plan)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let id = id.into_inner();
        if !state.plans.contains_key(&id) {
            return Err(UnifiedError::not_found("Hosting plan not found"));
        }
        if state
            .accounts
            .iter()
            .any(|entry| entry.value().plan_id == id)
        {
            return Err(UnifiedError::conflict(
                "Hosting plan cannot be deleted: hosting accounts still reference it",
            ));
        }
        state.plans.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
