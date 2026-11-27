//! Depreciation requests to the Calculator module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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

use super::*;
use super::helpers::{minor_to_major};

/// Depreciation: ask the Calculator module for a schedule and store
/// what comes back. The math is never done here. When the
/// Calculator module is not registered, or no handler answers, or
/// the answer carries an error, the asset is left with
/// `depreciation_status: "pending"` and the reason in
/// `depreciation_error` - an honest record, never a guessed
/// schedule. Returns true when a schedule was stored.
///
/// Resolved (A-M1): the Calculator module now answers
/// `calculator.depreciation_requested` (see
/// `calculator/hooks.rs::on_depreciation_requested`), so with a
/// depreciation method set the asset stores the returned schedule
/// instead of staying `pending`. The `pending` fallback above
/// still applies when the Calculator module is not enabled or no
/// handler answers.
pub(crate) fn request_depreciation(ctx: &AppContext, state: &AssetsState, asset: &mut Asset) -> bool {
    let Some(method) = asset.depreciation_method else {
        asset.depreciation_status = "none".to_string();
        asset.depreciation_schedule = None;
        asset.depreciation_error = None;
        return false;
    };
    let Some(useful_life) = asset.useful_life_years else {
        asset.depreciation_status = "pending".to_string();
        asset.depreciation_schedule = None;
        asset.depreciation_error =
            Some("Depreciation needs useful_life_years before a schedule can be requested".to_string());
        state.emit(
            "assets.depreciation.pending",
            json!({
                "asset_id": asset.id.to_string(),
                "method": method.as_str(),
                "reason": "missing useful_life_years",
            }),
        );
        return false;
    };
    if ctx.modules.get("calculator").is_none() {
        asset.depreciation_status = "pending".to_string();
        asset.depreciation_schedule = None;
        asset.depreciation_error =
            Some("Calculator module is not enabled, so no depreciation schedule is available".to_string());
        state.emit(
            "assets.depreciation.pending",
            json!({
                "asset_id": asset.id.to_string(),
                "method": method.as_str(),
                "reason": "calculator module is not enabled",
            }),
        );
        return false;
    }
    let answer = ctx.hooks.emit(
        "calculator.depreciation_requested",
        json!({
            "asset_id": asset.id.to_string(),
            "method": method.as_str(),
            "purchase_price": minor_to_major(asset.purchase_price, &asset.currency),
            "salvage_value": minor_to_major(asset.salvage_value, &asset.currency),
            "useful_life_years": useful_life,
            "purchase_date": asset.purchase_date.map(|date| date.to_string()),
            "currency": asset.currency,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("schedule").and_then(Value::as_array).is_some() => {
            asset.depreciation_status = "computed".to_string();
            asset.depreciation_schedule = result.get("schedule").cloned();
            asset.depreciation_error = None;
            state.emit(
                "assets.depreciation.computed",
                json!({
                    "asset_id": asset.id.to_string(),
                    "method": method.as_str(),
                }),
            );
            true
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Calculator module did not return a depreciation schedule")
                .to_string();
            asset.depreciation_status = "pending".to_string();
            asset.depreciation_schedule = None;
            asset.depreciation_error = Some(reason.clone());
            state.emit(
                "assets.depreciation.pending",
                json!({
                    "asset_id": asset.id.to_string(),
                    "method": method.as_str(),
                    "reason": reason,
                }),
            );
            false
        }
        None => {
            asset.depreciation_status = "pending".to_string();
            asset.depreciation_schedule = None;
            asset.depreciation_error = Some(
                "Calculator module did not answer calculator.depreciation_requested".to_string(),
            );
            state.emit(
                "assets.depreciation.pending",
                json!({
                    "asset_id": asset.id.to_string(),
                    "method": method.as_str(),
                    "reason": "no handler answered calculator.depreciation_requested",
                }),
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    fn test_asset() -> Asset {
        Asset {
            id: Uuid::new_v4(),
            name: "Test asset".to_string(),
            description: None,
            asset_type: AssetType::Hardware,
            serial_number: None,
            purchase_date: None,
            purchase_price: 100000,
            currency: "USD".to_string(),
            vendor: None,
            location: None,
            status: AssetStatus::InStock,
            warranty_end: None,
            license_expires_at: None,
            license_seats: None,
            depreciation_method: Some(DepreciationMethod::StraightLine),
            useful_life_years: Some(5),
            salvage_value: 0,
            depreciation_status: "pending".to_string(),
            depreciation_schedule: None,
            depreciation_error: None,
            created_by: "tester".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn depreciation_without_calculator_stays_pending() {
        // No calculator module is registered in this context, so
        // no listener answers `calculator.depreciation_requested`
        // here (the Calculator module registers one in production):
        // the request must stay honestly pending, never a guessed
        // schedule.
        let ctx = test_context();
        let state = AssetsState::default();
        let mut asset = test_asset();
        assert!(!request_depreciation(&ctx, &state, &mut asset));
        assert_eq!(asset.depreciation_status, "pending");
        assert!(asset.depreciation_schedule.is_none());
        assert!(asset
            .depreciation_error
            .as_deref()
            .unwrap_or_default()
            .contains("Calculator module is not enabled"));
    }

    #[test]
    fn depreciation_without_method_is_none() {
        let ctx = test_context();
        let state = AssetsState::default();
        let mut asset = test_asset();
        asset.depreciation_method = None;
        assert!(!request_depreciation(&ctx, &state, &mut asset));
        assert_eq!(asset.depreciation_status, "none");
    }
}
