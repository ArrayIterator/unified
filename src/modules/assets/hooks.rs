//! Hook-bus handlers: answering requests from other modules.

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

// --- Stock hook answerers -------------------------------------------------
//
// Manufacturing asks for levels (`manufacturing.stock.requested`)
// and POS / manufacturing / field service ask for movements
// (`assets.stock.decrement/increment/adjust_requested`). All four
// are answered here, against the same stock rows the HTTP stock
// endpoints manage, with the same audited-adjustment recording.

/// Read a trimmed, non-empty string field from a hook payload.
fn hook_string(ctx: &HookContext, key: &str) -> Option<String> {
    ctx.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Read a quantity from a hook payload: an integer, or a whole
/// float. A fractional quantity is not a stock count and is
/// refused by the caller.
fn hook_quantity(ctx: &HookContext, key: &str) -> Option<i64> {
    let value = ctx.get(key)?;
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    let number = value.as_f64()?;
    if number.is_finite()
        && number.fract() == 0.0
        && number >= i64::MIN as f64
        && number <= i64::MAX as f64
    {
        Some(number as i64)
    } else {
        None
    }
}

/// Resolve the id of the stock row a stock event names: an
/// explicit `stock_id` wins; otherwise the single stock row of the
/// named asset (`asset_id`, falling back to `product_id` — POS and
/// manufacturing send the product id as the asset id). Stock is
/// one row per asset (creation refuses a second row), so the asset
/// lookup is unambiguous. Returns `None` when the payload names
/// no row this module holds. Only the id is returned, so the
/// caller can lock the row itself (see `apply_stock_change`).
fn stock_id_for_event(state: &AssetsState, ctx: &HookContext) -> Option<Uuid> {
    if let Some(raw) = hook_string(ctx, "stock_id") {
        if let Ok(id) = Uuid::parse_str(&raw) {
            if state.stock.contains_key(&id) {
                return Some(id);
            }
        }
    }
    let asset_id = hook_string(ctx, "asset_id").or_else(|| hook_string(ctx, "product_id"))?;
    let asset_id = Uuid::parse_str(&asset_id).ok()?;
    state
        .stock
        .iter()
        .find(|entry| entry.value().asset_id == asset_id)
        .map(|entry| *entry.key())
}

/// Answer a stock movement event as refused, without moving stock.
fn refuse_stock_change(ctx: &mut HookContext, reason: &str) {
    ctx.set("applied", json!(false));
    ctx.add_result(json!({
        "applied": false,
        "stock_id": Value::Null,
        "quantity_after": Value::Null,
        "reason": reason,
    }));
}

/// Apply a signed stock change for one event and answer it. The
/// change is recorded as an audited `AssetStockAdjustment` (the
/// same shape as the HTTP adjust), `assets.stock.adjusted` is
/// emitted, and the low-stock alert runs when the row crosses down
/// into low stock. The answer is set on the context (`applied`,
/// `stock_id`, `quantity_after`) and added as a result row, so
/// both synchronous readers and `emit_request`-style callers see
/// it. Refusals never move stock: unknown row, zero delta, a
/// quantity that would overflow, or a change that would take the
/// row below zero.
///
/// The row is mutated under its DashMap write guard
/// (`get_mut`), never cloned and written back, so two concurrent
/// decrements cannot both read the same quantity and lose one
/// movement. The guard is dropped before any event is emitted, so
/// a listener can never deadlock on the row.
fn apply_stock_change(state: &AssetsState, ctx: &mut HookContext, delta: i64, source: &str) {
    let Some(stock_id) = stock_id_for_event(state, ctx) else {
        refuse_stock_change(ctx, "stock_not_found");
        return;
    };
    if delta == 0 {
        refuse_stock_change(ctx, "zero_delta");
        return;
    }
    let Some(mut stock) = state.stock.get_mut(&stock_id) else {
        refuse_stock_change(ctx, "stock_not_found");
        return;
    };
    let Some(quantity_after) = stock.quantity_on_hand.checked_add(delta) else {
        refuse_stock_change(ctx, "quantity_out_of_range");
        return;
    };
    if quantity_after < 0 {
        refuse_stock_change(ctx, "insufficient_stock");
        return;
    }
    let reason = hook_string(ctx, "reason")
        .unwrap_or_else(|| format!("Stock change via {source}"))
        .chars()
        .take(MAX_REASON_LEN)
        .collect::<String>();
    let actor = hook_string(ctx, "requested_by")
        .or_else(|| hook_string(ctx, "actor"))
        .unwrap_or_else(|| source.to_string());
    let was_low = stock.quantity_on_hand <= stock.reorder_threshold;
    stock.quantity_on_hand = quantity_after;
    stock.updated_at = Utc::now();
    let updated = stock.clone();
    drop(stock);
    let adjustment = AssetStockAdjustment {
        id: Uuid::now_v7(),
        stock_id: updated.id,
        asset_id: updated.asset_id,
        delta,
        quantity_after,
        reason,
        adjusted_by: actor.clone(),
        created_at: Utc::now(),
    };
    state.adjustments.insert(adjustment.id, adjustment.clone());
    if !was_low {
        check_low_stock(state, &updated);
    }
    state.emit(
        "assets.stock.adjusted",
        json!({
            "asset_id": updated.asset_id.to_string(),
            "stock_id": updated.id.to_string(),
            "adjustment_id": adjustment.id.to_string(),
            "delta": delta,
            "quantity_after": quantity_after,
            "actor": actor,
        }),
    );
    ctx.set("applied", json!(true));
    ctx.set("stock_id", json!(updated.id.to_string()));
    ctx.set("quantity_after", json!(quantity_after));
    ctx.add_result(json!({
        "applied": true,
        "stock_id": updated.id.to_string(),
        "quantity_after": quantity_after,
        "adjustment_id": adjustment.id.to_string(),
    }));
}

/// Answer `assets.stock.decrement_requested` (POS sale,
/// manufacturing consumption): subtract a positive `quantity`
/// from the named stock row.
pub(crate) fn on_stock_decrement_requested(state: AssetsState, ctx: &mut HookContext) {
    match hook_quantity(ctx, "quantity") {
        Some(quantity) if quantity > 0 => {
            apply_stock_change(&state, ctx, -quantity, "stock_decrement")
        }
        _ => refuse_stock_change(ctx, "invalid_quantity"),
    }
}

/// Answer `assets.stock.increment_requested` (POS refund): add a
/// positive `quantity` to the named stock row.
pub(crate) fn on_stock_increment_requested(state: AssetsState, ctx: &mut HookContext) {
    match hook_quantity(ctx, "quantity") {
        Some(quantity) if quantity > 0 => {
            apply_stock_change(&state, ctx, quantity, "stock_increment")
        }
        _ => refuse_stock_change(ctx, "invalid_quantity"),
    }
}

/// Answer `assets.stock.adjust_requested` (field-service part
/// consumption): move the named stock row by a signed `delta`.
pub(crate) fn on_stock_adjust_requested(state: AssetsState, ctx: &mut HookContext) {
    match hook_quantity(ctx, "delta") {
        Some(delta) => apply_stock_change(&state, ctx, delta, "stock_adjust"),
        None => refuse_stock_change(ctx, "invalid_delta"),
    }
}

/// Answer `manufacturing.stock.requested` with per-product levels
/// for the requested products this module holds a stock row for.
/// Products with no stock row are omitted, never invented, and
/// when no requested product has a row no answer is given at all
/// — the requester then keeps its `unknown` /
/// `pending_confirmation` behaviour instead of treating an empty
/// answer as a level. `reserved` is always 0 (this module holds
/// no reservations) and `unit` is null (stock rows carry no unit).
pub(crate) fn on_manufacturing_stock_requested(state: AssetsState, ctx: &mut HookContext) {
    let Some(product_ids) = ctx.get("product_ids").and_then(|value| value.as_array()) else {
        return;
    };
    let mut stock = serde_json::Map::new();
    for raw in product_ids {
        let Some(product_id) = raw.as_str().map(str::trim).filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Ok(asset_id) = Uuid::parse_str(product_id) else {
            continue;
        };
        let Some(row) = state
            .stock
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.asset_id == asset_id)
        else {
            continue;
        };
        stock.insert(
            product_id.to_string(),
            json!({
                "available": row.quantity_on_hand,
                "reserved": 0,
                "unit": Value::Null,
            }),
        );
    }
    if stock.is_empty() {
        return;
    }
    let answer = json!({ "stock": stock });
    ctx.set("stock", answer["stock"].clone());
    ctx.add_result(answer);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stock(state: &AssetsState, quantity: i64) -> AssetStock {
        let stock = AssetStock {
            id: Uuid::now_v7(),
            asset_id: Uuid::new_v4(),
            quantity_on_hand: quantity,
            reorder_threshold: 0,
            location: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.stock.insert(stock.id, stock.clone());
        stock
    }

    fn decrement_ctx(stock_id: Uuid, quantity: i64) -> HookContext {
        HookContext::new(
            "assets.stock.decrement_requested",
            json!({
                "stock_id": stock_id.to_string(),
                "quantity": quantity,
                "requested_by": "test",
            }),
        )
    }

    #[test]
    fn concurrent_decrements_lose_no_movement() {
        let state = AssetsState::default();
        let stock = test_stock(&state, 100);
        // Ten threads decrement by 10 each. With a clone-and-write-
        // back race, two threads could read the same quantity and
        // one movement would be lost; under the row guard the final
        // quantity must be exactly 0 with ten audited adjustments.
        std::thread::scope(|scope| {
            for _ in 0..10 {
                let state = state.clone();
                let stock_id = stock.id;
                scope.spawn(move || {
                    let mut ctx = decrement_ctx(stock_id, 10);
                    on_stock_decrement_requested(state, &mut ctx);
                    assert_eq!(ctx.get("applied").and_then(Value::as_bool), Some(true));
                });
            }
        });
        let final_stock = state
            .stock
            .get(&stock.id)
            .map(|entry| entry.value().clone())
            .expect("stock row must still exist");
        assert_eq!(final_stock.quantity_on_hand, 0);
        assert_eq!(state.adjustments.len(), 10);
    }

    #[test]
    fn stock_id_for_event_prefers_explicit_stock_id() {
        let state = AssetsState::default();
        let stock = test_stock(&state, 5);
        let ctx = decrement_ctx(stock.id, 1);
        assert_eq!(stock_id_for_event(&state, &ctx), Some(stock.id));
    }

    #[test]
    fn stock_id_for_event_falls_back_to_asset_id() {
        let state = AssetsState::default();
        let stock = test_stock(&state, 5);
        let ctx = HookContext::new(
            "assets.stock.decrement_requested",
            json!({ "asset_id": stock.asset_id.to_string(), "quantity": 1 }),
        );
        assert_eq!(stock_id_for_event(&state, &ctx), Some(stock.id));
    }

    #[test]
    fn apply_stock_change_refuses_overflow_and_negative() {
        let state = AssetsState::default();
        let stock = test_stock(&state, i64::MAX);
        let mut ctx = HookContext::new(
            "assets.stock.increment_requested",
            json!({ "stock_id": stock.id.to_string(), "quantity": 1 }),
        );
        on_stock_increment_requested(state.clone(), &mut ctx);
        assert_eq!(ctx.get("applied").and_then(Value::as_bool), Some(false));

        let stock = test_stock(&state, 0);
        let mut ctx = decrement_ctx(stock.id, 1);
        on_stock_decrement_requested(state.clone(), &mut ctx);
        assert_eq!(ctx.get("applied").and_then(Value::as_bool), Some(false));
        assert_eq!(
            state
                .stock
                .get(&stock.id)
                .map(|entry| entry.value().quantity_on_hand),
            Some(0)
        );
    }
}
