//! Curriculum module — competency-based school curricula. — Basic competency handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
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
use super::levels::*;
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

pub(super) async fn list_basic_competencies(
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
        let items: Vec<BasicCompetency> = state
            .basic_competencies
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_basic_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<BasicCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let core_id = required_uuid(&input.core_competency_id, "core competency")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let core = get_record(&state.core_competencies, &core_id, "Core competency")?;
        if core.framework_id != framework_id {
            return Err(UnifiedError::bad_request(
                "Core competency does not belong to the given framework",
            ));
        }
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        let version = input.version.unwrap_or(core.version);
        if state.basic_competencies.iter().any(|entry| {
            entry.value().core_competency_id == core_id
                && entry.value().code == code
                && entry.value().version == version
        }) {
            return Err(UnifiedError::conflict(format!(
                "A basic competency with code \"{code}\" already exists under this core competency and version"
            )));
        }
        let competency = BasicCompetency {
            id: Uuid::new_v4(),
            framework_id,
            core_competency_id: core_id,
            code,
            title: require_text(input.title.as_deref().unwrap_or(""), "title", MAX_TITLE_LEN)?,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            grade_year: input.grade_year.unwrap_or(core.grade_year),
            version,
            created_at: now(),
            updated_at: now(),
        };
        state.basic_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.basic_competency.created",
            json!({ "basic_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_basic_competency(
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
        let id = parse_uuid(&path.into_inner(), "basic competency")?;
        Ok(json!(get_record(&state.basic_competencies, &id, "Basic competency")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_basic_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BasicCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "basic competency")?;
        let input = body.into_inner();
        let mut competency = get_record(&state.basic_competencies, &id, "Basic competency")?;
        if let Some(raw) = &input.core_competency_id {
            let core_id = parse_uuid(raw, "core competency")?;
            let core = get_record(&state.core_competencies, &core_id, "Core competency")?;
            if core.framework_id != competency.framework_id {
                return Err(UnifiedError::bad_request(
                    "Core competency does not belong to this competency's framework",
                ));
            }
            competency.core_competency_id = core_id;
        }
        if let Some(raw) = &input.code {
            competency.code = require_code(raw, "code")?;
        }
        if let Some(raw) = &input.title {
            competency.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(raw) = &input.description {
            competency.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(year) = input.grade_year {
            competency.grade_year = year;
        }
        if let Some(version) = input.version {
            competency.version = version;
        }
        competency.updated_at = now();
        state.basic_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.basic_competency.updated",
            json!({ "basic_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_basic_competency(
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
        let id = parse_uuid(&path.into_inner(), "basic competency")?;
        get_record(&state.basic_competencies, &id, "Basic competency")?;
        let referenced = state
            .interventions
            .iter()
            .any(|entry| entry.value().basic_competency_id == id)
            || state
                .lesson_plans
                .iter()
                .any(|entry| entry.value().basic_competency_id == Some(id));
        if referenced {
            return Err(UnifiedError::conflict(
                "Basic competency is still referenced by interventions or lesson plans",
            ));
        }
        state.basic_competencies.remove(&id);
        state.emit(
            "curriculum.basic_competency.deleted",
            json!({ "basic_competency_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
