// Logistics module - shipments, fleet, warehouses, delivery
// routes, delivery scheduling, proof of delivery, delivery
// exceptions, and shipping cost quotes.
//
// Routes live under `/api/v1/logistics`:
//
//   Shipments:
//     GET/POST /shipments - list shipments / create a shipment.
//     GET/PATCH/DELETE /shipments/{id}.
//     POST /shipments/{id}/transition - move the shipment through
//          the status state machine (reason required for terminal
//          and exception branches).
//     GET  /shipments/{id}/timeline - every status transition with
//          actor, timestamp, location, and note.
//     POST /shipments/{id}/link-order - link an existing store
//          order (verified through the `store.order.lookup` hook
//          query; see the order-link note below).
//     POST /shipments/{id}/schedule - set the delivery window.
//     POST /shipments/{id}/reschedule - change the delivery window
//          (audited through the timeline).
//     POST /shipments/{id}/proof - record proof of delivery
//          (storage reference + hash only, never raw bytes).
//     GET  /shipments/{id}/proof - list proofs of delivery.
//     POST /shipments/{id}/exceptions - open a delivery exception.
//     GET  /shipments/{id}/exceptions - list exceptions.
//   Fleet:
//     GET/POST /vehicles, GET/PATCH/DELETE /vehicles/{id}.
//     POST /vehicles/{id}/assign - assign a driver to a vehicle.
//     GET/POST /vehicles/{id}/maintenance - list / create a
//          maintenance reminder (queued through the scheduler).
//   Drivers:
//     GET/POST /drivers, GET/PATCH/DELETE /drivers/{id}.
//   Warehouses:
//     GET/POST /warehouses, GET/PATCH/DELETE /warehouses/{id}.
//     GET/POST /warehouses/{id}/stock-movements - list / record a
//          stock movement (quantities only; the assets / inventory
//          module owns the item master data).
//     GET  /warehouses/{id}/stock-movements/{movement_id}.
//   Delivery routes:
//     GET/POST /routes, GET/PATCH/DELETE /routes/{id}.
//     GET/POST /routes/{id}/stops.
//     PATCH/DELETE /routes/{id}/stops/{stop_id}.
//     POST /routes/{id}/assign - assign a driver + vehicle.
//     POST /routes/{id}/reorder - nearest-neighbour stop reorder
//          (heuristic only; see the route note below).
//   Exceptions:
//     POST /exceptions/{exception_id}/resolve.
//     POST /exceptions/{exception_id}/return.
//   Pricing:
//     GET/POST /rate-tables, GET/PATCH/DELETE /rate-tables/{id}.
//     POST /quote - shipping cost quote from a rate table.
//   Public tracking (no auth; see the public-access note below):
//     GET  /track/{tracking_number}?token=<verification token>.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory shipment, timeline, vehicle, driver, warehouse, stock
// movement, route, stop, proof, exception, and rate-table
// management; the shipment status state machine with illegal
// transitions rejected; tracking numbers with a check digit;
// delivery windows with audited rescheduling; proof of delivery
// as storage references plus a content hash; exception workflow
// with customer notification; shipping cost quotes from
// configurable rate tables; and a token-gated, rate-limited public
// tracking lookup that exposes status and location only. What is
// NOT built here: real route optimisation (only a
// nearest-neighbour reorder heuristic - see the route note); live
// carrier APIs or label printing; actual distance measurement
// (distance comes from the caller or from the Calculator module
// through the `calculator.distance_requested` hook query - see the
// distance note); payment collection or refunds (the store /
// billing modules own those); and actual email / push delivery
// (the notifications module owns delivery - this module only
// emits `notification.requested`).
//
// Tracking number format: `LG-` followed by 8 decimal digits
// followed by `-` and one Luhn check digit computed over the 8
// digits, for example `LG-48291736-5`. The 8 digits are drawn at
// random; a collision with an existing tracking number regenerates
// them. The check digit lets a mistyped number be rejected
// without a database lookup, but it is not a secret: the public
// tracking lookup is protected by the verification token, not by
// the tracking number.
//
// Order-link note: a shipment may link a store order id, but this
// module never duplicates order rows and never reads the store's
// tables directly. Linking asks the store module through the
// `store.order.lookup` hook query with `{"order_id": "..."}` and
// expects a result carrying `{"found": true}` (or `false`). When
// the store answers `found: false` the link is rejected with
// 422. When no handler answers at all - today the store module
// has no `store.order.lookup` answerer - the shipment is still
// created but the link is recorded as `unverified`
// (`order_link_status: "unverified"`) and the response carries a
// warning; the shipment is NOT treated as linked to a real order
// until the store confirms it. The follow-up is for the store
// module to answer `store.order.lookup`.
//
// Distance note: a quote needs a distance in kilometres. The
// caller may supply `distance_km` directly. When it is absent and
// both endpoints carry coordinates, this module emits
// `calculator.distance_requested` with
// `{"origin_latitude", "origin_longitude", "destination_latitude",
// "destination_longitude"}` and expects a result carrying
// `{"distance_km": <number>, "method": "<name>"}`. No handler
// answers that event today, so an unanswered query fails the
// quote with 422 asking for `distance_km`; no distance is ever
// guessed. The follow-up is for the Calculator (or geo) module to
// answer `calculator.distance_requested`. This module performs no
// distance or pricing math of its own beyond applying the rate
// table to a billable weight and a distance it was given.
//
// Route note: route optimisation is out of scope. The reorder
// endpoint applies a nearest-neighbour heuristic starting from
// the route start point (or the first stop when no start point is
// set): repeatedly pick the closest unvisited stop by straight-line
// distance. It is a convenience ordering, not an optimiser - it
// ignores roads, traffic, time windows, and vehicle capacity -
// and it refuses (422) when any stop lacks coordinates instead of
// silently ordering on incomplete data.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`). The public tracking route below relies
// on that gate: an administrator must create the matching
// `ApiAccessRule` row for `GET /api/v1/logistics/track/*`.
// Do NOT treat the absence of a capability check on the public
// route as an open door: without the rule the API Manager denies
// the call. On top of the gate, the lookup requires the
// per-shipment verification token (issued once at creation, stored
// only as a SHA-256 hash), is rate-limited to 30 lookups per
// tracking number per client address per 10 minutes (429 beyond
// that), and returns status and location only - never the
// recipient name, addresses, phone data, cost, or order id.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `logistics.shipment.created`, `logistics.shipment.updated`,
//     `logistics.shipment.deleted`, `logistics.shipment.cancelled`,
//     `logistics.shipment.delivered`, `logistics.shipment.returned`,
//     `logistics.shipment.status_changed`
//   - `logistics.shipment.timeline_added`
//   - `logistics.vehicle.created`, `logistics.vehicle.updated`,
//     `logistics.vehicle.deleted`, `logistics.vehicle.assigned`,
//     `logistics.vehicle.maintenance_created`,
//     `logistics.vehicle.maintenance_due`
//   - `logistics.driver.created`, `logistics.driver.updated`,
//     `logistics.driver.deleted`
//   - `logistics.warehouse.created`, `logistics.warehouse.updated`,
//     `logistics.warehouse.deleted`
//   - `logistics.stock.moved`, `logistics.stock.low`
//   - `logistics.route.created`, `logistics.route.updated`,
//     `logistics.route.deleted`, `logistics.route.assigned`,
//     `logistics.route.reordered`
//   - `logistics.route_stop.created`, `logistics.route_stop.updated`,
//     `logistics.route_stop.deleted`
//   - `logistics.proof.created`
//   - `logistics.exception.created`, `logistics.exception.resolved`,
//     `logistics.exception.returned`
//   - `logistics.rate_table.created`, `logistics.rate_table.updated`,
//     `logistics.rate_table.deleted`
//   - `logistics.quote.created`
//   - `notification.requested` (delivery exceptions, low-stock
//     alerts, maintenance reminders, delivery completion) with the
//     notifications module's payload shape: `{"kind", "user_id",
//     "title", "body", "module": "logistics", "link", "priority",
//     "channels"}`.
//   - `scheduler.enqueue` with `{"kind":
//     "logistics.maintenance.remind", "payload": {"vehicle_id",
//     "reminder_id", ...}}` when a maintenance reminder is created.
//   - `store.order.lookup` (query, not an emit-and-forget event)
//     with `{"order_id": "..."}` when linking an order.
//   - `calculator.distance_requested` (query) with
//     `{"origin_latitude", "origin_longitude",
//     "destination_latitude", "destination_longitude"}` when a
//     quote needs a distance.
// Events this module listens to:
//   - `order.paid` - records the paid order id so a linked
//     shipment can be matched by the caller; no shipment is
//     auto-created (a paid order may be for digital goods).
//   - `store.order.cancelled` - cancels linked shipments still in
//     `created` status with reason "linked order cancelled" and
//     notifies the shipment creator.
//   - `assets.stock.low` - when the payload names a warehouse this
//     module knows (`warehouse_id`), notifies that warehouse's
//     manager and emits `logistics.stock.low`.
//   - `logistics.maintenance.remind_requested` - the event the
//     scheduler emits when a `logistics.maintenance.remind` job
//     fires, following its naming rule (`backup.run` ->
//     `backup.run_requested`); marks the reminder due and notifies
//     the assigned driver.
//
// Scheduler note: `logistics.maintenance.remind` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the reminder records `scheduler_status: "declined"` instead of
// pretending it was scheduled. The reminder is still stored and
// stays usable (it can be checked manually through the vehicle
// maintenance list); the follow-up is to add
// `"logistics.maintenance.remind"` to `KNOWN_KINDS` in
// `scheduler.rs` and arm the dispatch so the job emits
// `logistics.maintenance.remind_requested` with the reminder
// payload (see the integrator snippet in the module report).

