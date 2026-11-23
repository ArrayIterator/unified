// OKR / Goals — Cycle handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Cycle handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_cycles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<OkrCycle> = state
            .cycles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cycle| {
                if !in_company_scope(&cycle.company_id, &_scope) {
                    return false;
                }
                if let Some(company) = &company_filter {
                    if &cycle.company_id != company {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if cycle.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        cycle.name,
                        cycle.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "starts_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "starts_at" => a.starts_at.cmp(&b.starts_at),
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

pub(crate) async fn create_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    body: web::Json<CreateCycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCycle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_text(
            &require_company_match_str(&_tenant, &req, &body.company_id)?,
            "company_id",
            MAX_COMPANY_LEN,
        )?;
        if body.ends_at <= body.starts_at {
            return Err(UnifiedError::bad_request(
                "ends_at must be after starts_at",
            ));
        }
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let cycle = OkrCycle {
            id: Uuid::new_v4(),
            company_id: company_id.clone(),
            name,
            description,
            starts_at: body.starts_at,
            ends_at: body.ends_at,
            status: CycleStatus::Draft,
            created_at: now,
            updated_at: now,
        };
        state.cycles.insert(cycle.id, cycle.clone());
        state.emit(
            "okrs.cycle.created",
            json!({
                "cycle_id": cycle.id.to_string(),
                "company_id": company_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(cycle)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCycle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let cycle = load_cycle(&state, &path.into_inner())?;
        require_record_in_company_scope(&cycle.company_id, &_scope)?;
        Ok(cycle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn activate_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCycle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut cycle = load_cycle(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&cycle.company_id, &_scope)?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict("A closed cycle cannot be activated"));
        }
        if cycle.status == CycleStatus::Active {
            return Ok(cycle);
        }
        let clash = state.cycles.iter().any(|entry| {
            let other = entry.value();
            other.company_id == cycle.company_id
                && other.id != cycle.id
                && other.status == CycleStatus::Active
        });
        if clash {
            return Err(UnifiedError::conflict(format!(
                "Company \"{}\" already has an active cycle; close it before activating another",
                cycle.company_id
            )));
        }
        cycle.status = CycleStatus::Active;
        cycle.updated_at = Utc::now();
        state.cycles.insert(cycle.id, cycle.clone());
        Ok(cycle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn close_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut cycle = load_cycle(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&cycle.company_id, &_scope)?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict("Cycle is already closed"));
        }
        cycle.status = CycleStatus::Closed;
        cycle.updated_at = Utc::now();
        state.cycles.insert(cycle.id, cycle.clone());
        let mut scored: Vec<Value> = Vec::new();
        let objectives: Vec<OkrObjective> = state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|objective| objective.cycle_id == cycle.id)
            .collect();
        for mut objective in objectives {
            let score = objective_progress(&state, objective.id).unwrap_or(0.0);
            objective.final_score = Some(score);
            objective.updated_at = Utc::now();
            state.objectives.insert(objective.id, objective.clone());
            scored.push(json!({
                "objective_id": objective.id.to_string(),
                "title": objective.title,
                "score": score,
            }));
        }
        state.emit(
            "okrs.cycle.closed",
            json!({
                "cycle_id": cycle.id.to_string(),
                "company_id": cycle.company_id,
                "actor_id": caller.actor,
                "objectives_scored": scored.len(),
            }),
        );
        Ok(json!({
            "cycle_id": cycle.id.to_string(),
            "status": "closed",
            "objectives_scored": scored.len(),
            "scores": scored,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
