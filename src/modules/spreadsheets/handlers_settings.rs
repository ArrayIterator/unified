//! HTTP handlers for spreadsheets settings: per-tenant
//! number formats, share roles, default currency, public-link
//! switch, and sheet bound. Formats and roles are data, so
//! the settings row is where a tenant changes them.

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{
    caller, caller_company, err, ok, require_caller_company, require_manage, require_tenant,
    require_view, validate_settings,
};
use super::state::SpreadsheetsState;
use super::types::{SpreadsheetSettings, UpdateSettingsRequest};

pub(crate) async fn get_settings(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let settings = state.settings_for(&tenant_id);
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut settings = state.settings_for(&tenant_id);
        let input = body.into_inner();
        if let Some(default_currency) = input.default_currency {
            settings.default_currency = default_currency;
        }
        if let Some(number_formats) = input.number_formats {
            settings.number_formats = number_formats;
        }
        if let Some(share_roles) = input.share_roles {
            settings.share_roles = share_roles;
        }
        if let Some(default_share_role) = input.default_share_role {
            settings.default_share_role = default_share_role;
        }
        if let Some(public_links_allowed) = input.public_links_allowed {
            settings.public_links_allowed = public_links_allowed;
        }
        if let Some(max_sheets_per_workbook) = input.max_sheets_per_workbook {
            settings.max_sheets_per_workbook = max_sheets_per_workbook;
        }
        validate_settings(&settings)?;
        state.settings.insert(tenant_id, settings.clone());
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
