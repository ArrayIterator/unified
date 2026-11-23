//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::ai_capability;
use super::handlers_items::{add_item, delete_item, get_item, list_items, update_item};
use super::handlers_notes::{
    advance_note, cancel_note, create_note, create_note_from_shipment, delete_note, deliver_note,
    get_note, issue_note, list_notes, list_notes_for_shipment, return_note, update_note,
};
use super::handlers_print::{preview_note, print_note};
use super::handlers_settings::{get_settings, update_settings};
use super::hooks::{
    ai_readiness_event, ai_summarize_event, on_ai_readiness_requested, on_ai_summarize_requested,
    on_proof_created, on_rental_order_returned, on_shipment_cancelled, on_shipment_delivered,
    on_shipment_returned, on_shipment_status_changed, on_user_deleted,
};
use super::state::DeliveryNoteState;
use super::types::{
    EVENT_PROOF_CREATED, EVENT_RENTAL_ORDER_RETURNED, EVENT_SHIPMENT_CANCELLED,
    EVENT_SHIPMENT_DELIVERED, EVENT_SHIPMENT_RETURNED, EVENT_SHIPMENT_STATUS_CHANGED,
    EVENT_USER_DELETED,
};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct DeliveryNotesModule {
    pub(crate) state: DeliveryNoteState,
}

impl DeliveryNotesModule {
    pub fn new() -> Self {
        Self {
            state: DeliveryNoteState::default(),
        }
    }
}

