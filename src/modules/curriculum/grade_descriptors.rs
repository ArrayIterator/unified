//! Curriculum module - competency-based school curricula. - Grade descriptor handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
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

pub(super) async fn list_grade_descriptors(
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
        let items: Vec<GradeDescriptor> = state
            .grade_descriptors
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_grade_descriptor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<GradeDescriptorInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let domain_id = optional_uuid(&input.domain_id, "assessment domain")?;
        if let Some(domain_id) = domain_id {
            get_record(&state.assessment_domains, &domain_id, "Assessment domain")?;
        }
        let min_score = require_score(input.min_score.unwrap_or(0.0), "min_score")?;
        let max_score = require_score(input.max_score.unwrap_or(100.0), "max_score")?;
        if min_score > max_score {
            return Err(UnifiedError::bad_request(
                "min_score must not be greater than max_score",
            ));
        }
        let row = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id,
            min_score,
            max_score,
            grade_label: require_text(
                input.grade_label.as_deref().unwrap_or(""),
                "grade_label",
                MAX_CODE_LEN,
            )?,
            descriptor: require_text(
                input.descriptor.as_deref().unwrap_or(""),
                "descriptor",
                MAX_TEXT_LEN,
            )?,
            created_at: now(),
            updated_at: now(),
        };
        state.grade_descriptors.insert(row.id, row.clone());
        state.emit(
            "curriculum.grade_descriptor.created",
            json!({ "grade_descriptor": row }),
        );
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_grade_descriptor(
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
        let id = parse_uuid(&path.into_inner(), "grade descriptor")?;
        Ok(json!(get_record(&state.grade_descriptors, &id, "Grade descriptor")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_grade_descriptor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GradeDescriptorInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "grade descriptor")?;
        let input = body.into_inner();
        let mut row = get_record(&state.grade_descriptors, &id, "Grade descriptor")?;
        if let Some(raw) = &input.domain_id {
            let domain_id = if raw.trim().is_empty() {
                None
            } else {
                Some(parse_uuid(raw.trim(), "assessment domain")?)
            };
            if let Some(domain_id) = domain_id {
                get_record(&state.assessment_domains, &domain_id, "Assessment domain")?;
            }
            row.domain_id = domain_id;
        }
        if let Some(value) = input.min_score {
            row.min_score = require_score(value, "min_score")?;
        }
        if let Some(value) = input.max_score {
            row.max_score = require_score(value, "max_score")?;
        }
        if row.min_score > row.max_score {
            return Err(UnifiedError::bad_request(
                "min_score must not be greater than max_score",
            ));
        }
        if let Some(raw) = &input.grade_label {
            row.grade_label = require_text(raw, "grade_label", MAX_CODE_LEN)?;
        }
        if let Some(raw) = &input.descriptor {
            row.descriptor = require_text(raw, "descriptor", MAX_TEXT_LEN)?;
        }
        row.updated_at = now();
        state.grade_descriptors.insert(row.id, row.clone());
        state.emit(
            "curriculum.grade_descriptor.updated",
            json!({ "grade_descriptor": row }),
        );
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_grade_descriptor(
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
        let id = parse_uuid(&path.into_inner(), "grade descriptor")?;
        get_record(&state.grade_descriptors, &id, "Grade descriptor")?;
        state.grade_descriptors.remove(&id);
        state.emit(
            "curriculum.grade_descriptor.deleted",
            json!({ "grade_descriptor_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
