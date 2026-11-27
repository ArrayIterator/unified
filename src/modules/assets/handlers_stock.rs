//! HTTP handlers: consumable stock.

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

// Stock handlers

/// Checked stock arithmetic: `on_hand + delta`, refusing overflow
/// with a 409 instead of panicking (debug) or wrapping (release).
fn checked_quantity_after(on_hand: i64, delta: i64) -> UnifiedResult<i64> {
    on_hand.checked_add(delta).ok_or_else(|| {
        UnifiedError::conflict(format!(
            "Adjustment of {delta} would overflow the stock quantity (on hand: {on_hand})"
        ))
    })
}

/// True when a stock edit crosses from not-low into low: the
/// quantity was above the old threshold and sits at or below the
/// new one. Editing a row that was already low never crosses.
fn crosses_into_low(quantity: i64, old_threshold: i64, new_threshold: i64) -> bool {
    quantity > old_threshold && quantity <= new_threshold
}

pub(crate) async fn list_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let asset_filter = params.filters.get("asset_id").cloned();
        let low_stock_only = params
            .filters
            .get("low_stock")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AssetStock> = state
            .stock
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stock| {
                if let Some(asset_id) = &asset_filter {
                    if stock.asset_id.to_string() != *asset_id {
                        return false;
                    }
                }
                if low_stock_only && stock.quantity_on_hand > stock.reorder_threshold {
                    return false;
                }
                if !search.is_empty() {
                    let asset_name = state
                        .assets
                        .get(&stock.asset_id)
                        .map(|entry| entry.value().name.clone())
                        .unwrap_or_default()
                        .to_lowercase();
                    if !asset_name.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    body: web::Json<CreateStockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStock> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let input = body.into_inner();
        let asset = state
            .assets
            .get(&input.asset_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Asset not found: {}", input.asset_id)))?;
        if asset.asset_type != AssetType::Consumable {
            return Err(UnifiedError::bad_request(
                "Stock items can only be created for consumable assets",
            ));
        }
        if state
            .stock
            .iter()
            .any(|entry| entry.value().asset_id == input.asset_id)
        {
            return Err(UnifiedError::conflict(
                "A stock item already exists for this asset",
            ));
        }
        if input.quantity_on_hand < 0 {
            return Err(UnifiedError::bad_request("quantity_on_hand must be >= 0"));
        }
        let reorder_threshold = match input.reorder_threshold {
            Some(threshold) => {
                if threshold < 0 {
                    return Err(UnifiedError::bad_request("reorder_threshold must be >= 0"));
                }
                threshold
            }
            None => 0,
        };
        let location = match &input.location {
            Some(raw) => Some(require_text(raw, "location", MAX_LOCATION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let stock = AssetStock {
            id: Uuid::now_v7(),
            asset_id: input.asset_id,
            quantity_on_hand: input.quantity_on_hand,
            reorder_threshold,
            location,
            created_at: now,
            updated_at: now,
        };
        state.stock.insert(stock.id, stock.clone());
        check_low_stock(&state, &stock);
        state.emit(
            "assets.stock.created",
            json!({
                "asset_id": stock.asset_id.to_string(),
                "stock_id": stock.id.to_string(),
                "quantity_on_hand": stock.quantity_on_hand,
                "actor": caller.actor,
            }),
        );
        Ok(stock)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStock> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        load_stock(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateStockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStock> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let existing = load_stock(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(threshold) = input.reorder_threshold {
            if threshold < 0 {
                return Err(UnifiedError::bad_request("reorder_threshold must be >= 0"));
            }
        }
        let new_location: Option<Option<String>> = match &input.location {
            Some(raw) => match raw {
                Some(value) => Some(Some(require_text(value, "location", MAX_LOCATION_LEN)?)),
                None => Some(None),
            },
            None => None,
        };
        // Lock the row for the read-modify-write: mutate the
        // guarded row in place instead of cloning it and writing
        // it back, so a concurrent adjustment cannot be lost.
        let Some(mut stock) = state.stock.get_mut(&existing.id) else {
            return Err(UnifiedError::not_found(format!(
                "Stock item not found: {}",
                existing.id
            )));
        };
        let old_threshold = stock.reorder_threshold;
        if let Some(threshold) = input.reorder_threshold {
            stock.reorder_threshold = threshold;
        }
        if let Some(location) = new_location {
            stock.location = location;
        }
        stock.updated_at = Utc::now();
        let updated = stock.clone();
        drop(stock);
        // Only alert when the edit crosses the row from not-low
        // into low (or the threshold change itself newly makes it
        // low); editing a location while already low re-notifies
        // never.
        if crosses_into_low(
            updated.quantity_on_hand,
            old_threshold,
            updated.reorder_threshold,
        ) {
            check_low_stock(&state, &updated);
        }
        state.emit(
            "assets.stock.updated",
            json!({
                "asset_id": updated.asset_id.to_string(),
                "stock_id": updated.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn adjust_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AdjustStockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockAdjustment> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let existing = load_stock(&state, &path.into_inner())?;
        let input = body.into_inner();
        if input.delta == 0 {
            return Err(UnifiedError::bad_request("delta must not be 0"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        // Lock the row for the read-modify-write: mutate the
        // guarded row in place instead of cloning it and writing
        // it back, so two concurrent decrements cannot both read
        // the same quantity and lose one movement.
        let Some(mut stock) = state.stock.get_mut(&existing.id) else {
            return Err(UnifiedError::not_found(format!(
                "Stock item not found: {}",
                existing.id
            )));
        };
        let quantity_after = checked_quantity_after(stock.quantity_on_hand, input.delta)?;
        if quantity_after < 0 {
            return Err(UnifiedError::conflict(format!(
                "Adjustment of {} would take stock below 0 (on hand: {})",
                input.delta, stock.quantity_on_hand
            )));
        }
        let was_low = stock.quantity_on_hand <= stock.reorder_threshold;
        stock.quantity_on_hand = quantity_after;
        stock.updated_at = Utc::now();
        let updated = stock.clone();
        drop(stock);
        let adjustment = AssetStockAdjustment {
            id: Uuid::now_v7(),
            stock_id: updated.id,
            asset_id: updated.asset_id,
            delta: input.delta,
            quantity_after,
            reason,
            adjusted_by: caller.actor.clone(),
            created_at: Utc::now(),
        };
        state.adjustments.insert(adjustment.id, adjustment.clone());
        if !was_low {
            check_low_stock(&state, &updated);
        }
        state.emit(
            "assets.stock.adjusted",
            json!({
                "asset_id": updated.asset_id.to_string(),
                "stock_id": updated.id.to_string(),
                "adjustment_id": adjustment.id.to_string(),
                "delta": input.delta,
                "quantity_after": quantity_after,
                "actor": caller.actor,
            }),
        );
        Ok(adjustment)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_stock_adjustments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let stock = load_stock(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<AssetStockAdjustment> = state
            .adjustments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|adjustment| adjustment.stock_id == stock.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_quantity_after_cases() {
        assert_eq!(checked_quantity_after(10, 5).unwrap(), 15);
        assert_eq!(checked_quantity_after(10, -3).unwrap(), 7);
        assert_eq!(checked_quantity_after(0, 0).unwrap(), 0);
    }

    #[test]
    fn checked_quantity_after_refuses_overflow_with_409() {
        assert_eq!(checked_quantity_after(i64::MAX, 1).unwrap_err().code, 409);
        assert_eq!(
            checked_quantity_after(i64::MIN, -1).unwrap_err().code,
            409
        );
        assert_eq!(
            checked_quantity_after(i64::MAX, i64::MAX).unwrap_err().code,
            409
        );
    }

    #[test]
    fn crosses_into_low_cases() {
        // Threshold raised so the row newly becomes low.
        assert!(crosses_into_low(5, 3, 5));
        assert!(crosses_into_low(5, 4, 6));
        assert!(crosses_into_low(5, 3, 10));
        // Still above the new threshold: no crossing.
        assert!(!crosses_into_low(5, 10, 4));
        // Already low before the edit: no re-alert.
        assert!(!crosses_into_low(5, 5, 5));
        assert!(!crosses_into_low(5, 5, 10));
        assert!(!crosses_into_low(5, 8, 8));
    }
}
