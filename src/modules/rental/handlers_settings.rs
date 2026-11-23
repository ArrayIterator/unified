//! HTTP handlers for per-tenant rental settings.

use actix_web::{web, HttpRequest, HttpResponse};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::helpers::{
    caller, caller_company, err, ok, require_permission, require_tenant, resolve_tenant,
};
use super::state::RentalState;
use super::types::{RentalSettings, UpdateSettingsRequest, PERM_RENTAL_MANAGE};

pub(crate) async fn get_settings(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        Ok(state.current_settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.current_settings();
        if let Some(grace) = input.late_fee_grace_hours {
            if grace < 0 {
                return Err(UnifiedError::bad_request(
                    "late_fee_grace_hours must not be negative",
                ));
            }
            settings.late_fee_grace_hours = grace;
        }
        if let Some(days) = input.max_advance_booking_days {
            if days <= 0 {
                return Err(UnifiedError::bad_request(
                    "max_advance_booking_days must be positive",
                ));
            }
            settings.max_advance_booking_days = days;
        }
        if let Some(hours) = input.cancel_cutoff_hours {
            if hours < 0 {
                return Err(UnifiedError::bad_request(
                    "cancel_cutoff_hours must not be negative",
                ));
            }
            settings.cancel_cutoff_hours = hours;
        }
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
