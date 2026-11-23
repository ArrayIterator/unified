// Pos module — module definition and registration.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Point of Sale module.
pub struct PosModule {
    pub(crate) state: PosState,
}

impl PosModule {
    pub fn new() -> Self {
        Self {
            state: PosState::default(),
        }
    }
}

impl Default for PosModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PosModule {
    fn id(&self) -> &'static str {
        "pos"
    }

    fn name(&self) -> &'static str {
        "Point of Sale"
    }

    fn description(&self) -> &'static str {
        "In-person sales at a physical till: registers with shifts and cashier assignment, cart sales with discounts and taxes, configurable payment methods with split payment and change, structured receipts, refunds and voids, and end-of-shift and end-of-day reports. Sales emit store orders, billing invoices, and stock movements through the hook bus."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["store", "billing", "assets", "tax"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store", "billing", "assets", "tax"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        self.state.seed_payment_methods();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/pos")
                .app_data(state)
                // Registers.
                .route("/registers", web::get().to(list_registers))
                .route("/registers", web::post().to(create_register))
                .route("/registers/{id}", web::get().to(get_register))
                .route("/registers/{id}", web::patch().to(update_register))
                .route("/registers/{id}", web::delete().to(delete_register))
                .route(
                    "/registers/{id}/assign-cashier",
                    web::post().to(assign_cashier),
                )
                .route(
                    "/registers/{id}/unassign-cashier",
                    web::post().to(unassign_cashier),
                )
                // Shifts.
                .route("/shifts", web::get().to(list_shifts))
                .route("/shifts/open", web::post().to(open_shift))
                .route("/shifts/{id}", web::get().to(get_shift))
                .route("/shifts/{id}/close", web::post().to(close_shift))
                .route("/shifts/{id}/summary", web::get().to(get_shift_summary))
                // Sales.
                .route("/sales", web::get().to(list_sales))
                .route("/sales", web::post().to(create_sale))
                .route("/sales/{id}", web::get().to(get_sale))
                .route("/sales/{id}", web::patch().to(update_sale))
                .route("/sales/{id}/items", web::post().to(add_sale_item))
                .route(
                    "/sales/{id}/items/{item_id}",
                    web::patch().to(update_sale_item),
                )
                .route(
                    "/sales/{id}/items/{item_id}",
                    web::delete().to(remove_sale_item),
                )
                .route("/sales/{id}/complete", web::post().to(complete_sale))
                .route("/sales/{id}/void", web::post().to(void_sale))
                .route("/sales/{id}/refund", web::post().to(refund_sale))
                .route("/sales/{id}/hold", web::post().to(hold_sale))
                .route("/sales/{id}/resume", web::post().to(resume_sale))
                // Receipts.
                .route("/receipts", web::get().to(list_receipts))
                .route("/receipts/{sale_id}", web::get().to(get_receipt))
                .route(
                    "/receipts/{sale_id}/print",
                    web::get().to(get_receipt_print),
                )
                // Catalogue lookup (hook-driven).
                .route("/products/search", web::get().to(search_products))
                // Payment methods (admin configuration).
                .route("/payment-methods", web::get().to(list_payment_methods))
                .route(
                    "/payment-methods",
                    web::post().to(create_payment_method),
                )
                .route(
                    "/payment-methods/{id}",
                    web::patch().to(update_payment_method),
                )
                .route(
                    "/payment-methods/{id}",
                    web::delete().to(delete_payment_method),
                )
                // Settings.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                // Reports.
                .route("/reports/end-of-day", web::get().to(end_of_day_report)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/pos/registers", owner: "pos", description: "List registers (paginated, filter by status, search by name/code).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/registers", owner: "pos", description: "Create a register (code must be unique when given).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/registers/{id}", owner: "pos", description: "Get one register with its assigned cashiers.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/pos/registers/{id}", owner: "pos", description: "Update a register (name, location, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pos/registers/{id}", owner: "pos", description: "Delete a register with no open shift and no sales.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/registers/{id}/assign-cashier", owner: "pos", description: "Assign a cashier (user id) to a register.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/registers/{id}/unassign-cashier", owner: "pos", description: "Remove a cashier from a register.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/shifts", owner: "pos", description: "List shifts (paginated, filter by register_id, cashier_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/shifts/open", owner: "pos", description: "Open a shift on a register with an opening float (one open shift per register).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/shifts/{id}", owner: "pos", description: "Get one shift.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/shifts/{id}/close", owner: "pos", description: "Close a shift with a counted cash drawer; reports expected cash and variance.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/shifts/{id}/summary", owner: "pos", description: "Shift totals by payment method, refunds, voids, and cash variance.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/sales", owner: "pos", description: "List sales (paginated, filter by register_id, shift_id, cashier_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales", owner: "pos", description: "Create a draft sale (cart) on a register, optionally in an open shift.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/sales/{id}", owner: "pos", description: "Get one sale with its lines and payments.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/pos/sales/{id}", owner: "pos", description: "Update a draft sale (sale-level discount, customer, notes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/items", owner: "pos", description: "Add a line to a draft sale (product, price, quantity, discount, tax rate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/pos/sales/{id}/items/{item_id}", owner: "pos", description: "Update a draft sale line (quantity, price, discount, tax rate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pos/sales/{id}/items/{item_id}", owner: "pos", description: "Remove a line from a draft sale.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/complete", owner: "pos", description: "Complete a sale with one or more payments (split payment, change); emits order.created, pos.sale.completed, stock decrement events, and creates the receipt.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/void", owner: "pos", description: "Void a draft or held sale (completed sales use refund).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/refund", owner: "pos", description: "Refund a completed sale by line items or by amount; emits payment.refunded and stock increment events.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/hold", owner: "pos", description: "Park a draft sale for later.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/resume", owner: "pos", description: "Resume a held sale back to draft.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/receipts", owner: "pos", description: "List receipts (paginated, filter by register_id, cashier_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/receipts/{sale_id}", owner: "pos", description: "Get the receipt data of a completed sale.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/receipts/{sale_id}/print", owner: "pos", description: "Get the structured printable receipt lines for a completed sale.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/products/search", owner: "pos", description: "Search the catalogue by text, barcode, or SKU; currently reports honestly that lookup is unavailable — no catalogue handler is registered for pos.product.search_requested yet (pending catalogue integration).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/payment-methods", owner: "pos", description: "List the configured payment methods (paginated, filter by active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pos/payment-methods", owner: "pos", description: "Add a payment method definition (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/pos/payment-methods/{id}", owner: "pos", description: "Update a payment method definition (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pos/payment-methods/{id}", owner: "pos", description: "Delete a payment method definition that no payment references (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/settings", owner: "pos", description: "Get the POS settings, including the offline-queueing capability flag.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pos/settings", owner: "pos", description: "Update the POS settings (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pos/reports/end-of-day", owner: "pos", description: "End-of-day report: per-register totals by payment method, refunds, voids, and shift cash variances for one date.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PosModule::new()));
}
