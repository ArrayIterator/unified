//! Curriculum module — competency-based school curricula. — Module.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

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
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json;
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `curriculum.ai.draft_lesson_plan_requested` hook, and run through
// the AI module's gated runner. Drafts are returned inline and are
// never stored, published, or issued by this module: creating a
// lesson plan stays on the existing human endpoints.
// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: competency texts and the
/// topic are curriculum data, redacted before reaching the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_lesson_plan",
        module_id: "curriculum",
        name: "Draft lesson plan",
        description: "Draft a lesson plan from a core competency, a basic competency, and a topic. The output is returned inline; it is never stored or published by this module.",
        input_fields: &["core_competency_id", "basic_competency_id", "topic"],
        allows_pii: false,
        use_case: "curriculum.draft_lesson_plan",
    },
];

/// Grounding for a lesson-plan draft: the named core and basic
/// competencies with their codes, titles, and descriptions, plus
/// the subject and framework they belong to. Unknown ids ground
/// nothing (the endpoint 404s on them before delegating).
pub(super) fn lesson_plan_grounding(
    state: &CurriculumState,
    core_competency_id: &str,
    basic_competency_id: &str,
) -> Value {
    let core = Uuid::parse_str(core_competency_id)
        .ok()
        .and_then(|id| {
            state
                .core_competencies
                .get(&id)
                .map(|entry| entry.value().clone())
        });
    let basic = Uuid::parse_str(basic_competency_id)
        .ok()
        .and_then(|id| {
            state
                .basic_competencies
                .get(&id)
                .map(|entry| entry.value().clone())
        });
    let subject = core.as_ref().and_then(|competency| {
        state
            .subjects
            .get(&competency.subject_id)
            .map(|entry| entry.value().clone())
    });
    let framework = core.as_ref().and_then(|competency| {
        state
            .frameworks
            .get(&competency.framework_id)
            .map(|entry| entry.value().clone())
    });
    json!({
        "core_competency": core.map(|competency| json!({
            "code": competency.code,
            "title": competency.title,
            "description": competency.description,
            "grade_year": competency.grade_year,
        })),
        "basic_competency": basic.map(|competency| json!({
            "code": competency.code,
            "title": competency.title,
            "description": competency.description,
            "grade_year": competency.grade_year,
        })),
        "subject": subject.map(|subject| json!({
            "code": subject.code,
            "name": subject.name,
        })),
        "framework": framework.map(|framework| json!({
            "code": framework.code,
            "name": framework.name,
        })),
    })
}

/// Run one registered curriculum AI capability:
/// `POST /api/v1/curriculum/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use` plus the module's own
/// `curriculum:view`; for `draft_lesson_plan` the named
/// competencies must exist (404 otherwise). Delegates to the AI
/// module's gated runner.
pub(super) async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW) {
        return json::error_response(&error, debug);
    }
    if capability == "draft_lesson_plan" {
        for field in ["core_competency_id", "basic_competency_id"] {
            let raw = input
                .get(field)
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let id = match Uuid::parse_str(raw) {
                Ok(id) => id,
                Err(_) => {
                    return json::error_response(
                        &UnifiedError::bad_request(format!(
                            "input.{field} must be a valid id"
                        )),
                        debug,
                    )
                }
            };
            let exists = if field == "core_competency_id" {
                state.core_competencies.contains_key(&id)
            } else {
                state.basic_competencies.contains_key(&id)
            };
            if !exists {
                return json::error_response(
                    &UnifiedError::not_found(format!("Competency not found: {id}")),
                    debug,
                );
            }
        }
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "curriculum",
        &capability,
        input,
    )
    .await
    {
        Ok(data) => json::success_response(data, 200),
        Err(error) => json::error_response(&error, debug),
    }
}
