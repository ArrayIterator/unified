//! manufacturing - handlers settings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{web, HttpRequest, HttpResponse};
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, reports::*};



// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------
pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ManufacturingSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ManufacturingSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(currency) = &input.currency {
            settings.currency = valid_currency(currency)?;
        }
        if let Some(capacity) = input.default_capacity_hours_per_day {
            settings.default_capacity_hours_per_day =
                valid_quantity(capacity, "default_capacity_hours_per_day")?;
        }
        if let Some(depth) = input.max_bom_depth {
            if depth == 0 || depth > MAX_BOM_DEPTH_HARD_CAP {
                return Err(UnifiedError::bad_request(format!(
                    "max_bom_depth must be between 1 and {MAX_BOM_DEPTH_HARD_CAP}"
                )));
            }
            settings.max_bom_depth = depth;
        }
        if let Some(allowance) = input.default_scrap_allowance_percent {
            settings.default_scrap_allowance_percent =
                valid_percent(allowance, "default_scrap_allowance_percent")?;
        }
        if let Some(enforce) = input.enforce_capacity_on_confirm {
            settings.enforce_capacity_on_confirm = enforce;
        }
        if let Some(units) = &input.units {
            if units.is_empty() {
                return Err(UnifiedError::bad_request(
                    "units must contain at least one unit",
                ));
            }
            let mut clean: Vec<String> = Vec::new();
            for unit in units {
                let unit = require_text(unit, "unit", MAX_UNIT_LEN)?.to_lowercase();
                if !clean.contains(&unit) {
                    clean.push(unit);
                }
            }
            settings.units = clean;
        }
        if let Some(transitions) = &input.work_order_transitions {
            for (from, targets) in transitions {
                let from = from.trim().to_lowercase();
                if !settings.work_order_statuses.contains(&from) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown work order status in transition map: {from}"
                    )));
                }
                for target in targets {
                    let target = target.trim().to_lowercase();
                    if !settings.work_order_statuses.contains(&target) {
                        return Err(UnifiedError::bad_request(format!(
                            "Unknown work order status in transition map: {target}"
                        )));
                    }
                }
            }
            settings.work_order_transitions = transitions.clone();
        }
        if let Some(statuses) = &input.work_order_statuses {
            if statuses.is_empty() {
                return Err(UnifiedError::bad_request(
                    "work_order_statuses must contain at least one status",
                ));
            }
            let mut clean: Vec<String> = Vec::new();
            for status in statuses {
                let status = require_text(status, "work order status", MAX_CODE_LEN)?
                    .to_lowercase();
                if !clean.contains(&status) {
                    clean.push(status);
                }
            }
            settings.work_order_statuses = clean;
        }
        *state
            .settings
            .write()
            .expect("manufacturing settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
