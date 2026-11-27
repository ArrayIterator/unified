//! Curriculum module - competency-based school curricula. - Core competency handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
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

pub(super) async fn list_core_competencies(
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
        let items: Vec<CoreCompetency> = state
            .core_competencies
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_core_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<CoreCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != framework_id || subject.level_id != level_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to the given framework and level",
            ));
        }
        get_record(&state.levels, &level_id, "School level")?;
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        let version = input.version.unwrap_or(2013);
        if state.core_competencies.iter().any(|entry| {
            entry.value().subject_id == subject_id
                && entry.value().code == code
                && entry.value().version == version
        }) {
            return Err(UnifiedError::conflict(format!(
                "A core competency with code \"{code}\" already exists for this subject and version"
            )));
        }
        let competency = CoreCompetency {
            id: Uuid::new_v4(),
            framework_id,
            subject_id,
            level_id,
            code,
            title: require_text(input.title.as_deref().unwrap_or(""), "title", MAX_TITLE_LEN)?,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            grade_year: input.grade_year.unwrap_or(1),
            version,
            created_at: now(),
            updated_at: now(),
        };
        state.core_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.core_competency.created",
            json!({ "core_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_core_competency(
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
        let id = parse_uuid(&path.into_inner(), "core competency")?;
        Ok(json!(get_record(&state.core_competencies, &id, "Core competency")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_core_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CoreCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "core competency")?;
        let input = body.into_inner();
        let mut competency = get_record(&state.core_competencies, &id, "Core competency")?;
        if let Some(raw) = &input.subject_id {
            let subject_id = parse_uuid(raw, "subject")?;
            let subject = get_record(&state.subjects, &subject_id, "Subject")?;
            if subject.framework_id != competency.framework_id {
                return Err(UnifiedError::bad_request(
                    "Subject does not belong to this competency's framework",
                ));
            }
            competency.subject_id = subject_id;
            competency.level_id = subject.level_id;
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
        state.core_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.core_competency.updated",
            json!({ "core_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_core_competency(
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
        let id = parse_uuid(&path.into_inner(), "core competency")?;
        get_record(&state.core_competencies, &id, "Core competency")?;
        let referenced = state
            .basic_competencies
            .iter()
            .any(|entry| entry.value().core_competency_id == id)
            || state
                .lesson_plans
                .iter()
                .any(|entry| entry.value().core_competency_id == Some(id));
        if referenced {
            return Err(UnifiedError::conflict(
                "Core competency is still referenced by basic competencies or lesson plans",
            ));
        }
        state.core_competencies.remove(&id);
        state.emit(
            "curriculum.core_competency.deleted",
            json!({ "core_competency_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
