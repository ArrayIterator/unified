//! Curriculum module - competency-based school curricula. - School level handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::helpers::*;
use super::hooks::*;
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_levels(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<SchoolLevel> =
            state.levels.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<LevelInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> =
        (|| {
            let _tenant = require_tenant(&req)?;
            let _scope = caller_company(&_tenant, &req)?;
            let caller = caller(&req)?;
            require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
            let input = body.into_inner();
            let framework_id = required_uuid(&input.framework_id, "framework")?;
            get_record(&state.frameworks, &framework_id, "Framework")?;
            let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
            if state.levels.iter().any(|entry| {
                entry.value().framework_id == framework_id && entry.value().code == code
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A level with code \"{code}\" already exists in this framework"
                )));
            }
            let level = SchoolLevel {
                id: Uuid::new_v4(),
                framework_id,
                code,
                name: require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?,
                grade_count: input.grade_count.unwrap_or(0),
                sort_order: input.sort_order.unwrap_or(0),
                created_at: now(),
                updated_at: now(),
            };
            state.levels.insert(level.id, level.clone());
            state.emit("curriculum.level.created", json!({ "level": level }));
            Ok(json!(level))
        })();
    json::respond(result, debug)
}

pub(super) async fn get_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "level")?;
        Ok(json!(get_record(&state.levels, &id, "School level")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LevelInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "level")?;
        let input = body.into_inner();
        let mut level = get_record(&state.levels, &id, "School level")?;
        if let Some(raw) = &input.framework_id {
            let framework_id = parse_uuid(raw, "framework")?;
            get_record(&state.frameworks, &framework_id, "Framework")?;
            level.framework_id = framework_id;
        }
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state.levels.iter().any(|entry| {
                entry.value().framework_id == level.framework_id
                    && entry.value().code == code
                    && entry.value().id != id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A level with code \"{code}\" already exists in this framework"
                )));
            }
            level.code = code;
        }
        if let Some(raw) = &input.name {
            level.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(count) = input.grade_count {
            level.grade_count = count;
        }
        if let Some(order) = input.sort_order {
            level.sort_order = order;
        }
        level.updated_at = now();
        state.levels.insert(level.id, level.clone());
        state.emit("curriculum.level.updated", json!({ "level": level }));
        Ok(json!(level))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "level")?;
        get_record(&state.levels, &id, "School level")?;
        let referenced = state.subjects.iter().any(|entry| entry.value().level_id == id)
            || state.core_competencies.iter().any(|entry| entry.value().level_id == id)
            || state.structures.iter().any(|entry| entry.value().level_id == id)
            || state.report_cards.iter().any(|entry| entry.value().level_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Level is still referenced by subjects, competencies, structures, or report cards",
            ));
        }
        state.levels.remove(&id);
        state.emit("curriculum.level.deleted", json!({ "level_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
