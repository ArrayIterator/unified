//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_fulfilment::{
    create_damage_report, get_damage_report, get_delivery, handover_order, list_damage_reports,
    list_deliveries, resolve_damage_report, return_order,
};
use super::handlers_listings::{
    check_listing_availability, create_listing, create_rate, delete_listing, delete_rate,
    get_listing, get_public_listing, list_listings, list_public_listings, list_rates,
    update_listing, update_rate,
};
use super::handlers_orders::{
    cancel_order, complete_order, create_order, delete_order, get_order, handle_deposit,
    list_orders, reserve_order, update_order,
};
use super::handlers_settings::{get_settings, update_settings};
use super::hooks::{
    on_contract_signed, on_payment_captured, on_payment_refunded, on_shipment_delivered,
    on_user_deleted,
};
use super::state::RentalState;
use super::types::{
    EVENT_CONTRACT_SIGNED, EVENT_PAYMENT_CAPTURED, EVENT_PAYMENT_REFUNDED,
    EVENT_SHIPMENT_DELIVERED, EVENT_USER_DELETED,
};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// Module

pub struct RentalModule {
    pub(crate) state: RentalState,
}

impl RentalModule {
    pub fn new() -> Self {
        Self {
            state: RentalState::default(),
        }
    }
}

impl Default for RentalModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RentalModule {
    fn id(&self) -> &'static str {
        "rental"
    }

    fn name(&self) -> &'static str {
        "Rental"
    }

    fn description(&self) -> &'static str {
        "Rentable products: listings that turn a store product into a rentable item with data-driven per-period pricing, a refundable deposit, and a data-driven late-fee rule; orders with a status machine (draft, reserved, out, returned, completed, cancelled) priced from the chosen rate and checked against an availability calendar that blocks double-booking; hand-out and return flows with condition notes and photo metadata; damage reports that charge the order once on resolve; deposit hold, refund, and deduct handling; and per-tenant settings. Contracts are linked by reference for the rental agreement, payments drive deposit capture and refund, and logistics deliveries are traced on the order."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "users",
            "roles",
            "notifications",
            "store",
            "products",
            "payments",
            "contracts",
            "logistics",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "notifications",
            "store",
            "products",
            "payments",
            "contracts",
            "logistics",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("rental", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "rental",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PAYMENT_CAPTURED,
            "rental",
            Arc::new(move |ctx: &mut HookContext| on_payment_captured(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PAYMENT_REFUNDED,
            "rental",
            Arc::new(move |ctx: &mut HookContext| on_payment_refunded(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SHIPMENT_DELIVERED,
            "rental",
            Arc::new(move |ctx: &mut HookContext| on_shipment_delivered(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_CONTRACT_SIGNED,
            "rental",
            Arc::new(move |ctx: &mut HookContext| on_contract_signed(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/rental")
                .app_data(state)
                .route("/public/listings", web::get().to(list_public_listings))
                .route("/public/listings/{slug}", web::get().to(get_public_listing))
                .route("/listings", web::get().to(list_listings))
                .route("/listings", web::post().to(create_listing))
                .route("/listings/{listing_id}", web::get().to(get_listing))
                .route("/listings/{listing_id}", web::patch().to(update_listing))
                .route("/listings/{listing_id}", web::delete().to(delete_listing))
                .route("/listings/{listing_id}/rates", web::get().to(list_rates))
                .route("/listings/{listing_id}/rates", web::post().to(create_rate))
                .route(
                    "/listings/{listing_id}/rates/{rate_id}",
                    web::patch().to(update_rate),
                )
                .route(
                    "/listings/{listing_id}/rates/{rate_id}",
                    web::delete().to(delete_rate),
                )
                .route(
                    "/listings/{listing_id}/availability",
                    web::get().to(check_listing_availability),
                )
                .route("/orders", web::get().to(list_orders))
                .route("/orders", web::post().to(create_order))
                .route("/orders/{order_id}", web::get().to(get_order))
                .route("/orders/{order_id}", web::patch().to(update_order))
                .route("/orders/{order_id}", web::delete().to(delete_order))
                .route("/orders/{order_id}/reserve", web::post().to(reserve_order))
                .route("/orders/{order_id}/cancel", web::post().to(cancel_order))
                .route(
                    "/orders/{order_id}/complete",
                    web::post().to(complete_order),
                )
                .route("/orders/{order_id}/deposit", web::post().to(handle_deposit))
                .route(
                    "/orders/{order_id}/handover",
                    web::post().to(handover_order),
                )
                .route("/orders/{order_id}/return", web::post().to(return_order))
                .route(
                    "/orders/{order_id}/deliveries",
                    web::get().to(list_deliveries),
                )
                .route(
                    "/orders/{order_id}/deliveries/{delivery_id}",
                    web::get().to(get_delivery),
                )
                .route(
                    "/orders/{order_id}/damage-reports",
                    web::get().to(list_damage_reports),
                )
                .route(
                    "/orders/{order_id}/damage-reports",
                    web::post().to(create_damage_report),
                )
                .route(
                    "/orders/{order_id}/damage-reports/{report_id}",
                    web::get().to(get_damage_report),
                )
                .route(
                    "/orders/{order_id}/damage-reports/{report_id}/resolve",
                    web::post().to(resolve_damage_report),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/public/listings",
                owner: "rental",
                description: "PUBLIC: list active rental listings with their rates (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/public/listings/{slug}",
                owner: "rental",
                description: "PUBLIC: one active rental listing by slug with its rates (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/listings",
                owner: "rental",
                description: "List listings (paginated, filter by active, product_id; search by title/description).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/listings",
                owner: "rental",
                description: "Create a rental listing for a store product with deposit and a late-fee rule (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/listings/{listing_id}",
                owner: "rental",
                description: "Get one listing with its rates.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/rental/listings/{listing_id}",
                owner: "rental",
                description: "Update a listing (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/rental/listings/{listing_id}",
                owner: "rental",
                description: "Delete a listing with no orders, with its rates (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/listings/{listing_id}/rates",
                owner: "rental",
                description: "List the per-period rates of a listing (paginated, filter by period_unit).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/listings/{listing_id}/rates",
                owner: "rental",
                description: "Add a per-period rate to a listing (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/rental/listings/{listing_id}/rates/{rate_id}",
                owner: "rental",
                description: "Update a rate (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/rental/listings/{listing_id}/rates/{rate_id}",
                owner: "rental",
                description: "Delete a rate with no orders (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/listings/{listing_id}/availability",
                owner: "rental",
                description: "Availability check for a listing over a date range (?starts_at=, ends_at=, quantity=).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/orders",
                owner: "rental",
                description: "List orders (paginated, filter by status, listing_id, customer_id, starts_from, starts_to; customers see only their own).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders",
                owner: "rental",
                description: "Create a draft order priced from a rate, rejected when the range is double-booked.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/orders/{order_id}",
                owner: "rental",
                description: "Get one order with its deliveries and damage reports (customer or manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/rental/orders/{order_id}",
                owner: "rental",
                description: "Update an order's linked contract id (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/rental/orders/{order_id}",
                owner: "rental",
                description: "Delete a draft order (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/reserve",
                owner: "rental",
                description: "Reserve a draft order (draft -> reserved; availability is re-checked).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/cancel",
                owner: "rental",
                description: "Cancel a draft or reserved order (cancel cutoff applies to customers, not managers).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/complete",
                owner: "rental",
                description: "Complete a returned order (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/deposit",
                owner: "rental",
                description: "Refund the held deposit in full or deduct an amount from it (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/handover",
                owner: "rental",
                description: "Hand a reserved order out with condition notes and photo metadata (manager; reserved -> out).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/return",
                owner: "rental",
                description: "Return an out order with condition notes and photo metadata; the late fee is computed from the listing rule (manager; out -> returned).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/orders/{order_id}/deliveries",
                owner: "rental",
                description: "List hand-out and return deliveries of an order (paginated, filter by kind; customer or manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/orders/{order_id}/deliveries/{delivery_id}",
                owner: "rental",
                description: "Get one delivery of an order (customer or manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/orders/{order_id}/damage-reports",
                owner: "rental",
                description: "List damage reports of an order (paginated, filter by status; customer or manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/damage-reports",
                owner: "rental",
                description: "Report damage on an out or returned order with an estimated cost (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/orders/{order_id}/damage-reports/{report_id}",
                owner: "rental",
                description: "Get one damage report (customer or manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/rental/orders/{order_id}/damage-reports/{report_id}/resolve",
                owner: "rental",
                description: "Resolve a damage report (adds the estimated cost to the order total once) or dismiss it (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/rental/settings",
                owner: "rental",
                description: "Get per-tenant rental settings (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/rental/settings",
                owner: "rental",
                description: "Update per-tenant rental settings (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RentalModule::new()));
}
