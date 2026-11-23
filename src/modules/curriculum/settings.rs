//! Curriculum module — competency-based school curricula. — Settings handlers.
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

pub(super) async fn get_settings(
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
        Ok(json!(state.settings()))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<SettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(raw) = &input.default_framework_code {
            let code = require_code(raw, "default_framework_code")?;
            if !state.frameworks.iter().any(|entry| entry.value().code == code) {
                return Err(UnifiedError::bad_request(format!(
                    "No framework with code \"{code}\" exists"
                )));
            }
            settings.default_framework_code = code;
        }
        if let Some(raw) = &input.default_paper_size {
            let size = require_code(raw, "default_paper_size")?;
            if let Some(sizes) = &input.paper_sizes {
                if !sizes.iter().any(|item| item == &size) {
                    return Err(UnifiedError::bad_request(
                        "default_paper_size must be one of paper_sizes",
                    ));
                }
            } else if !settings.paper_sizes.iter().any(|item| item == &size) {
                return Err(UnifiedError::bad_request(
                    "default_paper_size must be one of paper_sizes",
                ));
            }
            settings.default_paper_size = size;
        }
        if let Some(values) = &input.paper_sizes {
            if values.is_empty() {
                return Err(UnifiedError::bad_request("paper_sizes must not be empty"));
            }
            settings.paper_sizes =
                require_string_list(values, "paper_sizes", MAX_CODE_LEN)?;
        }
        if let Some(values) = &input.semesters {
            if values.is_empty() {
                return Err(UnifiedError::bad_request("semesters must not be empty"));
            }
            settings.semesters = values.clone();
        }
        if let Some(values) = &input.intervention_kinds {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "intervention_kinds must not be empty",
                ));
            }
            settings.intervention_kinds =
                require_string_list(values, "intervention_kinds", MAX_CODE_LEN)?;
        }
        if let Some(values) = &input.lesson_plan_statuses {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "lesson_plan_statuses must not be empty",
                ));
            }
            settings.lesson_plan_statuses = require_string_list(
                values,
                "lesson_plan_statuses",
                MAX_CODE_LEN,
            )?;
        }
        if let Some(values) = &input.report_card_statuses {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "report_card_statuses must not be empty",
                ));
            }
            settings.report_card_statuses = require_string_list(
                values,
                "report_card_statuses",
                MAX_CODE_LEN,
            )?;
        }
        if let Some(values) = &input.intervention_statuses {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "intervention_statuses must not be empty",
                ));
            }
            settings.intervention_statuses = require_string_list(
                values,
                "intervention_statuses",
                MAX_CODE_LEN,
            )?;
        }
        if let Some(bands) = input.default_grade_bands {
            if bands.is_empty() {
                return Err(UnifiedError::bad_request(
                    "default_grade_bands must not be empty",
                ));
            }
            for band in &bands {
                require_score(band.min_score, "grade band min_score")?;
                require_score(band.max_score, "grade band max_score")?;
                if band.min_score > band.max_score {
                    return Err(UnifiedError::bad_request(
                        "Grade band min_score must not exceed max_score",
                    ));
                }
                require_text(&band.label, "grade band label", MAX_CODE_LEN)?;
            }
            settings.default_grade_bands = bands;
        }
        {
            let mut guard = state
                .settings
                .write()
                .expect("settings lock poisoned");
            *guard = settings.clone();
        }
        state.emit("curriculum.settings.updated", json!({ "settings": settings }));
        Ok(json!(settings))
    })();
    json::respond(result, debug)
}
