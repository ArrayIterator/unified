// Elearning - Settings handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Settings handlers

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ElearningSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ElearningSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        let mut validate_list = |values: &Option<Vec<String>>, field: &str| -> UnifiedResult<Option<Vec<String>>> {
            match values {
                Some(list) => {
                    if list.is_empty() {
                        return Err(UnifiedError::bad_request(format!(
                            "{field} must not be empty"
                        )));
                    }
                    let mut checked = Vec::with_capacity(list.len());
                    for value in list {
                        checked.push(require_text(value, field, MAX_SLUG_LEN)?);
                    }
                    Ok(Some(checked))
                }
                None => Ok(None),
            }
        };
        if let Some(levels) = validate_list(&input.levels, "levels")? {
            settings.levels = levels;
        }
        if let Some(categories) = validate_list(&input.categories, "categories")? {
            settings.categories = categories;
        }
        if let Some(languages) = validate_list(&input.languages, "languages")? {
            settings.languages = languages;
        }
        if let Some(lesson_types) = validate_list(&input.lesson_types, "lesson_types")? {
            settings.lesson_types = lesson_types;
        }
        if let Some(video_types) = validate_list(&input.video_lesson_types, "video_lesson_types")? {
            settings.video_lesson_types = video_types;
        }
        if let Some(text_types) = validate_list(&input.text_lesson_types, "text_lesson_types")? {
            settings.text_lesson_types = text_types;
        }
        if let Some(pass_mark) = input.default_pass_mark {
            if pass_mark < 0 || pass_mark > 100 {
                return Err(UnifiedError::bad_request(
                    "default_pass_mark must be between 0 and 100",
                ));
            }
            settings.default_pass_mark = pass_mark;
        }
        if let Some(max_attempts) = input.default_max_attempts {
            if let Some(limit) = max_attempts {
                if limit < 1 || limit > 1000 {
                    return Err(UnifiedError::bad_request(
                        "default_max_attempts must be between 1 and 1000",
                    ));
                }
            }
            settings.default_max_attempts = max_attempts;
        }
        if let Some(currency) = &input.default_currency {
            settings.default_currency = require_currency(currency)?;
        }
        if let Some(issuer) = &input.certificate_issuer_name {
            settings.certificate_issuer_name = require_text(issuer, "certificate_issuer_name", MAX_TITLE_LEN)?;
        }
        if let Some(enabled) = input.certificate_anchoring_enabled {
            settings.certificate_anchoring_enabled = enabled;
        }
        if let Some(network) = &input.anchoring_network_id {
            settings.anchoring_network_id = require_text(network, "anchoring_network_id", MAX_SLUG_LEN)?;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "elearning.settings.updated",
            json!({
                "levels": settings.levels,
                "categories": settings.categories,
                "certificate_anchoring_enabled": settings.certificate_anchoring_enabled,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
