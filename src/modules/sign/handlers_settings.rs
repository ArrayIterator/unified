//! HTTP handlers for sign settings: per-tenant signature
//! configuration (allowed capture methods, default expiry,
//! default reminder schedule, provider selection).

use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::helpers::{
    caller, caller_company, err, ok, require_permission, require_tenant, require_text,
    resolve_tenant, validate_reminder_schedule,
};
use super::state::SignState;
use super::types::{
    PERM_SIGN_MANAGE, PERM_SIGN_VIEW, UpdateSettingsRequest, valid_signature_method,
};

pub(crate) async fn get_settings(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let settings = state.settings_for(&tenant_id);
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let mut settings = state.settings_for(&tenant_id);
        let input = body.into_inner();
        if let Some(signature_types) = input.signature_types {
            if signature_types.is_empty() {
                return Err(UnifiedError::bad_request(
                    "signature_types must contain at least one capture method",
                ));
            }
            for kind in &signature_types {
                if !valid_signature_method(kind) {
                    return Err(UnifiedError::bad_request(format!(
                        "signature_types must only contain drawn, typed, or uploaded, got \"{kind}\""
                    )));
                }
            }
            let mut unique = signature_types.clone();
            unique.sort();
            unique.dedup();
            if unique.len() != signature_types.len() {
                return Err(UnifiedError::bad_request(
                    "signature_types must not repeat a capture method",
                ));
            }
            settings.signature_types = signature_types;
        }
        if let Some(default_expiry_days) = input.default_expiry_days {
            if default_expiry_days < 1 {
                return Err(UnifiedError::bad_request(
                    "default_expiry_days must be at least 1",
                ));
            }
            settings.default_expiry_days = default_expiry_days;
        }
        if let Some(schedule) = input.default_reminder_schedule {
            validate_reminder_schedule(&schedule)?;
            settings.default_reminder_schedule = schedule;
        }
        if let Some(reminder_enabled) = input.reminder_enabled {
            settings.reminder_enabled = reminder_enabled;
        }
        if let Some(require_view_before_sign) = input.require_view_before_sign {
            settings.require_view_before_sign = require_view_before_sign;
        }
        if let Some(provider) = input.provider {
            // The provider is a name, not a behaviour: it selects
            // configuration, so any non-empty name is accepted.
            settings.provider = require_text(&provider, "provider", 128)?;
        }
        if let Some(provider_settings) = input.provider_settings {
            settings.provider_settings = Some(provider_settings);
        }
        state.settings.insert(tenant_id, settings.clone());
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
