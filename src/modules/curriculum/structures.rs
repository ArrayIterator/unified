//! Curriculum module — competency-based school curricula. — Curriculum structure handlers.
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

pub(super) async fn list_structures(
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
        let items: Vec<CurriculumStructure> =
            state.structures.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_structure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<StructureInput>,
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
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        get_record(&state.levels, &level_id, "School level")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != framework_id || subject.level_id != level_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to the given framework and level",
            ));
        }
        let grade_year = input.grade_year.unwrap_or(1);
        if state.structures.iter().any(|entry| {
            let row = entry.value();
            row.framework_id == framework_id
                && row.level_id == level_id
                && row.grade_year == grade_year
                && row.subject_id == subject_id
        }) {
            return Err(UnifiedError::conflict(
                "This subject is already in the structure for this framework, level, and grade year",
            ));
        }
        let hours = input.hours_per_week.unwrap_or(0.0);
        if !hours.is_finite() || hours < 0.0 || hours > 168.0 {
            return Err(UnifiedError::bad_request(
                "hours_per_week must be between 0 and 168",
            ));
        }
        let row = CurriculumStructure {
            id: Uuid::new_v4(),
            framework_id,
            level_id,
            grade_year,
            subject_id,
            hours_per_week: hours,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.structures.insert(row.id, row.clone());
        state.emit("curriculum.structure.created", json!({ "structure": row }));
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_structure(
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
        let id = parse_uuid(&path.into_inner(), "structure")?;
        Ok(json!(get_record(&state.structures, &id, "Curriculum structure")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_structure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<StructureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "structure")?;
        let input = body.into_inner();
        let mut row = get_record(&state.structures, &id, "Curriculum structure")?;
        if let Some(hours) = input.hours_per_week {
            if !hours.is_finite() || hours < 0.0 || hours > 168.0 {
                return Err(UnifiedError::bad_request(
                    "hours_per_week must be between 0 and 168",
                ));
            }
            row.hours_per_week = hours;
        }
        if let Some(year) = input.grade_year {
            row.grade_year = year;
        }
        if let Some(order) = input.sort_order {
            row.sort_order = order;
        }
        row.updated_at = now();
        state.structures.insert(row.id, row.clone());
        state.emit("curriculum.structure.updated", json!({ "structure": row }));
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_structure(
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
        let id = parse_uuid(&path.into_inner(), "structure")?;
        get_record(&state.structures, &id, "Curriculum structure")?;
        state.structures.remove(&id);
        state.emit("curriculum.structure.deleted", json!({ "structure_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
