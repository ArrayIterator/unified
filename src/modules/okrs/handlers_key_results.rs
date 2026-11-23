// OKR / Goals — Key result, check-in, and link handlers.

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
use super::handlers_objectives::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Key-result handlers
// ---------------------------------------------------------------------------

/// Remove a key result with its check-ins and links (shared by the
/// objective delete and the key-result delete).
pub(crate) fn delete_key_result_inner(state: &OkrState, key_result_id: Uuid) {
    state
        .checkins
        .retain(|_, checkin| checkin.key_result_id != key_result_id);
    state
        .links
        .retain(|_, link| link.key_result_id != key_result_id);
    state.key_results.remove(&key_result_id);
}

pub(crate) async fn list_key_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let objective = load_objective(&state, &path.into_inner())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let params = query.into_inner();
        let items: Vec<KeyResultView> = key_results_of(&state, objective.id)
            .iter()
            .map(|key_result| key_result_view(&state, key_result))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateKeyResultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let objective = load_objective(&state, &path.into_inner())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot add a key result to an objective of a closed cycle",
            ));
        }
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let unit = require_text(&body.unit, "unit", MAX_UNIT_LEN)?;
        let target_value = valid_number(body.target_value, "target_value")?;
        let start_value = valid_number(body.start_value.unwrap_or(0.0), "start_value")?;
        if target_value == start_value {
            return Err(UnifiedError::bad_request(
                "target_value must differ from start_value",
            ));
        }
        let now = Utc::now();
        let key_result = OkrKeyResult {
            id: Uuid::new_v4(),
            objective_id: objective.id,
            title,
            unit,
            target_value,
            start_value,
            current_value: start_value,
            created_at: now,
            updated_at: now,
        };
        state.key_results.insert(key_result.id, key_result.clone());
        state.emit(
            "okrs.key_result.created",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": objective.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let parent_objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&parent_objective.company_id, &_scope)?;
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateKeyResultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot update a key result of a closed cycle",
            ));
        }
        if let Some(title) = &body.title {
            key_result.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(unit) = &body.unit {
            key_result.unit = require_text(unit, "unit", MAX_UNIT_LEN)?;
        }
        if let Some(target_value) = body.target_value {
            key_result.target_value = valid_number(target_value, "target_value")?;
        }
        if let Some(start_value) = body.start_value {
            key_result.start_value = valid_number(start_value, "start_value")?;
        }
        if key_result.target_value == key_result.start_value {
            return Err(UnifiedError::bad_request(
                "target_value must differ from start_value",
            ));
        }
        // The current value is never hand-typed here: it moves only
        // through linked-task roll-up or the override endpoint.
        key_result.updated_at = Utc::now();
        state.key_results.insert(key_result.id, key_result.clone());
        state.emit(
            "okrs.key_result.updated",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_key_result(
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
        let key_result = load_key_result(&state, &path.into_inner())?;
        let parent_objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&parent_objective.company_id, &_scope)?;
        delete_key_result_inner(&state, key_result.id);
        state.emit(
            "okrs.key_result.deleted",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": key_result.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Check-in + override handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_checkins(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let parent_objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&parent_objective.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<OkrCheckin> = state
            .checkins
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|checkin| checkin.key_result_id == key_result.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_checkin(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCheckinRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCheckin> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot check in on a key result of a closed cycle",
            ));
        }
        let note = require_text(&body.note, "note", MAX_NOTE_LEN)?;
        let confidence = body.confidence.trim().to_lowercase();
        if !ALLOWED_CONFIDENCE.contains(&confidence.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "confidence must be one of: {}",
                ALLOWED_CONFIDENCE.join(", ")
            )));
        }
        let checkin = OkrCheckin {
            id: Uuid::new_v4(),
            key_result_id: key_result.id,
            author_id: caller.actor.clone(),
            note,
            confidence: confidence.clone(),
            kind: "note".to_string(),
            reason: None,
            value_snapshot: Some(key_result.current_value),
            created_at: Utc::now(),
        };
        state.checkins.insert(checkin.id, checkin.clone());
        state.emit(
            "okrs.kr.checkin",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "checkin_id": checkin.id.to_string(),
                "confidence": confidence,
                "value_snapshot": key_result.current_value,
                "actor_id": caller.actor,
            }),
        );
        Ok(checkin)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn override_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<OverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot override a key result of a closed cycle",
            ));
        }
        let value = valid_number(body.value, "value")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        let old_value = key_result.current_value;
        key_result.current_value = value;
        key_result.updated_at = Utc::now();
        state.key_results.insert(key_result.id, key_result.clone());
        // The override is stored as a check-in row so the manual
        // change always has a reason on record.
        let checkin = OkrCheckin {
            id: Uuid::new_v4(),
            key_result_id: key_result.id,
            author_id: caller.actor.clone(),
            note: format!("Manual override: {old_value} -> {value} ({reason})"),
            confidence: "medium".to_string(),
            kind: "override".to_string(),
            reason: Some(reason.clone()),
            value_snapshot: Some(value),
            created_at: Utc::now(),
        };
        state.checkins.insert(checkin.id, checkin.clone());
        emit_audit(
            &state,
            "okrs.kr.override",
            &caller.actor,
            &format!("key_result:{}", key_result.id),
            json!({
                "id": key_result.id.to_string(),
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "old_value": old_value,
                "new_value": value,
                "reason": reason,
            }),
        );
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Link handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let parent_objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&parent_objective.company_id, &_scope)?;
        let params = query.into_inner();
        let items = links_of(&state, key_result.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrLink> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&objective.company_id, &_scope)?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot link work to a key result of a closed cycle",
            ));
        }
        let link_type = body.link_type.trim().to_lowercase();
        if !ALLOWED_LINK_TYPES.contains(&link_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "link_type must be one of: {}",
                ALLOWED_LINK_TYPES.join(", ")
            )));
        }
        let target_id = parse_uuid(&body.target_id, "target")?;
        let duplicate = state.links.iter().any(|entry| {
            let link = entry.value();
            link.key_result_id == key_result.id
                && link.link_type == link_type
                && link.target_id == target_id
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "Key result is already linked to {link_type} {target_id}"
            )));
        }
        let link = OkrLink {
            id: Uuid::new_v4(),
            key_result_id: key_result.id,
            link_type: link_type.clone(),
            target_id,
            created_at: Utc::now(),
        };
        state.links.insert(link.id, link.clone());
        // A link created after its task already completed picks the
        // completion up immediately.
        refresh_rollup(&state, key_result.id);
        state.emit(
            "okrs.link.created",
            json!({
                "link_id": link.id.to_string(),
                "key_result_id": key_result.id.to_string(),
                "link_type": link_type,
                "target_id": target_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(link)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let (key_result_id, link_id) = path.into_inner();
        let key_result = load_key_result(&state, &key_result_id)?;
        let parent_objective = load_objective(&state, &key_result.objective_id.to_string())?;
        require_record_in_company_scope(&parent_objective.company_id, &_scope)?;
        let link = load_link(&state, &link_id)?;
        if link.key_result_id != key_result.id {
            return Err(UnifiedError::not_found(format!("Link not found: {link_id}")));
        }
        state.links.remove(&link.id);
        refresh_rollup(&state, key_result.id);
        state.emit(
            "okrs.link.deleted",
            json!({
                "link_id": link.id.to_string(),
                "key_result_id": key_result.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": link.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
