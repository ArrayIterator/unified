//! exams - handlers settings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, hooks::*};



// Handlers: settings
pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamsSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        Ok(state.settings())
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<UpdateExamsSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamsSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(values) = input.question_types {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "question_types must not be empty",
                ));
            }
            settings.question_types = values;
        }
        if let Some(values) = input.difficulty_levels {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "difficulty_levels must not be empty",
                ));
            }
            settings.difficulty_levels = values;
        }
        if let Some(values) = input.categories {
            if values.is_empty() {
                return Err(UnifiedError::bad_request("categories must not be empty"));
            }
            settings.categories = values;
        }
        if let Some(values) = input.proctor_signal_types {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "proctor_signal_types must not be empty",
                ));
            }
            settings.proctor_signal_types = values;
        }
        if let Some(value) = input.default_pass_mark {
            settings.default_pass_mark = require_percentage(value, "default_pass_mark")?;
        }
        if let Some(value) = input.default_duration_minutes {
            if value < 1 || value > 10080 {
                return Err(UnifiedError::bad_request(
                    "default_duration_minutes must be between 1 and 10080",
                ));
            }
            settings.default_duration_minutes = value;
        }
        if let Some(value) = input.default_attempts_allowed {
            settings.default_attempts_allowed = value.max(1);
        }
        if let Some(value) = input.default_show_results {
            let checked = require_text(&value, "default_show_results", MAX_SLUG_LEN)?;
            if checked != "immediately" && checked != "after_grading" {
                return Err(UnifiedError::bad_request(
                    "default_show_results must be \"immediately\" or \"after_grading\"",
                ));
            }
            settings.default_show_results = checked;
        }
        if let Some(value) = input.distribution_buckets {
            if value < 1 || value > 100 {
                return Err(UnifiedError::bad_request(
                    "distribution_buckets must be between 1 and 100",
                ));
            }
            settings.distribution_buckets = value;
        }
        if let Some(value) = input.short_answer_case_insensitive {
            settings.short_answer_case_insensitive = value;
        }
        if let Some(value) = input.certificate_on_pass {
            settings.certificate_on_pass = value;
        }
        if let Some(value) = input.max_questions_per_exam {
            if value < 1 || value > 1000 {
                return Err(UnifiedError::bad_request(
                    "max_questions_per_exam must be between 1 and 1000",
                ));
            }
            settings.max_questions_per_exam = value;
        }
        if let Some(value) = input.max_options_per_question {
            if value < 2 || value > 20 {
                return Err(UnifiedError::bad_request(
                    "max_options_per_question must be between 2 and 20",
                ));
            }
            settings.max_options_per_question = value;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    json::respond(result, debug)
}
