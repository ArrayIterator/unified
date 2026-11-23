// OKR / Goals — Objective and alignment handlers.

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
use super::handlers_cycles::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Objective handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_objectives(
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
        let cycle_filter = params.filters.get("cycle_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let level_filter = params.filters.get("level").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let parent_filter = params.filters.get("parent_objective_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ObjectiveView> = state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|objective| {
                if !in_company_scope(&objective.company_id, &_scope) {
                    return false;
                }
                if let Some(cycle_id) = &cycle_filter {
                    if objective.cycle_id.to_string() != *cycle_id {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if &objective.company_id != company {
                        return false;
                    }
                }
                if let Some(level) = &level_filter {
                    if &objective.level != level {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &objective.owner_id != owner {
                        return false;
                    }
                }
                if let Some(parent) = &parent_filter {
                    if objective
                        .parent_objective_id
                        .map(|id| id.to_string())
                        .as_deref()
                        != Some(parent.as_str())
                    {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        objective.title,
                        objective.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|objective| objective_view(&state, &objective))
            .collect();
        let sort = params.allowed_sort(&["title", "level", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.objective.title.cmp(&b.objective.title),
                "level" => a.objective.level.cmp(&b.objective.level),
                _ => a.objective.created_at.cmp(&b.objective.created_at),
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

pub(crate) async fn create_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    body: web::Json<CreateObjectiveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ObjectiveView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let cycle = load_cycle(&state, &body.cycle_id)?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot add an objective to a closed cycle",
            ));
        }
        let company_id = require_text(
            &require_company_match_str(&_tenant, &req, &body.company_id)?,
            "company_id",
            MAX_COMPANY_LEN,
        )?;
        if cycle.company_id != company_id {
            return Err(UnifiedError::bad_request(
                "company_id does not match the cycle's company_id",
            ));
        }
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let level = body.level.trim().to_lowercase();
        if !ALLOWED_LEVELS.contains(&level.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "level must be one of: {}",
                ALLOWED_LEVELS.join(", ")
            )));
        }
        let owner_id = match &body.owner_id {
            Some(owner) => require_text(owner, "owner_id", MAX_NAME_LEN)?,
            None => caller.actor.clone(),
        };
        let parent_objective_id = match &body.parent_objective_id {
            Some(raw) => {
                let parent = load_objective(&state, raw)?;
                if parent.cycle_id != cycle.id {
                    return Err(UnifiedError::bad_request(
                        "parent objective must belong to the same cycle",
                    ));
                }
                Some(parent.id)
            }
            None => None,
        };
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let objective = OkrObjective {
            id: Uuid::new_v4(),
            cycle_id: cycle.id,
            company_id: company_id.clone(),
            parent_objective_id,
            title,
            description,
            level,
            owner_id: owner_id.clone(),
            final_score: None,
            created_at: now,
            updated_at: now,
        };
        state.objectives.insert(objective.id, objective.clone());
        state.emit(
            "okrs.objective.created",
            json!({
                "objective_id": objective.id.to_string(),
                "cycle_id": cycle.id.to_string(),
                "company_id": company_id,
                "level": objective.level,
                "actor_id": caller.actor,
            }),
        );
        Ok(objective_view(&state, &objective))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ObjectiveView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let objective = load_objective(&state, &path.into_inner())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        Ok(objective_view(&state, &objective))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateObjectiveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ObjectiveView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut objective = load_objective(&state, &path.into_inner())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot update an objective of a closed cycle",
            ));
        }
        if let Some(title) = &body.title {
            objective.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &body.description {
            objective.description = match description {
                Some(text) if !text.trim().is_empty() => Some(require_text(
                    text,
                    "description",
                    MAX_DESCRIPTION_LEN,
                )?),
                _ => None,
            };
        }
        if let Some(level) = &body.level {
            let level = level.trim().to_lowercase();
            if !ALLOWED_LEVELS.contains(&level.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "level must be one of: {}",
                    ALLOWED_LEVELS.join(", ")
                )));
            }
            objective.level = level;
        }
        if let Some(owner_id) = &body.owner_id {
            objective.owner_id = require_text(owner_id, "owner_id", MAX_NAME_LEN)?;
        }
        if let Some(parent) = &body.parent_objective_id {
            objective.parent_objective_id = match parent {
                Some(raw) => {
                    let parent_objective = load_objective(&state, raw)?;
                    if parent_objective.cycle_id != objective.cycle_id {
                        return Err(UnifiedError::bad_request(
                            "parent objective must belong to the same cycle",
                        ));
                    }
                    if parent_objective.id == objective.id {
                        return Err(UnifiedError::bad_request(
                            "an objective cannot be its own parent",
                        ));
                    }
                    Some(parent_objective.id)
                }
                None => None,
            };
        }
        objective.updated_at = Utc::now();
        state.objectives.insert(objective.id, objective.clone());
        state.emit(
            "okrs.objective.updated",
            json!({
                "objective_id": objective.id.to_string(),
                "cycle_id": objective.cycle_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(objective_view(&state, &objective))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let objective = load_objective(&state, &path.into_inner())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        // Children lose their parent instead of being deleted: the
        // alignment tree keeps the rest of the objectives intact.
        for mut child in state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|child| child.parent_objective_id == Some(objective.id))
        {
            child.parent_objective_id = None;
            child.updated_at = Utc::now();
            state.objectives.insert(child.id, child);
        }
        for key_result in key_results_of(&state, objective.id) {
            delete_key_result_inner(&state, key_result.id);
        }
        state.objectives.remove(&objective.id);
        state.emit(
            "okrs.objective.deleted",
            json!({
                "objective_id": objective.id.to_string(),
                "cycle_id": objective.cycle_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": objective.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_alignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let objective = load_objective(&state, &path.into_inner())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        // Parent chain, root first, guarding against a cycle in
        // the parent pointers.
        let mut parents: Vec<ObjectiveView> = Vec::new();
        let mut seen: Vec<Uuid> = vec![objective.id];
        let mut current = objective.parent_objective_id;
        while let Some(parent_id) = current {
            if seen.contains(&parent_id) {
                break;
            }
            seen.push(parent_id);
            let Some(parent) = state
                .objectives
                .get(&parent_id)
                .map(|entry| entry.value().clone())
            else {
                break;
            };
            parents.push(objective_view(&state, &parent));
            current = parent.parent_objective_id;
        }
        parents.reverse();
        let children: Vec<ObjectiveView> = state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|child| child.parent_objective_id == Some(objective.id))
            .map(|child| objective_view(&state, &child))
            .collect();
        let mut linked_projects: Vec<String> = Vec::new();
        let mut linked_tasks: Vec<String> = Vec::new();
        for key_result in key_results_of(&state, objective.id) {
            for link in links_of(&state, key_result.id) {
                match link.link_type.as_str() {
                    "project" => {
                        let id = link.target_id.to_string();
                        if !linked_projects.contains(&id) {
                            linked_projects.push(id);
                        }
                    }
                    "task" => {
                        let id = link.target_id.to_string();
                        if !linked_tasks.contains(&id) {
                            linked_tasks.push(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(json!({
            "objective": objective_view(&state, &objective),
            "parents": parents,
            "children": children,
            "linked_projects": linked_projects,
            "linked_tasks": linked_tasks,
            "note": "Project links are alignment only: the numeric roll-up counts linked tasks (see the module header).",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
