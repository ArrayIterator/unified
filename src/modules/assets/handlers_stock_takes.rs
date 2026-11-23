//! HTTP handlers: stock takes.

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

// ---------------------------------------------------------------------------
// Stock-take handlers
// ---------------------------------------------------------------------------

/// Checked stock-take totals: sums of expected / counted
/// quantities and their difference, refusing overflow with a 409
/// instead of panicking (debug) or wrapping (release).
fn checked_stock_take_totals(lines: &[AssetStockTakeLine]) -> UnifiedResult<(i64, i64, i64)> {
    let mut total_expected: i64 = 0;
    let mut total_counted: i64 = 0;
    for line in lines {
        let counted = line.counted_quantity.unwrap_or(0);
        total_expected = total_expected
            .checked_add(line.expected_quantity)
            .ok_or_else(|| {
                UnifiedError::conflict("Stock take totals overflowed the quantity range")
            })?;
        total_counted = total_counted.checked_add(counted).ok_or_else(|| {
            UnifiedError::conflict("Stock take totals overflowed the quantity range")
        })?;
    }
    let total_discrepancy = total_counted
        .checked_sub(total_expected)
        .ok_or_else(|| UnifiedError::conflict("Stock take totals overflowed the quantity range"))?;
    Ok((total_expected, total_counted, total_discrepancy))
}

