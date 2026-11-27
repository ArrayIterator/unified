//! manufacturing - module.

// Manufacturing / MRP module - bills of materials, work orders,
// production runs, work centers, material reservations, cost
// roll-up, and production reports.
//
// Bills of materials (BOMs) are versioned: every edit of the
// component lines or the routing creates a new version and a work
// order pins the version it was created from, so a production run
// always uses the BOM that was approved when the order was placed.
// BOMs are multi-level: a component may itself be a manufactured
// product with its own BOM, and the explosion endpoint resolves
// the full tree with cycle detection and a configurable depth cap.
//
// Work orders move through `draft -> confirmed -> in_progress ->
// done / cancelled`. Transitions are checked against the
// transition map in the module settings (admin-editable); an
// illegal transition is a 409 carrying the list of allowed next
// states.
//
// Interconnection happens only through the hook bus; this module
// never calls another module directly and never duplicates stock
// data:
//
//   - Stock availability is requested with
//     `manufacturing.stock.requested` (`{product_ids}`); an
//     inventory handler answers with per-product
//     `{available, reserved, unit}` levels. When no handler
//     answers, availability is reported as `unknown` and nothing
//     is assumed: reservations are recorded as
//     `pending_confirmation` and no purchase request is emitted.
//   - Reserving components emits
//     `manufacturing.stock.reserve_requested` per component.
//     Releasing emits `manufacturing.stock.release_requested`.
//   - Consuming components emits `manufacturing.components.consumed`
//     per component and `assets.stock.decrement_requested` per
//     line, exactly like the POS stock contract.
//   - Producing finished goods emits
//     `manufacturing.goods.produced` per production run and, on
//     work-order completion, `manufacturing.finished_goods.costed`
//     with the cost roll-up for invoicing.
//   - Short stock emits `manufacturing.purchase.requested`
//     (`{work_order_id, product_id, quantity_short, unit}`) so
//     the procurement side can raise a purchase request.
//   - Completion emits `manufacturing.inspection.requested`
//     (`{work_order_id, target_ref, target_kind, product_id,
//     quantity}` - `target_ref` is the work order id and
//     `target_kind` is `work_order`, the keys the quality side
//     reads) so the quality side can inspect the order; this
//     module listens for the canonical
//     `quality.inspection.completed` and records the outcome on
//     the work order. Before completing, the completion path
//     consults the quality blocks (`quality.blocks.query`, the
//     hook form of quality's `GET /blocks`) and refuses to
//     complete a work order a blocking quality alert covers.
//   - Cost data is requested with `manufacturing.cost.requested`
//     (`{items: [{product_id, quantity, unit}], currency}`);
//     a calculator handler answers with per-product unit costs.
//     Missing cost data is reported as a gap in the response,
//     never fabricated.
//   - This module listens for `manufacturing.stock.confirmed`
//     and `manufacturing.stock.short` and records the
//     confirmations / shortages against the work order.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory BOMs with versions, work centers with capacity and
// scheduling, work orders with checked transitions, reservations,
// production runs, scrap recording against admin-defined reason
// codes, cost roll-up from hook-answered prices plus work-center
// labor rates, and the four reports. What is NOT built here:
//   - Persistence. Durable rows are defined by migration
//     `2026-11-22-000081_manufacturing`; until that migration is
//     executed, records do not survive a restart. Settings,
//     scrap-reason codes, and unit definitions are admin-editable
//     runtime configuration (same pattern as the billing and POS
//     settings); their durable form is a follow-up for the
//     settings store.
//   - Stock application. The stock request / reserve / release
//     events above are a defined contract, but no inventory
//     handler is registered for them in this codebase at the time
//     of writing, so until the inventory side answers them,
//     availability stays `unknown` and stock does not move.
//     Reservations still record what was requested so the
//     inventory side can reconcile them when it lands.
//   - Cost answers. No handler is registered for
//     `manufacturing.cost.requested` at the time of writing, so
//     the cost roll-up reports every component as a gap until
//     the calculator side answers the event. Labor cost is
//     computed from work-center rates stored here.
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check, and no manufacturing route is exposed through
// the API Manager public-access rules (default deny).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the assets and POS modules. Capability
// checks use the `role.check` hook and fail closed (no answer
// means not allowed).

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};
use super::ai_capabilities::AI_CAPABILITIES;



// Module
pub struct ManufacturingModule {
    pub(crate) state: ManufacturingState,
}


impl ManufacturingModule {
    pub fn new() -> Self {
        Self {
            state: ManufacturingState::default(),
        }
    }
}


impl Default for ManufacturingModule {
    fn default() -> Self {
        Self::new()
    }
}


