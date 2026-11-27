// Handlers: settings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;

use super::*;

// Handlers: settings

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(kinds) = &input.expense_kinds {
            if kinds.is_empty() {
                return Err(UnifiedError::bad_request(
                    "expense_kinds must not be empty",
                ));
            }
            let mut cleaned: Vec<String> = Vec::new();
            for kind in kinds {
                cleaned.push(require_text(kind, "expense kind", MAX_SLUG_LEN)?);
            }
            settings.expense_kinds = cleaned;
        }
        if let Some(currencies) = &input.supported_currencies {
            if currencies.is_empty() {
                return Err(UnifiedError::bad_request(
                    "supported_currencies must not be empty",
                ));
            }
            let mut cleaned: Vec<String> = Vec::new();
            for currency in currencies {
                cleaned.push(require_currency(currency)?);
            }
            settings.supported_currencies = cleaned;
        }
        if let Some(raw) = &input.default_currency {
            let currency = require_currency(raw)?;
            if !settings.supported_currencies.contains(&currency) {
                return Err(UnifiedError::bad_request(
                    "default_currency must be one of supported_currencies",
                ));
            }
            settings.default_currency = currency;
        }
        if let Some(value) = input.default_mileage_rate_minor {
            settings.default_mileage_rate_minor = require_amount(value, "default_mileage_rate_minor")?;
        }
        if let Some(raw) = &input.default_mileage_unit {
            settings.default_mileage_unit = require_text(raw, "default_mileage_unit", 16)?;
        }
        if let Some(value) = input.default_per_diem_rate_minor {
            settings.default_per_diem_rate_minor =
                require_amount(value, "default_per_diem_rate_minor")?;
        }
        if let Some(value) = input.report_total_warning_minor {
            settings.report_total_warning_minor =
                require_amount(value, "report_total_warning_minor")?;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "expenses.settings.updated",
            json!({
                "default_currency": settings.default_currency,
                "expense_kinds": settings.expense_kinds,
                "supported_currencies": settings.supported_currencies,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