pub(crate) async fn list_stock_takes(
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
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AssetStockTake> = state
            .stock_takes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|take| {
                if let Some(status) = &status_filter {
                    if take.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() && !take.title.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_stock_take(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    body: web::Json<CreateStockTakeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockTake> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_NAME_LEN)?;
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let take = AssetStockTake {
            id: Uuid::now_v7(),
            title,
            notes,
            status: StockTakeStatus::Open,
            started_by: caller.actor.clone(),
            started_at: now,
            closed_at: None,
            closed_by: None,
            created_at: now,
            updated_at: now,
        };
        state.stock_takes.insert(take.id, take.clone());
        state.emit(
            "assets.stock_take.started",
            json!({
                "stock_take_id": take.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(take)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

fn stock_take_view(state: &AssetsState, take: &AssetStockTake) -> Value {
    let mut lines: Vec<Value> = state
        .stock_take_lines
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|line| line.stock_take_id == take.id)
        .map(|line| {
            json!({
                "id": line.id,
                "stock_take_id": line.stock_take_id,
                "asset_id": line.asset_id,
                "stock_id": line.stock_id,
                "expected_quantity": line.expected_quantity,
                "counted_quantity": line.counted_quantity,
                "discrepancy": line.discrepancy(),
                "counted_by": line.counted_by,
                "note": line.note,
                "created_at": line.created_at,
                "updated_at": line.updated_at,
            })
        })
        .collect();
    lines.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .cmp(&b.get("created_at").and_then(Value::as_str))
    });
    let mut value = serde_json::to_value(take).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("lines".to_string(), Value::Array(lines));
    }
    value
}

pub(crate) async fn get_stock_take(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let take = load_stock_take(&state, &path.into_inner())?;
        Ok(stock_take_view(&state, &take))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_stock_take_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddStockTakeLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockTakeLine> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let take = load_stock_take(&state, &path.into_inner())?;
        if take.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict(
                "Lines can only be added to an open stock take",
            ));
        }
        let input = body.into_inner();
        let asset = state
            .assets
            .get(&input.asset_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Asset not found: {}", input.asset_id)))?;
        if asset.asset_type != AssetType::Consumable {
            return Err(UnifiedError::bad_request(
                "Stock takes count consumable assets only",
            ));
        }
        let stock = match input.stock_id {
            Some(stock_id) => state
                .stock
                .get(&stock_id)
                .map(|entry| entry.value().clone())
                .filter(|item| item.asset_id == input.asset_id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Stock item not found: {stock_id}"))
                })?,
            None => state
                .stock
                .iter()
                .map(|entry| entry.value().clone())
                .find(|item| item.asset_id == input.asset_id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "No stock item exists for asset {}",
                        input.asset_id
                    ))
                })?,
        };
        if state.stock_take_lines.iter().any(|entry| {
            entry.value().stock_take_id == take.id && entry.value().asset_id == input.asset_id
        }) {
            return Err(UnifiedError::conflict(
                "This asset already has a count line in this stock take",
            ));
        }
        let now = Utc::now();
        let line = AssetStockTakeLine {
            id: Uuid::now_v7(),
            stock_take_id: take.id,
            asset_id: input.asset_id,
            stock_id: stock.id,
            expected_quantity: stock.quantity_on_hand,
            counted_quantity: None,
            counted_by: None,
            note: None,
            created_at: now,
            updated_at: now,
        };
        state.stock_take_lines.insert(line.id, line.clone());
        Ok(line)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn count_stock_take_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CountStockTakeLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockTakeLine> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let (take_id, line_id) = path.into_inner();
        let take = load_stock_take(&state, &take_id)?;
        if take.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict(
                "Lines can only be counted on an open stock take",
            ));
        }
        let line = load_stock_take_line(&state, take.id, &line_id)?;
        let input = body.into_inner();
        if input.counted_quantity < 0 {
            return Err(UnifiedError::bad_request("counted_quantity must be >= 0"));
        }
        let note = match &input.note {
            Some(raw) => Some(require_text(raw, "note", MAX_REASON_LEN)?),
            None => None,
        };
        // Lock the row for the read-modify-write: mutate the
        // guarded row in place instead of cloning it and writing
        // it back, so a concurrent count of the same line cannot
        // be lost.
        let Some(mut line) = state.stock_take_lines.get_mut(&line.id) else {
            return Err(UnifiedError::not_found(format!(
                "Stock take line not found: {}",
                line.id
            )));
        };
        line.counted_quantity = Some(input.counted_quantity);
        line.counted_by = Some(caller.actor.clone());
        line.note = note;
        line.updated_at = Utc::now();
        let updated = line.clone();
        drop(line);
        Ok(updated)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn close_stock_take(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let take = load_stock_take(&state, &path.into_inner())?;
        if take.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict("Stock take is already closed"));
        }
        let lines: Vec<AssetStockTakeLine> = state
            .stock_take_lines
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|line| line.stock_take_id == take.id)
            .collect();
        if lines.is_empty() {
            return Err(UnifiedError::conflict(
                "A stock take with no count lines cannot be closed",
            ));
        }
        let uncounted = lines
            .iter()
            .filter(|line| line.counted_quantity.is_none())
            .count();
        if uncounted > 0 {
            return Err(UnifiedError::conflict(format!(
                "{uncounted} count line(s) still have no counted quantity"
            )));
        }
        // Totals are computed before the status flip, so a 409
        // from the checked totals leaves the take open instead of
        // closing it and then failing.
        let mut discrepancies: Vec<Value> = Vec::new();
        for line in &lines {
            let counted = line.counted_quantity.unwrap_or(0);
            let discrepancy = line.discrepancy().unwrap_or(0);
            if discrepancy != 0 {
                discrepancies.push(json!({
                    "line_id": line.id,
                    "asset_id": line.asset_id,
                    "stock_id": line.stock_id,
                    "expected_quantity": line.expected_quantity,
                    "counted_quantity": counted,
                    "discrepancy": discrepancy,
                }));
            }
        }
        let (total_expected, total_counted, total_discrepancy) = checked_stock_take_totals(&lines)?;
        // Flip the status under the row guard, re-checking it is
        // still open: two concurrent closes cannot both succeed
        // and both emit a close event.
        let now = Utc::now();
        let Some(mut guarded) = state.stock_takes.get_mut(&take.id) else {
            return Err(UnifiedError::not_found(format!(
                "Stock take not found: {}",
                take.id
            )));
        };
        if guarded.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict("Stock take is already closed"));
        }
        guarded.status = StockTakeStatus::Closed;
        guarded.closed_at = Some(now);
        guarded.closed_by = Some(caller.actor.clone());
        guarded.updated_at = now;
        let take = guarded.clone();
        drop(guarded);
        let report = json!({
            "stock_take_id": take.id,
            "title": take.title,
            "status": take.status.as_str(),
            "closed_at": take.closed_at,
            "closed_by": take.closed_by,
            "lines_total": lines.len(),
            "lines_with_discrepancy": discrepancies.len(),
            "total_expected": total_expected,
            "total_counted": total_counted,
            "total_discrepancy": total_discrepancy,
            "discrepancies": discrepancies,
        });
        state.emit(
            "assets.stock_take.closed",
            json!({
                "stock_take_id": take.id.to_string(),
                "actor": caller.actor,
                "lines_total": lines.len(),
                "lines_with_discrepancy": discrepancies.len(),
                "total_discrepancy": total_discrepancy,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_line(expected: i64, counted: Option<i64>) -> AssetStockTakeLine {
        AssetStockTakeLine {
            id: Uuid::new_v4(),
            stock_take_id: Uuid::new_v4(),
            asset_id: Uuid::new_v4(),
            stock_id: Uuid::new_v4(),
            expected_quantity: expected,
            counted_quantity: counted,
            counted_by: None,
            note: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn checked_stock_take_totals_cases() {
        let lines = vec![test_line(10, Some(8)), test_line(5, Some(7))];
        assert_eq!(checked_stock_take_totals(&lines).unwrap(), (15, 15, 0));
        let lines = vec![test_line(10, Some(12))];
        assert_eq!(checked_stock_take_totals(&lines).unwrap(), (10, 12, 2));
        assert_eq!(checked_stock_take_totals(&[]).unwrap(), (0, 0, 0));
    }

    #[test]
    fn checked_stock_take_totals_refuses_overflow_with_409() {
        let lines = vec![test_line(i64::MAX, Some(0)), test_line(1, Some(0))];
        assert_eq!(checked_stock_take_totals(&lines).unwrap_err().code, 409);
        let lines = vec![test_line(0, Some(i64::MAX)), test_line(0, Some(1))];
        assert_eq!(checked_stock_take_totals(&lines).unwrap_err().code, 409);
    }
}