impl Default for DeliveryNotesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DeliveryNotesModule {
    fn id(&self) -> &'static str {
        "delivery_notes"
    }

    fn name(&self) -> &'static str {
        "Delivery Notes"
    }

    fn description(&self) -> &'static str {
        "Delivery notes: DO documents generated from logistics shipments with the shipment's items, sender, receiver, and addresses snapshotted at creation time, data-driven DO numbering (prefix, width, reset rule per tenant), a pure status machine (draft, issued, in transit, delivered, returned, cancelled) with proof-of-delivery gating on delivery, partial and full returns with per-item returned quantities and rental-order linkage, and print-ready documents queued through the printing module."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "logistics", "printing"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "logistics", "printing"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("delivery_notes", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SHIPMENT_STATUS_CHANGED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_shipment_status_changed(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SHIPMENT_DELIVERED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_shipment_delivered(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PROOF_CREATED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_proof_created(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SHIPMENT_RETURNED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_shipment_returned(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SHIPMENT_CANCELLED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_shipment_cancelled(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_RENTAL_ORDER_RETURNED,
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_rental_order_returned(state.clone(), ctx)),
        );
        // The AI module's runner emits these grounding events for
        // the summary and readiness capabilities; this module
        // answers with the DO's stored facts.
        let state = self.state.clone();
        bus.register(
            ai_summarize_event(),
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_ai_summarize_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_readiness_event(),
            "delivery_notes",
            Arc::new(move |ctx: &mut HookContext| on_ai_readiness_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/delivery-notes")
                .app_data(state)
                .route("", web::get().to(list_notes))
                .route("", web::post().to(create_note))
                .route(
                    "/by-shipment/{shipment_id}",
                    web::get().to(list_notes_for_shipment),
                )
                .route(
                    "/from-shipment/{shipment_id}",
                    web::post().to(create_note_from_shipment),
                )
                .route("/{note_id}", web::get().to(get_note))
                .route("/{note_id}", web::patch().to(update_note))
                .route("/{note_id}", web::delete().to(delete_note))
                .route("/{note_id}/issue", web::post().to(issue_note))
                .route("/{note_id}/advance", web::post().to(advance_note))
                .route("/{note_id}/deliver", web::post().to(deliver_note))
                .route("/{note_id}/return", web::post().to(return_note))
                .route("/{note_id}/cancel", web::post().to(cancel_note))
                .route("/{note_id}/print", web::post().to(print_note))
                .route("/{note_id}/print-preview", web::get().to(preview_note))
                .route("/{note_id}/items", web::get().to(list_items))
                .route("/{note_id}/items", web::post().to(add_item))
                .route("/{note_id}/items/{item_id}", web::get().to(get_item))
                .route("/{note_id}/items/{item_id}", web::patch().to(update_item))
                .route("/{note_id}/items/{item_id}", web::delete().to(delete_item))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes", owner: "delivery_notes", description: "List delivery notes (paginated, filter by status, shipment_id, rental_order_id, receiver, company_id, date range).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes", owner: "delivery_notes", description: "Create a draft delivery note with a full snapshot (parties, driver, vehicle, warehouse, items); number issued from the tenant's numbering settings.", auth },
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes/by-shipment/{shipment_id}", owner: "delivery_notes", description: "List the delivery notes of one logistics shipment (paginated).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/from-shipment/{shipment_id}", owner: "delivery_notes", description: "Create a draft delivery note from a logistics shipment id: the shipment is linked by id only and the snapshot in the body freezes its state at creation time (link recorded as unverified when logistics cannot confirm it).", auth },
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes/{note_id}", owner: "delivery_notes", description: "Get one delivery note with its items and quantity totals.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/delivery-notes/{note_id}", owner: "delivery_notes", description: "Update a draft delivery note's snapshot (parties, driver, vehicle, warehouse, notes, links).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/delivery-notes/{note_id}", owner: "delivery_notes", description: "Delete a draft delivery note with its items (issued notes are cancelled, never deleted).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/issue", owner: "delivery_notes", description: "Issue a draft delivery note (draft -> issued; the snapshot freezes).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/advance", owner: "delivery_notes", description: "Advance a delivery note to its next status, or to a named target status when the machine allows it (delivery still needs a linked proof of delivery).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/deliver", owner: "delivery_notes", description: "Deliver a delivery note: link the logistics proof of delivery and record the recipient signature metadata (in_transit -> delivered).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/return", owner: "delivery_notes", description: "Mark a delivery note returned (partial or full): per-item returned quantities plus the reason; the scope is computed from the quantities.", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/cancel", owner: "delivery_notes", description: "Cancel a delivery note that has not been delivered or returned (the row stays, so the number is never reused).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/print", owner: "delivery_notes", description: "Print a delivery note: queue a print job through the printing module and return the print-ready document inline (paper/pdf/thermal/email).", auth },
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes/{note_id}/print-preview", owner: "delivery_notes", description: "Preview the print-ready document of a delivery note without queueing a print job.", auth },
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes/{note_id}/items", owner: "delivery_notes", description: "List the items of a delivery note (paginated, filter by product_id).", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/{note_id}/items", owner: "delivery_notes", description: "Add one line to a draft delivery note (ordered and shipped start at the supplied quantity).", auth },
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes/{note_id}/items/{item_id}", owner: "delivery_notes", description: "Get one item of a delivery note.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/delivery-notes/{note_id}/items/{item_id}", owner: "delivery_notes", description: "Update one line of a draft delivery note (ordered/shipped; returned moves only through the return endpoint).", auth },
            RouteInfo { method: "DELETE", path: "/api/v1/delivery-notes/{note_id}/items/{item_id}", owner: "delivery_notes", description: "Remove one line from a draft delivery note (the last line cannot be removed).", auth },
            RouteInfo { method: "GET", path: "/api/v1/delivery-notes/settings", owner: "delivery_notes", description: "Get per-tenant delivery notes settings (number format, print defaults, delivery gates) plus a next-number example.", auth },
            RouteInfo { method: "PATCH", path: "/api/v1/delivery-notes/settings", owner: "delivery_notes", description: "Update per-tenant delivery notes settings (manager): number prefix, width, reset rule, print defaults, proof gate, auto-advance.", auth },
            RouteInfo { method: "POST", path: "/api/v1/delivery-notes/ai/{capability}", owner: "delivery_notes", description: "Run one registered delivery notes AI capability through the AI module's gated runner.", auth },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DeliveryNotesModule::new()));
}
