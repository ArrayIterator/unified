//! Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;
// Asset & Inventory module - hardware / software inventory,
// assignment to users, warranty and licence expiry alerts,
// depreciation through the Calculator module, consumable stock
// levels, audited stock adjustments, and stock-take audits.
//
// Routes live under `/api/v1/assets`:
//
//   Assets:
//     GET/POST / - list assets / create an asset.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/assign - assign to a user (append-only history).
//     POST /{id}/return - return an assigned asset.
//     POST /{id}/retire - retire an asset.
//     POST /{id}/dispose - dispose of an asset.
//     POST /{id}/send-for-repair, /{id}/return-from-repair.
//     GET  /{id}/assignments - assignment history (never overwritten).
//     GET  /{id}/depreciation - depreciation schedule / status.
//     POST /{id}/depreciation/compute - ask the Calculator module
//          for a depreciation schedule (see the honesty note).
//   Warranty / licence expiry:
//     GET  /warranty/expiring?days=30 - assets whose warranty or
//          software licence ends within the window.
//     POST /warranty/check - run the expiry check now (alerts via
//          `notification.requested`, job via `scheduler.enqueue`).
//   Consumable stock:
//     GET/POST /stock - list stock items / create a stock item.
//     GET/PATCH /stock/{stock_id}.
//     POST /stock/{stock_id}/adjust - audited quantity change
//          (reason required; low-stock alert on crossing).
//     GET  /stock/{stock_id}/adjustments - adjustment history.
//   Stock takes:
//     GET/POST /stock-takes - list / start a stock-take.
//     GET  /stock-takes/{id}.
//     POST /stock-takes/{id}/lines - add a count line.
//     PATCH /stock-takes/{id}/lines/{line_id} - record the count.
//     POST /stock-takes/{id}/close - close with a discrepancy report.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory asset, assignment, stock, adjustment, stock-take, and
// stock-take-line management; append-only assignment history (a
// return closes the active assignment row, it never deletes or
// overwrites it); audited stock adjustments (every change keeps its
// reason, the actor, and the quantity after the change); stock-take
// counting with a discrepancy report on close; warranty and licence
// expiry windows computed from stored dates with alerts emitted as
// `notification.requested` to the asset owner and, when assigned,
// the current assignee. What is NOT built here: the depreciation
// math itself - this module never computes a depreciation figure.
// It emits `calculator.depreciation_requested` and stores the
// schedule the Calculator module returns; when the Calculator
// module is not enabled, or no handler answers, the asset records
// `depreciation_status: "pending"` with the reason instead of a
// guessed schedule (see `request_depreciation`). RESOLVED
// (inspector defect A-M1): the Calculator module now answers
// `calculator.depreciation_requested` (see
// `calculator/hooks.rs::on_depreciation_requested`), so create /
// update / compute with a depreciation method stores the schedule
// instead of staying `pending`. Assets still holds no HTTP client
// to the Calculator, so there is no fallback call from here - the
// `pending` fallback above covers the Calculator-disabled case.
// Actual email /
// push delivery belongs to the notifications module (this module
// only emits `notification.requested`); no payment, procurement,
// or vendor order is created here - `vendor` is a plain text
// reference to the vendor record kept by the procurement side,
// and no procurement module is consulted. Warranty alerts are
// checked on demand (`POST /warranty/check`) and through the
// `assets.warranty.check_requested` hook the scheduler fires;
// `assets.warranty.check` IS in the scheduler's `KNOWN_KINDS`
// list (scheduler/types.rs) with a dispatch arm in
// scheduler/service.rs emitting `assets.warranty.check_requested`,
// so the recurring half of POST /warranty/check can be queued
// (inspector defect A-M2 resolved).
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check. Stock data is internal and is never exposed
// through the API Manager public-access rules.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `assets.created`, `assets.updated`, `assets.deleted`,
//     `assets.assigned`, `assets.returned`, `assets.retired`,
//     `assets.disposed`, `assets.repair.started`,
//     `assets.repair.completed`
//   - `assets.depreciation.computed`,
//     `assets.depreciation.pending`
//   - `assets.warranty.expiring`, `assets.license.expiring`
//   - `assets.stock.created`, `assets.stock.updated`,
//     `assets.stock.adjusted`, `assets.stock.low`
//   - `assets.stock_take.started`, `assets.stock_take.closed`
//   - `notification.requested` (warranty / licence expiry alerts
//     and low-stock alerts) with the notifications module's
//     payload shape: `{"kind", "user_id", "title", "body",
//     "module": "assets", "link", "priority", "channels"}`.
//   - `scheduler.enqueue` with `{"kind": "assets.warranty.check",
//     "payload": {"checked_at", ...}}` when a warranty check runs.
//   - `calculator.depreciation_requested` with `{"asset_id",
//     "method", "purchase_price", "salvage_value",
//     "useful_life_years", "purchase_date", "currency"}`; the
//     Calculator module answers with a result carrying a
//     `"schedule"` array (one entry per period with at least
//     `period`, `depreciation`, and `book_value`) or an `"error"`.
// Events this module listens to:
//   - `assets.warranty.check_requested` - the event the scheduler
//     emits when an `assets.warranty.check` job fires, following
//     its naming rule (`backup.run` -> `backup.run_requested`);
//     runs the expiry check and emits `notification.requested`
//     for every asset inside the alert windows.
//   - `manufacturing.stock.requested` - answered with per-product
//     levels (`{"stock": {product_id: {available, reserved,
//     unit}}}`) for the products this module holds a stock row
//     for. Products with no stock row are omitted, never
//     invented: the requester reports them as `unknown`. When
//     no requested product has a row, no answer is given at all,
//     so the requester keeps its `unknown` / `pending_confirmation`
//     behaviour instead of treating an empty answer as a level.
//     `reserved` is always 0 (this module holds no reservations)
//     and `unit` is null (stock rows carry no unit).
//   - `assets.stock.decrement_requested` /
//     `assets.stock.increment_requested` - POS sales and refunds
//     and manufacturing consumption move stock by a positive
//     `quantity`; the change is recorded as an audited adjustment
//     (same shape as the HTTP adjust) and answered with
//     `{"applied", "stock_id", "quantity_after"}`. A decrement
//     that would take stock below zero is refused (`applied:
//     false`, reason `insufficient_stock`) - stock is never
//     driven negative by a hook.
//   - `assets.stock.adjust_requested` - field-service part
//     consumption moves stock by a signed `delta`; same audited
//     recording and answer shape as above. A request naming a
//     stock row that does not exist is answered `applied: false`
//     with reason `stock_not_found`; no stock row is ever created
//     from a hook - creation stays an admin HTTP action.


