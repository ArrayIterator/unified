//! Curriculum module - competency-based school curricula. - Grade-component mapping handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
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
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

/// List the grade-component mappings (academic component id ->
/// basic competency id), sorted by component id. The mapping table
/// is the admin-managed bridge that lets `academic.grade.recorded`
/// events resolve to curriculum competencies.
pub(super) async fn list_component_mappings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let mut items: Vec<ComponentCompetencyMapping> = state
            .component_mappings
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| a.academic_component_id.cmp(&b.academic_component_id));
        Ok(json!(items))
    })();
    json::respond(result, debug)
}

/// Create one grade-component mapping. The basic competency must
/// exist in this module; the academic component id is an opaque
/// reference owned by the academic module and is stored as given
/// (one mapping per component - a second row for the same component
/// is a conflict). Grades recorded before the mapping exists stay
/// unmapped; creating the row does not rewrite them.
pub(super) async fn create_component_mapping(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<ComponentMappingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let component_id = require_text(
            input.academic_component_id.as_deref().unwrap_or(""),
            "academic_component_id",
            MAX_REF_LEN,
        )?;
        let competency_id = required_uuid(
            &input.basic_competency_id,
            "basic competency",
        )?;
        get_record(
            &state.basic_competencies,
            &competency_id,
            "Basic competency",
        )?;
        if state.component_mappings.contains_key(&component_id) {
            return Err(UnifiedError::conflict(format!(
                "A mapping already exists for academic component \"{component_id}\""
            )));
        }
        let mapping = ComponentCompetencyMapping {
            academic_component_id: component_id.clone(),
            basic_competency_id: competency_id.to_string(),
            created_at: now(),
        };
        state
            .component_mappings
            .insert(component_id, mapping.clone());
        state.emit(
            "curriculum.grade_component_mapping.created",
            json!({ "grade_component_mapping": mapping }),
        );
        Ok(json!(mapping))
    })();
    json::respond(result, debug)
}

/// Delete one grade-component mapping. Grades already recorded
/// against the component keep the competency they resolved to at
/// the time; later grades for the component record as unmapped
/// until a new mapping row is created.
pub(super) async fn delete_component_mapping(
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
        require_teach(&ctx, &caller.role)?;
        let component_id = require_text(path.as_str(), "academic_component_id", MAX_REF_LEN)?;
        let mapping = state
            .component_mappings
            .remove(&component_id)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No mapping exists for academic component \"{component_id}\""
                ))
            })?;
        state.emit(
            "curriculum.grade_component_mapping.deleted",
            json!({ "grade_component_mapping": mapping }),
        );
        Ok(json!({ "deleted": true, "academic_component_id": component_id }))
    })();
    json::respond(result, debug)
}