//! Logistics module - shipments, fleet, warehouses, delivery - Module.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::hooks::*;
use super::rates::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;
use super::ai_capabilities::AI_CAPABILITIES;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

pub struct LogisticsModule {
    pub(super) state: LogisticsState,
}

impl LogisticsModule {
    pub fn new() -> Self {
        Self {
            state: LogisticsState::default(),
        }
    }
}

impl Default for LogisticsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LogisticsModule {
    fn id(&self) -> &'static str {
        "logistics"
    }

    fn name(&self) -> &'static str {
        "Logistics"
    }

    fn description(&self) -> &'static str {
        "Shipments with a checked state machine and timeline, fleet vehicles and drivers, warehouses with stock movements and low-stock alerts, delivery routes with a nearest-neighbour reorder heuristic, delivery scheduling, proof of delivery, delivery exceptions, rate-table shipping quotes, and token-gated public tracking."
    }

    fn requires(&self) -> &[&'static str] {
        // Locations are the shared maps module's job: shipments,
        // routes, vehicles, drivers, and warehouses all carry
        // coordinates, and this module consumes the maps contract
        // for pins, nearby search, and geofences instead of
        // keeping its own geospatial store. Maps is core (always
        // active), so this only documents the dependency and
        // blocks disabling maps while logistics is enabled.
        &[
            "users", "roles", "store", "notifications", "scheduler", "assets", "maps",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users", "roles", "store", "notifications", "scheduler", "assets", "maps",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("logistics", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            ORDER_PAID,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| on_order_paid(Arc::clone(&state), ctx)),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_CANCELLED,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_cancelled(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            ASSETS_STOCK_LOW,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| {
                on_assets_stock_low(Arc::clone(&state), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            MAINTENANCE_REMIND_REQUESTED,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| {
                on_maintenance_remind_requested(state.clone().into(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/logistics")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("/track/{tracking_number}", web::get().to(track_shipment))
                .route("/quote", web::post().to(quote_shipping))
                .route("/shipments", web::get().to(list_shipments))
                .route("/shipments", web::post().to(create_shipment))
                .route("/shipments/{id}", web::get().to(get_shipment))
                .route("/shipments/{id}", web::patch().to(update_shipment))
                .route("/shipments/{id}", web::delete().to(delete_shipment))
                .route("/shipments/{id}/transition", web::post().to(transition_shipment))
                .route("/shipments/{id}/timeline", web::get().to(list_shipment_timeline))
                .route("/shipments/{id}/link-order", web::post().to(link_order))
                .route("/shipments/{id}/schedule", web::post().to(schedule_shipment))
                .route("/shipments/{id}/reschedule", web::post().to(reschedule_shipment))
                .route("/shipments/{id}/proof", web::post().to(create_proof))
                .route("/shipments/{id}/proof", web::get().to(list_proofs))
                .route("/shipments/{id}/exceptions", web::post().to(create_exception))
                .route("/shipments/{id}/exceptions", web::get().to(list_exceptions))
                .route("/exceptions/{exception_id}/resolve", web::post().to(resolve_exception))
                .route("/exceptions/{exception_id}/return", web::post().to(return_exception))
                .route("/vehicles", web::get().to(list_vehicles))
                .route("/vehicles", web::post().to(create_vehicle))
                .route("/vehicles/{id}", web::get().to(get_vehicle))
                .route("/vehicles/{id}", web::patch().to(update_vehicle))
                .route("/vehicles/{id}", web::delete().to(delete_vehicle))
                .route("/vehicles/{id}/assign", web::post().to(assign_vehicle))
                .route("/vehicles/{id}/maintenance", web::get().to(list_maintenance))
                .route("/vehicles/{id}/maintenance", web::post().to(create_maintenance))
                .route("/drivers", web::get().to(list_drivers))
                .route("/drivers", web::post().to(create_driver))
                .route("/drivers/{id}", web::get().to(get_driver))
                .route("/drivers/{id}", web::patch().to(update_driver))
                .route("/drivers/{id}", web::delete().to(delete_driver))
                .route("/warehouses", web::get().to(list_warehouses))
                .route("/warehouses", web::post().to(create_warehouse))
                .route("/warehouses/{id}", web::get().to(get_warehouse))
                .route("/warehouses/{id}", web::patch().to(update_warehouse))
                .route("/warehouses/{id}", web::delete().to(delete_warehouse))
                .route(
                    "/warehouses/{id}/stock-movements",
                    web::get().to(list_stock_movements),
                )
                .route(
                    "/warehouses/{id}/stock-movements",
                    web::post().to(create_stock_movement),
                )
                .route(
                    "/warehouses/{id}/stock-movements/{movement_id}",
                    web::get().to(get_stock_movement),
                )
                .route("/routes", web::get().to(list_routes))
                .route("/routes", web::post().to(create_route))
                .route("/routes/{id}", web::get().to(get_route))
                .route("/routes/{id}", web::patch().to(update_route))
                .route("/routes/{id}", web::delete().to(delete_route))
                .route("/routes/{id}/assign", web::post().to(assign_route))
                .route("/routes/{id}/stops", web::get().to(list_route_stops))
                .route("/routes/{id}/stops", web::post().to(create_route_stop))
                .route("/routes/{id}/stops/{stop_id}", web::patch().to(update_route_stop))
                .route("/routes/{id}/stops/{stop_id}", web::delete().to(delete_route_stop))
                .route("/routes/{id}/reorder", web::post().to(reorder_route))
                .route("/rate-tables", web::get().to(list_rate_tables))
                .route("/rate-tables", web::post().to(create_rate_table))
                .route("/rate-tables/{id}", web::get().to(get_rate_table))
                .route("/rate-tables/{id}", web::patch().to(update_rate_table))
                .route("/rate-tables/{id}", web::delete().to(delete_rate_table)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/logistics/track/{tracking_number}", owner: "logistics", description: "PUBLIC: track a shipment by tracking number + verification token (?token=...); rate-limited, status and location only (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/logistics/quote", owner: "logistics", description: "Shipping cost quote from a rate table (billable weight = max(actual, dimensional); distance from caller or calculator.distance_requested).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments", owner: "logistics", description: "List shipments (paginated, filter by status, warehouse_id, driver_id, vehicle_id, order_id, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments", owner: "logistics", description: "Create a shipment (tracking number + one-time verification token returned; order link verified via store.order.lookup, unverified when unanswered).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}", owner: "logistics", description: "Get one shipment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/shipments/{id}", owner: "logistics", description: "Update a shipment in created status only.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/shipments/{id}", owner: "logistics", description: "Delete a shipment in created status only.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/transition", owner: "logistics", description: "Move a shipment through the state machine (illegal transitions rejected with 409; delivered/exception go through proof/exception endpoints).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}/timeline", owner: "logistics", description: "List the shipment status timeline (actor, timestamp, location, note).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/link-order", owner: "logistics", description: "Link a store order to a shipment (verified via store.order.lookup; rejected when the store denies it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/schedule", owner: "logistics", description: "Set the delivery window of a shipment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/reschedule", owner: "logistics", description: "Reschedule a delivery (reason required; audited in the timeline; customer notified).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/proof", owner: "logistics", description: "Record proof of delivery (storage reference + SHA-256 hash; moves the shipment to delivered).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}/proof", owner: "logistics", description: "List proofs of delivery of a shipment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/exceptions", owner: "logistics", description: "Open a delivery exception (moves the shipment to exception; customer notified).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}/exceptions", owner: "logistics", description: "List exceptions of a shipment (filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/exceptions/{exception_id}/resolve", owner: "logistics", description: "Resolve an exception (shipment resumes to in_transit, or returns when resume_delivery is false).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/exceptions/{exception_id}/return", owner: "logistics", description: "Return a shipment from an open exception to the sender.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/vehicles", owner: "logistics", description: "List vehicles (paginated, filter by status, vehicle_type, assigned_driver_id, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/vehicles", owner: "logistics", description: "Create a vehicle (plate must be unique).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/vehicles/{id}", owner: "logistics", description: "Get one vehicle.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/vehicles/{id}", owner: "logistics", description: "Update a vehicle.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/vehicles/{id}", owner: "logistics", description: "Delete a vehicle with no live shipments.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/vehicles/{id}/assign", owner: "logistics", description: "Assign an active driver to a vehicle (one driver, one vehicle).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/vehicles/{id}/maintenance", owner: "logistics", description: "List maintenance reminders of a vehicle.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/vehicles/{id}/maintenance", owner: "logistics", description: "Create a maintenance reminder (queued as logistics.maintenance.remind; scheduler_status declined until the kind is known).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/drivers", owner: "logistics", description: "List drivers (paginated, filter by status, user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/drivers", owner: "logistics", description: "Create a driver (links a user id; never duplicates user rows).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/drivers/{id}", owner: "logistics", description: "Get one driver.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/drivers/{id}", owner: "logistics", description: "Update a driver.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/drivers/{id}", owner: "logistics", description: "Delete a driver with no live shipments (clears vehicle assignments).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses", owner: "logistics", description: "List warehouses (paginated, filter by company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/warehouses", owner: "logistics", description: "Create a warehouse.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses/{id}", owner: "logistics", description: "Get one warehouse.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/warehouses/{id}", owner: "logistics", description: "Update a warehouse.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/warehouses/{id}", owner: "logistics", description: "Delete a warehouse with no stock movements.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses/{id}/stock-movements", owner: "logistics", description: "List stock movements of a warehouse (filter by direction, asset_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/warehouses/{id}/stock-movements", owner: "logistics", description: "Record a stock movement in/out (quantities only; out cannot exceed on-hand; low-stock alert at/below threshold).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses/{id}/stock-movements/{movement_id}", owner: "logistics", description: "Get one stock movement.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/routes", owner: "logistics", description: "List delivery routes (paginated, filter by status, driver_id, vehicle_id, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes", owner: "logistics", description: "Create a delivery route.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/routes/{id}", owner: "logistics", description: "Get one delivery route.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/routes/{id}", owner: "logistics", description: "Update a delivery route (not when completed or cancelled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/routes/{id}", owner: "logistics", description: "Delete a delivery route (not when active; stops are removed with it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes/{id}/assign", owner: "logistics", description: "Assign a driver + vehicle to a delivery route.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/routes/{id}/stops", owner: "logistics", description: "List stops of a delivery route in visit order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes/{id}/stops", owner: "logistics", description: "Add a stop to a delivery route (appended at the end).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/routes/{id}/stops/{stop_id}", owner: "logistics", description: "Update a route stop (address, coordinates, status, note).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/routes/{id}/stops/{stop_id}", owner: "logistics", description: "Delete a route stop (remaining stops are renumbered).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes/{id}/reorder", owner: "logistics", description: "Reorder stops by nearest-neighbour heuristic (requires coordinates on every stop; not a real optimiser).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/rate-tables", owner: "logistics", description: "List shipping rate tables (filter by origin_zone, destination_zone, active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logistics/rate-tables", owner: "logistics", description: "Create a shipping rate table.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logistics/rate-tables/{id}", owner: "logistics", description: "Get one rate table.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/rate-tables/{id}", owner: "logistics", description: "Update a rate table.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/rate-tables/{id}", owner: "logistics", description: "Delete a rate table.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LogisticsModule::new()));
}