impl Module for ManufacturingModule {
    fn id(&self) -> &'static str {
        "manufacturing"
    }

    fn name(&self) -> &'static str {
        "Manufacturing"
    }

    fn description(&self) -> &'static str {
        "Manufacturing resource planning: versioned multi-level bills of materials, work centers with capacity and scheduling, work orders with checked state transitions, material reservations, production runs with scrap recording, cost roll-up with honest gaps, and production reports. Stock, procurement, quality, and billing are wired through the hook bus only."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["assets", "calculator", "billing"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["assets", "calculator", "billing"]
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

        ai::register_ai_capabilities("manufacturing", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        self.state.seed_definitions();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_stock_confirmed(state.clone(), ctx));
        bus.register(EVENT_STOCK_CONFIRMED, "manufacturing", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_stock_short(state.clone(), ctx));
        bus.register(EVENT_STOCK_SHORT, "manufacturing", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_inspection_completed(state.clone(), ctx));
        bus.register(EVENT_INSPECTION_COMPLETED, "manufacturing", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/manufacturing")
                .app_data(state)
                // Bills of materials.
                .route("/boms", web::get().to(list_boms))
                .route("/boms", web::post().to(create_bom))
                .route("/boms/{id}", web::get().to(get_bom))
                .route("/boms/{id}", web::patch().to(update_bom))
                .route("/boms/{id}", web::delete().to(delete_bom))
                .route("/boms/{id}/versions", web::get().to(list_bom_versions))
                .route("/boms/{id}/versions", web::post().to(create_bom_version))
                .route(
                    "/boms/{id}/versions/{version_id}",
                    web::get().to(get_bom_version),
                )
                .route(
                    "/boms/{id}/versions/{version_id}/activate",
                    web::post().to(activate_bom_version),
                )
                .route("/boms/{id}/explode", web::get().to(explode_bom))
                // Work centers.
                .route("/work-centers", web::get().to(list_work_centers))
                .route("/work-centers", web::post().to(create_work_center))
                .route("/work-centers/{id}", web::get().to(get_work_center))
                .route("/work-centers/{id}", web::patch().to(update_work_center))
                .route("/work-centers/{id}", web::delete().to(delete_work_center))
                .route(
                    "/work-centers/{id}/schedule",
                    web::get().to(get_work_center_schedule),
                )
                // Work orders.
                .route("/work-orders", web::get().to(list_work_orders))
                .route("/work-orders", web::post().to(create_work_order))
                .route("/work-orders/{id}", web::get().to(get_work_order))
                .route("/work-orders/{id}", web::patch().to(update_work_order))
                .route(
                    "/work-orders/{id}/transition",
                    web::post().to(transition_work_order),
                )
                .route(
                    "/work-orders/{id}/check-stock",
                    web::post().to(check_work_order_stock),
                )
                .route(
                    "/work-orders/{id}/reserve",
                    web::post().to(reserve_work_order),
                )
                .route(
                    "/work-orders/{id}/release-reservations",
                    web::post().to(release_work_order_reservations),
                )
                .route(
                    "/work-orders/{id}/consume",
                    web::post().to(consume_work_order),
                )
                .route(
                    "/work-orders/{id}/produce",
                    web::post().to(produce_work_order),
                )
                .route(
                    "/work-orders/{id}/complete",
                    web::post().to(complete_work_order),
                )
                .route("/work-orders/{id}/cost", web::get().to(get_work_order_cost))
                .route(
                    "/work-orders/{id}/reservations",
                    web::get().to(list_work_order_reservations),
                )
                .route(
                    "/work-orders/{id}/runs",
                    web::get().to(list_work_order_runs),
                )
                // Production runs.
                .route("/production-runs", web::get().to(list_production_runs))
                .route("/production-runs/{id}", web::get().to(get_production_run))
                // Reservations.
                .route("/reservations", web::get().to(list_reservations))
                // Scrap reasons (admin definitions).
                .route("/scrap-reasons", web::get().to(list_scrap_reasons))
                .route("/scrap-reasons", web::post().to(create_scrap_reason))
                .route(
                    "/scrap-reasons/{id}",
                    web::patch().to(update_scrap_reason),
                )
                .route(
                    "/scrap-reasons/{id}",
                    web::delete().to(delete_scrap_reason),
                )
                // Units of measure (admin definitions).
                .route("/units", web::get().to(list_units))
                .route("/units", web::post().to(create_unit))
                .route("/units/{id}", web::delete().to(delete_unit))
                // Settings.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                // Reports.
                .route(
                    "/reports/work-orders-by-status",
                    web::get().to(report_work_orders_by_status),
                )
                .route(
                    "/reports/production-output",
                    web::get().to(report_production_output),
                )
                .route(
                    "/reports/component-usage",
                    web::get().to(report_component_usage),
                )
                .route("/reports/scrap-rate", web::get().to(report_scrap_rate)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms", owner: "manufacturing", description: "List bills of materials (paginated, filter by status, product_id, search by name/code).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/boms", owner: "manufacturing", description: "Create a BOM with component lines and routing; creates version 1.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}", owner: "manufacturing", description: "Get one BOM with its active version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/boms/{id}", owner: "manufacturing", description: "Update a BOM; new lines or routing create a new version and activate it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/boms/{id}", owner: "manufacturing", description: "Delete a BOM with no work orders.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}/versions", owner: "manufacturing", description: "List the versions of a BOM (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/boms/{id}/versions", owner: "manufacturing", description: "Create a new BOM version from lines and routing.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}/versions/{version_id}", owner: "manufacturing", description: "Get one BOM version with its lines and routing.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/boms/{id}/versions/{version_id}/activate", owner: "manufacturing", description: "Make a version the active version for new work orders.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}/explode", owner: "manufacturing", description: "Multi-level BOM explosion for a quantity, with cycle detection and a configurable depth cap.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-centers", owner: "manufacturing", description: "List work centers (paginated, filter by status, search by name/code).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-centers", owner: "manufacturing", description: "Create a work center with capacity and optional labor rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-centers/{id}", owner: "manufacturing", description: "Get one work center.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/work-centers/{id}", owner: "manufacturing", description: "Update a work center (capacity, labor rate, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/work-centers/{id}", owner: "manufacturing", description: "Delete a work center not referenced by any BOM routing.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-centers/{id}/schedule", owner: "manufacturing", description: "Daily scheduled load versus capacity for a work center over a date range (from/to filters).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders", owner: "manufacturing", description: "List work orders (paginated, filter by status, product_id, bom_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders", owner: "manufacturing", description: "Create a work order from a BOM and quantity; pins the active (or chosen) BOM version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}", owner: "manufacturing", description: "Get one work order with its component requirements.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/work-orders/{id}", owner: "manufacturing", description: "Update a draft work order (quantity, planned dates, notes); recomputes requirements.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/transition", owner: "manufacturing", description: "Move a work order to a new status; illegal transitions are 409 with the allowed list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/check-stock", owner: "manufacturing", description: "Check component availability through the stock hook; shortfalls emit purchase requests.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/reserve", owner: "manufacturing", description: "Reserve components for a work order; emits stock reserve requests per component.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/release-reservations", owner: "manufacturing", description: "Release a work order's reservations; emits stock release requests.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/consume", owner: "manufacturing", description: "Record component consumption on an in-progress work order; emits components.consumed and stock decrement requests.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/produce", owner: "manufacturing", description: "Record a production run (produced and scrapped quantities, scrap reason required when scrapping); emits goods.produced.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/complete", owner: "manufacturing", description: "Complete a work order; emits inspection, finished-goods cost, and status events.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}/cost", owner: "manufacturing", description: "Cost roll-up for a work order; missing prices are reported as gaps, never fabricated.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}/reservations", owner: "manufacturing", description: "List a work order's material reservations (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}/runs", owner: "manufacturing", description: "List a work order's production runs (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/production-runs", owner: "manufacturing", description: "List production runs (paginated, filter by work_order_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/production-runs/{id}", owner: "manufacturing", description: "Get one production run.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reservations", owner: "manufacturing", description: "List material reservations (paginated, filter by work_order_id, component_product_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/scrap-reasons", owner: "manufacturing", description: "List the admin-defined scrap reason codes (paginated, filter by active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/scrap-reasons", owner: "manufacturing", description: "Add a scrap reason code (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/scrap-reasons/{id}", owner: "manufacturing", description: "Update a scrap reason code (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/scrap-reasons/{id}", owner: "manufacturing", description: "Delete a scrap reason code not used by any production run (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/units", owner: "manufacturing", description: "List the admin-defined units of measure (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/units", owner: "manufacturing", description: "Add a unit of measure (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/units/{id}", owner: "manufacturing", description: "Delete a unit of measure not used by any BOM line (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/settings", owner: "manufacturing", description: "Get the manufacturing settings, including the work-order transition map.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/manufacturing/settings", owner: "manufacturing", description: "Update the manufacturing settings (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/work-orders-by-status", owner: "manufacturing", description: "Report: work-order counts and planned quantities grouped by status.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/production-output", owner: "manufacturing", description: "Report: produced and scrapped quantities per period (from/to filters, group_by day/week/month).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/component-usage", owner: "manufacturing", description: "Report: consumed component quantities per product over a period (from/to filters).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/scrap-rate", owner: "manufacturing", description: "Report: scrap rate overall and per product over a period (from/to filters).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ManufacturingModule::new()));
}
