//! Warranty and licence expiry checks.

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

/// One expiry alert window hit, for the warranty check report.
#[derive(Debug, Clone, Serialize)]
struct ExpiryAlert {
    asset_id: Uuid,
    asset_name: String,
    kind: String,
    expires_at: String,
    days_until: i64,
    window: String,
}

/// Run the warranty / licence expiry check: every asset whose
/// warranty (or, for software, licence) ends within the 30/14/7
/// day windows — or has already expired — gets a
/// `notification.requested` to its owner (and its current assignee
/// when assigned) plus an `assets.warranty.expiring` /
/// `assets.license.expiring` event. Returns the alerts raised.
/// The scheduler job is queued separately by the caller, because
/// the check also runs from the `assets.warranty.check_requested`
/// hook where no scheduler round-trip is needed.
pub(crate) fn run_expiry_check(state: &AssetsState) -> Vec<ExpiryAlert> {
    let today = Utc::now().date_naive();
    let mut alerts: Vec<ExpiryAlert> = Vec::new();
    for entry in state.assets.iter() {
        let asset = entry.value().clone();
        let mut targets: Vec<(String, NaiveDate, &str)> = Vec::new();
        if let Some(warranty_end) = asset.warranty_end {
            targets.push(("warranty".to_string(), warranty_end, "assets.warranty.expiring"));
        }
        if asset.asset_type == AssetType::Software {
            if let Some(license_end) = asset.license_expires_at {
                targets.push(("license".to_string(), license_end, "assets.license.expiring"));
            }
        }
        for (kind, expires_at, event_name) in targets {
            let days_until = (expires_at - today).num_days();
            if days_until > ALERT_WINDOWS_DAYS[0] {
                continue;
            }
            let window = if days_until < 0 {
                "expired".to_string()
            } else if days_until <= ALERT_WINDOWS_DAYS[2] {
                "7-day".to_string()
            } else if days_until <= ALERT_WINDOWS_DAYS[1] {
                "14-day".to_string()
            } else {
                "30-day".to_string()
            };
            let title = if days_until < 0 {
                format!("{} {} has expired", asset.name, kind)
            } else if days_until == 0 {
                format!("{} {} expires today", asset.name, kind)
            } else {
                format!(
                    "{} {} expires in {} days",
                    asset.name, kind, days_until
                )
            };
            let body = format!(
                "Asset \"{}\" ({}): {} ends on {}. Window: {}.",
                asset.name,
                asset.id,
                kind,
                expires_at,
                window
            );
            let link = format!("/api/v1/assets/{}", asset.id);
            state.notify(&format!("assets.{kind}.expiring"), &asset.created_by, &title, &body, Some(link.clone()));
            if let Some(assignment) = active_assignment(state, asset.id) {
                if assignment.user_id != asset.created_by {
                    state.notify(
                        &format!("assets.{kind}.expiring"),
                        &assignment.user_id,
                        &title,
                        &body,
                        Some(link),
                    );
                }
            }
            state.emit(
                event_name,
                json!({
                    "asset_id": asset.id.to_string(),
                    "kind": kind,
                    "expires_at": expires_at.to_string(),
                    "days_until": days_until,
                    "window": window,
                }),
            );
            alerts.push(ExpiryAlert {
                asset_id: asset.id,
                asset_name: asset.name.clone(),
                kind,
                expires_at: expires_at.to_string(),
                days_until,
                window,
            });
        }
    }
    alerts.sort_by(|a, b| a.days_until.cmp(&b.days_until));
    alerts
}

/// Queue the recurring warranty check through `scheduler.enqueue`.
/// Mirrors the events module's `schedule_reminder`: the scheduler
/// answers `{"queued": true, "job_id": ...}` when it knows the
/// kind, and `{"queued": false, "error": "Unknown job kind: ..."}`
/// today, because `assets.warranty.check` is not in `KNOWN_KINDS`
/// yet. The check itself still runs; only the recurring queueing
/// is recorded honestly as `queued` / `declined`.
///
/// BLOCKED FOR SCHEDULER WORK (A-M2): the kind can only be added
/// by the scheduler module, and no existing scheduler kind can
/// stand in for a warranty check. See the module root doc in
/// `module.rs`.
pub(crate) fn queue_warranty_check(state: &AssetsState) -> (bool, Option<String>) {
    let Some(bus) = state.bus() else {
        return (false, None);
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "assets.warranty.check",
            "payload": { "checked_at": Utc::now().to_rfc3339() },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => (
            true,
            result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        ),
        _ => (false, None),
    }
}

/// Emit a low-stock alert when a stock item sits at or below its
/// reorder threshold. Called after every adjustment and stock
/// creation; the caller decides whether the crossing is new.
pub(crate) fn check_low_stock(state: &AssetsState, stock: &AssetStock) {
    if stock.quantity_on_hand > stock.reorder_threshold {
        return;
    }
    let Some(asset) = state.assets.get(&stock.asset_id).map(|entry| entry.value().clone()) else {
        return;
    };
    let title = format!("Low stock: {}", asset.name);
    let body = format!(
        "Asset \"{}\" ({}): {} on hand, reorder threshold is {}.",
        asset.name, asset.id, stock.quantity_on_hand, stock.reorder_threshold
    );
    let link = format!("/api/v1/assets/stock/{}", stock.id);
    state.notify("assets.stock.low", &asset.created_by, &title, &body, Some(link));
    state.emit(
        "assets.stock.low",
        json!({
            "asset_id": asset.id.to_string(),
            "stock_id": stock.id.to_string(),
            "quantity_on_hand": stock.quantity_on_hand,
            "reorder_threshold": stock.reorder_threshold,
        }),
    );
}

pub(crate) fn on_warranty_check_requested(state: AssetsState, ctx: &mut HookContext) {
    let alerts = run_expiry_check(&state);
    ctx.add_result(json!({
        "checked": true,
        "alerts": alerts.len(),
        "alerts_detail": alerts,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_warranty_check_without_bus_is_declined() {
        // No hook bus is attached to this state, so there is no
        // scheduler to answer `scheduler.enqueue` (A-M2, blocked
        // for scheduler work): the queueing is recorded honestly
        // as declined instead of pretending the job was queued.
        let state = AssetsState::default();
        assert_eq!(queue_warranty_check(&state), (false, None));
    }
}
