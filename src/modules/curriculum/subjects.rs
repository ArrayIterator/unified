//! Curriculum module - competency-based school curricula. - Subject handlers.
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
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_subjects(
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
        let items: Vec<Subject> =
            state.subjects.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<SubjectInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        get_record(&state.levels, &level_id, "School level")?;
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        if state.subjects.iter().any(|entry| {
            entry.value().framework_id == framework_id
                && entry.value().level_id == level_id
                && entry.value().code == code
        }) {
            return Err(UnifiedError::conflict(format!(
                "A subject with code \"{code}\" already exists at this level in this framework"
            )));
        }
        let subject = Subject {
            id: Uuid::new_v4(),
            framework_id,
            level_id,
            code,
            name: require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.subjects.insert(subject.id, subject.clone());
        state.emit("curriculum.subject.created", json!({ "subject": subject }));
        Ok(json!(subject))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_subject(
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
        let id = parse_uuid(&path.into_inner(), "subject")?;
        Ok(json!(get_record(&state.subjects, &id, "Subject")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubjectInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "subject")?;
        let input = body.into_inner();
        let mut subject = get_record(&state.subjects, &id, "Subject")?;
        if let Some(raw) = &input.framework_id {
            let framework_id = parse_uuid(raw, "framework")?;
            get_record(&state.frameworks, &framework_id, "Framework")?;
            subject.framework_id = framework_id;
        }
        if let Some(raw) = &input.level_id {
            let level_id = parse_uuid(raw, "level")?;
            get_record(&state.levels, &level_id, "School level")?;
            subject.level_id = level_id;
        }
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state.subjects.iter().any(|entry| {
                entry.value().framework_id == subject.framework_id
                    && entry.value().level_id == subject.level_id
                    && entry.value().code == code
                    && entry.value().id != id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A subject with code \"{code}\" already exists at this level in this framework"
                )));
            }
            subject.code = code;
        }
        if let Some(raw) = &input.name {
            subject.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(order) = input.sort_order {
            subject.sort_order = order;
        }
        subject.updated_at = now();
        state.subjects.insert(subject.id, subject.clone());
        state.emit("curriculum.subject.updated", json!({ "subject": subject }));
        Ok(json!(subject))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_subject(
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
        let id = parse_uuid(&path.into_inner(), "subject")?;
        get_record(&state.subjects, &id, "Subject")?;
        let referenced = state
            .core_competencies
            .iter()
            .any(|entry| entry.value().subject_id == id)
            || state.structures.iter().any(|entry| entry.value().subject_id == id)
            || state.lesson_plans.iter().any(|entry| entry.value().subject_id == id)
            || state
                .report_card_entries
                .iter()
                .any(|entry| entry.value().subject_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Subject is still referenced by competencies, structures, lesson plans, or report-card entries",
            ));
        }
        state.subjects.remove(&id);
        state.emit("curriculum.subject.deleted", json!({ "subject_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