// Module

/// The Asset & Inventory module.
pub struct AssetsModule {
    state: AssetsState,
}

impl AssetsModule {
    pub fn new() -> Self {
        Self {
            state: AssetsState::default(),
        }
    }
}

impl Default for AssetsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AssetsModule {
    fn id(&self) -> &'static str {
        "assets"
    }

    fn name(&self) -> &'static str {
        "Asset & Inventory"
    }

    fn description(&self) -> &'static str {
        "Hardware and software inventory with assignment history, warranty and licence expiry alerts, depreciation through the Calculator module, consumable stock levels with audited adjustments, and stock-take audits."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler", "calculator"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler", "calculator"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("assets", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            WARRANTY_CHECK_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_warranty_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            MANUFACTURING_STOCK_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_manufacturing_stock_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STOCK_DECREMENT_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_stock_decrement_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STOCK_INCREMENT_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_stock_increment_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STOCK_ADJUST_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_stock_adjust_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/assets")
                .app_data(state)
                .route("", web::get().to(list_assets))
                .route("", web::post().to(create_asset))
                .route("/warranty/expiring", web::get().to(list_expiring))
                .route("/warranty/check", web::post().to(run_warranty_check_handler))
                .route("/stock", web::get().to(list_stock))
                .route("/stock", web::post().to(create_stock))
                .route("/stock/{stock_id}", web::get().to(get_stock))
                .route("/stock/{stock_id}", web::patch().to(update_stock))
                .route("/stock/{stock_id}/adjust", web::post().to(adjust_stock))
                .route(
                    "/stock/{stock_id}/adjustments",
                    web::get().to(list_stock_adjustments),
                )
                .route("/stock-takes", web::get().to(list_stock_takes))
                .route("/stock-takes", web::post().to(create_stock_take))
                .route("/stock-takes/{id}", web::get().to(get_stock_take))
                .route(
                    "/stock-takes/{id}/lines",
                    web::post().to(add_stock_take_line),
                )
                .route(
                    "/stock-takes/{id}/lines/{line_id}",
                    web::patch().to(count_stock_take_line),
                )
                .route("/stock-takes/{id}/close", web::post().to(close_stock_take))
                .route("/{id}", web::get().to(get_asset))
                .route("/{id}", web::patch().to(update_asset))
                .route("/{id}", web::delete().to(delete_asset))
                .route("/{id}/assign", web::post().to(assign_asset))
                .route("/{id}/return", web::post().to(return_asset))
                .route("/{id}/retire", web::post().to(retire_asset))
                .route("/{id}/dispose", web::post().to(dispose_asset))
                .route("/{id}/send-for-repair", web::post().to(send_for_repair))
                .route("/{id}/return-from-repair", web::post().to(return_from_repair))
                .route("/{id}/assignments", web::get().to(list_assignments))
                .route("/{id}/depreciation", web::get().to(get_depreciation))
                .route(
                    "/{id}/depreciation/compute",
                    web::post().to(compute_depreciation),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/assets", owner: "assets", description: "List assets (paginated, filter by status, type, location, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets", owner: "assets", description: "Create an asset (serial number must be unique when given).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/{id}", owner: "assets", description: "Get one asset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/assets/{id}", owner: "assets", description: "Update an asset (retired/disposed assets cannot be edited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/assets/{id}", owner: "assets", description: "Delete an asset with no active assignment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/assign", owner: "assets", description: "Assign an in-stock asset to a user (append-only history).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/return", owner: "assets", description: "Return an assigned asset (closes the active assignment row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/retire", owner: "assets", description: "Retire an asset (terminal for assignment; emits assets.retired).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/dispose", owner: "assets", description: "Dispose of an asset (terminal; emits assets.disposed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/send-for-repair", owner: "assets", description: "Send an asset for repair (in_stock -> in_repair).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/return-from-repair", owner: "assets", description: "Return an asset from repair to in-stock.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/{id}/assignments", owner: "assets", description: "Assignment history of an asset (append-only, paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/{id}/depreciation", owner: "assets", description: "Depreciation status and schedule of an asset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/depreciation/compute", owner: "assets", description: "Ask the Calculator module for a depreciation schedule; records pending when it is not enabled.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/warranty/expiring", owner: "assets", description: "Assets whose warranty or licence ends within ?days (default 30).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/warranty/check", owner: "assets", description: "Run the warranty/licence expiry check now and queue the recurring job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/stock", owner: "assets", description: "List consumable stock items (paginated, filter by asset_id, low_stock=true).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/stock", owner: "assets", description: "Create a stock item for a consumable asset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/stock/{stock_id}", owner: "assets", description: "Get one stock item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/assets/stock/{stock_id}", owner: "assets", description: "Update a stock item's reorder threshold / location.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/stock/{stock_id}/adjust", owner: "assets", description: "Audited stock adjustment (delta + required reason; low-stock alert on crossing).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/stock/{stock_id}/adjustments", owner: "assets", description: "Adjustment history of a stock item (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/stock-takes", owner: "assets", description: "List stock takes (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/stock-takes", owner: "assets", description: "Start a stock take.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/assets/stock-takes/{id}", owner: "assets", description: "Get one stock take with its count lines and discrepancies.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/stock-takes/{id}/lines", owner: "assets", description: "Add a count line (expected quantity is the live on-hand count).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/assets/stock-takes/{id}/lines/{line_id}", owner: "assets", description: "Record the counted quantity of a line (open stock takes only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/assets/stock-takes/{id}/close", owner: "assets", description: "Close a stock take with a discrepancy report.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AssetsModule::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_for_repair_description_matches_guard() {
        // The handler rejects Assigned assets ("return it first"),
        // so the catalog must say in_stock -> in_repair only.
        let routes = AssetsModule::new().route_info();
        let route = routes
            .iter()
            .find(|route| route.path == "/api/v1/assets/{id}/send-for-repair")
            .expect("send-for-repair route must be listed");
        assert_eq!(
            route.description,
            "Send an asset for repair (in_stock -> in_repair)."
        );
    }
}
