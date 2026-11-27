// procurement -- Module struct, Module impl and registration.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::handlers_lines::*;
use super::handlers_orders::*;
use super::handlers_receipts_budgets::*;
use super::handlers_vendors::*;
use super::state::*;
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct ProcurementModule {
    state: ProcurementState,
}

impl ProcurementModule {
    pub fn new() -> Self {
        Self {
            state: ProcurementState::default(),
        }
    }
}

impl Default for ProcurementModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ProcurementModule {
    fn id(&self) -> &'static str {
        "procurement"
    }

    fn name(&self) -> &'static str {
        "Procurement / Vendor Management"
    }

    fn description(&self) -> &'static str {
        "Vendors, purchase orders with server-side totals and an approval workflow, goods receipt against PO lines with over-receipt rejection, spend tracking with per-category and per-vendor budgets (over-budget warnings never block), and vendor performance from promised vs received dates. Vendor payments and inventory stock movements are not handled here."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    /// Procurement is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("procurement", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/procurement")
                .app_data(state)
                .route("/vendors", web::get().to(list_vendors))
                .route("/vendors", web::post().to(create_vendor))
                .route("/vendors/{id}", web::get().to(get_vendor))
                .route("/vendors/{id}", web::patch().to(update_vendor))
                .route("/vendors/{id}", web::delete().to(delete_vendor))
                .route(
                    "/vendors/{id}/performance",
                    web::get().to(vendor_performance),
                )
                .route("/purchase-orders", web::get().to(list_purchase_orders))
                .route("/purchase-orders", web::post().to(create_purchase_order))
                .route("/purchase-orders/{id}", web::get().to(get_purchase_order))
                .route(
                    "/purchase-orders/{id}",
                    web::patch().to(update_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}",
                    web::delete().to(delete_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/submit",
                    web::post().to(submit_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/approve",
                    web::post().to(approve_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/reject",
                    web::post().to(reject_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/order",
                    web::post().to(order_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/receive",
                    web::post().to(receive_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/close",
                    web::post().to(close_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/cancel",
                    web::post().to(cancel_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/approvals",
                    web::get().to(list_po_approvals),
                )
                .route(
                    "/purchase-orders/{id}/receipts",
                    web::get().to(list_po_receipts),
                )
                .route("/purchase-orders/{id}/lines", web::get().to(list_po_lines))
                .route("/purchase-orders/{id}/lines", web::post().to(add_po_line))
                .route(
                    "/purchase-orders/{id}/lines/{line_id}",
                    web::patch().to(update_po_line),
                )
                .route(
                    "/purchase-orders/{id}/lines/{line_id}",
                    web::delete().to(delete_po_line),
                )
                .route("/receipts", web::get().to(list_receipts))
                .route("/budgets", web::get().to(list_budgets))
                .route("/budgets", web::post().to(create_budget))
                .route("/budgets/{id}", web::get().to(get_budget))
                .route("/budgets/{id}", web::patch().to(update_budget))
                .route("/budgets/{id}", web::delete().to(delete_budget))
                .route("/spend-summary", web::get().to(spend_summary)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/vendors",
                owner: "procurement",
                description: "List vendors (paginated, filter by status, category, search).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/vendors",
                owner: "procurement",
                description: "Create a vendor (payment_reference is a reference only; never raw bank secrets).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/vendors/{id}",
                owner: "procurement",
                description: "Get one vendor.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/procurement/vendors/{id}",
                owner: "procurement",
                description: "Update a vendor.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/procurement/vendors/{id}",
                owner: "procurement",
                description: "Delete a vendor with no purchase orders.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/vendors/{id}/performance",
                owner: "procurement",
                description: "Vendor performance: on-time delivery rate, committed spend, quality notes.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/purchase-orders",
                owner: "procurement",
                description: "List purchase orders (paginated, filter by status, vendor_id, category, search).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders",
                owner: "procurement",
                description: "Create a purchase order (draft; totals computed server-side from lines).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/purchase-orders/{id}",
                owner: "procurement",
                description: "Get one purchase order with lines and budget warnings.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/procurement/purchase-orders/{id}",
                owner: "procurement",
                description: "Update a draft purchase order only.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/procurement/purchase-orders/{id}",
                owner: "procurement",
                description: "Delete a draft purchase order only.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/submit",
                owner: "procurement",
                description: "Submit a draft purchase order for approval (draft -> pending_approval).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/approve",
                owner: "procurement",
                description: "Approve a purchase order (approval row recorded; over-budget warning is advisory only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/reject",
                owner: "procurement",
                description: "Reject a purchase order with a reason (returns it to draft; rejection recorded).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/order",
                owner: "procurement",
                description: "Place an approved purchase order with the vendor (approved -> ordered).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/receive",
                owner: "procurement",
                description: "Receive goods against a PO line (partial receipt supported; over-receipt rejected).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/close",
                owner: "procurement",
                description: "Close a received or partially received purchase order.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/cancel",
                owner: "procurement",
                description: "Cancel a draft, pending-approval, or approved purchase order.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/purchase-orders/{id}/approvals",
                owner: "procurement",
                description: "Approval history of a purchase order (paginated, filter by decision).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/purchase-orders/{id}/receipts",
                owner: "procurement",
                description: "Goods receipts of a purchase order (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/purchase-orders/{id}/lines",
                owner: "procurement",
                description: "List lines of a purchase order (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/purchase-orders/{id}/lines",
                owner: "procurement",
                description: "Add a line to a draft purchase order (totals recomputed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/procurement/purchase-orders/{id}/lines/{line_id}",
                owner: "procurement",
                description: "Update a line of a draft purchase order (totals recomputed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/procurement/purchase-orders/{id}/lines/{line_id}",
                owner: "procurement",
                description: "Remove a line from a draft purchase order (totals recomputed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/receipts",
                owner: "procurement",
                description: "List all goods receipts (paginated, filter by purchase_order_id, vendor_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/budgets",
                owner: "procurement",
                description: "List budgets with spend vs budget on read (filter by category, vendor_id, over_budget).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/procurement/budgets",
                owner: "procurement",
                description: "Create a budget scoped to a category, a vendor, or both.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/budgets/{id}",
                owner: "procurement",
                description: "Get one budget with spend vs budget on read.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/procurement/budgets/{id}",
                owner: "procurement",
                description: "Update a budget.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/procurement/budgets/{id}",
                owner: "procurement",
                description: "Delete a budget.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/procurement/spend-summary",
                owner: "procurement",
                description: "Committed spend totals with per-category / per-vendor breakdowns and budget comparison (filter by from, to, category, vendor_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ProcurementModule::new()));
}
