// admissions module — Settings handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{KEY_APPLICATION_STATUSES, KEY_CHECKLIST_STATUSES, KEY_DECISIONS, KEY_FEE_CURRENCY, KEY_PERIOD_STATUSES, KEY_RESULT_VISIBLE, KEY_SCORE_SCALE_MAX, KEY_WAITLIST_ORDER, KEY_WAITLIST_STATUSES, MAX_SETTING_KEY, MAX_SETTING_VALUE, MAX_TEXT, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ, WAITLIST_ORDERS};
use super::types::{AdmissionSetting, UpdateSetting};
use super::state::AdmissionsState;
use super::helpers::{
    caller, caller_company, matches_search, now, paginate, require_permission, require_tenant,
    require_text, valid_currency,
};

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

pub(crate) async fn list_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AdmissionSetting>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AdmissionSetting> = state
            .settings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|setting| matches_search(&format!("{} {}", setting.key, setting.value), &params))
            .collect();
        items.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_setting(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    body: web::Json<UpdateSetting>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionSetting> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let input = body.into_inner();
        let key = require_text(&input.key, "key", MAX_SETTING_KEY)?;
        if !key
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_')
        {
            return Err(UnifiedError::bad_request(
                "key must use only lowercase letters, digits, '.' and '_'",
            ));
        }
        let value = require_text(&input.value, "value", MAX_SETTING_VALUE)?;
        // Vocabulary settings must stay non-empty lists; the waitlist
        // order must name a known policy; the visibility flag must be
        // a boolean; the score scale must be a positive number.
        match key.as_str() {
            KEY_PERIOD_STATUSES | KEY_APPLICATION_STATUSES | KEY_CHECKLIST_STATUSES
            | KEY_WAITLIST_STATUSES | KEY_DECISIONS => {
                let parts: Vec<&str> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect();
                if parts.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "a vocabulary setting must list at least one value",
                    ));
                }
            }
            KEY_WAITLIST_ORDER => {
                if !WAITLIST_ORDERS.contains(&value.trim()) {
                    return Err(UnifiedError::bad_request(format!(
                        "waitlist order must be one of: {}",
                        WAITLIST_ORDERS.join(", ")
                    )));
                }
            }
            KEY_RESULT_VISIBLE => {
                if !matches!(value.trim().to_lowercase().as_str(), "true" | "false") {
                    return Err(UnifiedError::bad_request(
                        "result visibility must be true or false",
                    ));
                }
            }
            KEY_SCORE_SCALE_MAX => {
                let parsed: f64 = value.trim().parse().map_err(|_| {
                    UnifiedError::bad_request("score scale max must be a number")
                })?;
                if !parsed.is_finite() || parsed <= 0.0 || parsed > 1000.0 {
                    return Err(UnifiedError::bad_request(
                        "score scale max must be between 0 and 1000",
                    ));
                }
            }
            KEY_FEE_CURRENCY => {
                valid_currency(&value)?;
            }
            _ => {}
        }
        let description = match input.description {
            Some(description) => require_text(&description, "description", MAX_TEXT)?,
            None => state
                .settings
                .get(&key)
                .map(|entry| entry.value().description.clone())
                .unwrap_or_default(),
        };
        let setting = AdmissionSetting {
            key: key.clone(),
            value,
            description,
            updated_at: now(),
        };
        state.settings.insert(key, setting.clone());
        Ok(setting)
    })();
    json::respond(result, debug)
}
