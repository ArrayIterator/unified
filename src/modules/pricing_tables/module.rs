//! pricing tables - module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use super::state::*;
use super::hooks::*;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_marketplace::{approve_payout, approve_product, create_commission_rule, delete_commission_rule, get_commission, get_commission_rule, get_payout, get_product_approval, list_commission_rules, list_commissions, list_payouts, list_product_approvals, mark_payout_paid, reject_payout, reject_product, request_payout, submit_product, update_commission_rule};
use super::handlers_tables::{create_feature, create_table, create_tier, delete_feature, delete_table, delete_tier, embed_table, get_feature, get_table, get_tier, list_features, list_tables, list_tiers, publish_table, render_table, unpublish_table, update_feature, update_table, update_tier};
use super::handlers_vendors::{apply_vendor, approve_vendor, get_vendor, list_vendor_commissions, list_vendor_payouts, list_vendors, reactivate_vendor, reject_vendor, suspend_vendor, update_vendor, vendor_dashboard, vendor_storefront};

pub struct PricingTablesModule {
    state: Arc<PricingTablesState>,
}

impl PricingTablesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PricingTablesState::new()),
        }
    }
}

impl Default for PricingTablesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PricingTablesModule {
    fn id(&self) -> &'static str {
        "pricing_tables"
    }

    fn name(&self) -> &'static str {
        "Pricing Tables & Marketplace Builder"
    }

    fn description(&self) -> &'static str {
        "Reusable pricing tables with tiers, feature lists, and a monthly/yearly billing-cycle toggle, plus a marketplace builder: vendor onboarding, product approval, commissions computed from store orders, vendor dashboards, and payouts through billing."
    }

    fn requires(&self) -> &[&'static str] {
        &["products", "store", "billing", "site_builder", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("pricing_tables", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx));
        bus.register("order.paid", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx));
        bus.register("store.order.completed", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_refunded(state.clone(), ctx));
        bus.register("store.order.refunded", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_invoice_created(state.clone(), ctx));
        bus.register("invoice.created", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_render_requested(state.clone(), ctx));
        bus.register("pricing_table.render_requested", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_vendor_check(state.clone(), ctx));
        bus.register("marketplace.vendor_check", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_commission_quote(state.clone(), ctx));
        bus.register("marketplace.commission_quote", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_product_approval_status(state.clone(), ctx)
        });
        bus.register(
            "marketplace.product_approval_status",
            "pricing_tables",
            handler,
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/pricing-tables")
                // Tables.
                .route("/tables", web::get().to(list_tables))
                .route("/tables", web::post().to(create_table))
                .route("/tables/{id}", web::get().to(get_table))
                .route("/tables/{id}", web::put().to(update_table))
                .route("/tables/{id}", web::delete().to(delete_table))
                .route("/tables/{id}/publish", web::post().to(publish_table))
                .route("/tables/{id}/unpublish", web::post().to(unpublish_table))
                .route("/tables/{id}/render", web::get().to(render_table))
                .route("/tables/{id}/embed", web::get().to(embed_table))
                // Tiers.
                .route("/tables/{id}/tiers", web::get().to(list_tiers))
                .route("/tables/{id}/tiers", web::post().to(create_tier))
                .route("/tables/{id}/tiers/{tier_id}", web::get().to(get_tier))
                .route("/tables/{id}/tiers/{tier_id}", web::put().to(update_tier))
                .route("/tables/{id}/tiers/{tier_id}", web::delete().to(delete_tier))
                // Tier features.
                .route(
                    "/tables/{id}/tiers/{tier_id}/features",
                    web::get().to(list_features),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features",
                    web::post().to(create_feature),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features/{feature_id}",
                    web::get().to(get_feature),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features/{feature_id}",
                    web::put().to(update_feature),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features/{feature_id}",
                    web::delete().to(delete_feature),
                )
                // Marketplace vendors.
                .route("/marketplace/vendors", web::get().to(list_vendors))
                .route("/marketplace/vendors", web::post().to(apply_vendor))
                .route("/marketplace/vendors/{id}", web::get().to(get_vendor))
                .route("/marketplace/vendors/{id}", web::put().to(update_vendor))
                .route(
                    "/marketplace/vendors/{id}/approve",
                    web::post().to(approve_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/reject",
                    web::post().to(reject_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/suspend",
                    web::post().to(suspend_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/reactivate",
                    web::post().to(reactivate_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/dashboard",
                    web::get().to(vendor_dashboard),
                )
                .route(
                    "/marketplace/vendors/{id}/commissions",
                    web::get().to(list_vendor_commissions),
                )
                .route(
                    "/marketplace/vendors/{id}/payouts",
                    web::get().to(list_vendor_payouts),
                )
                .route(
                    "/marketplace/vendors/{id}/payouts",
                    web::post().to(request_payout),
                )
                // Marketplace storefront.
                .route(
                    "/marketplace/storefronts/{slug}",
                    web::get().to(vendor_storefront),
                )
                // Commission rules.
                .route(
                    "/marketplace/commission-rules",
                    web::get().to(list_commission_rules),
                )
                .route(
                    "/marketplace/commission-rules",
                    web::post().to(create_commission_rule),
                )
                .route(
                    "/marketplace/commission-rules/{id}",
                    web::get().to(get_commission_rule),
                )
                .route(
                    "/marketplace/commission-rules/{id}",
                    web::put().to(update_commission_rule),
                )
                .route(
                    "/marketplace/commission-rules/{id}",
                    web::delete().to(delete_commission_rule),
                )
                // Commissions (read-only ledger).
                .route("/marketplace/commissions", web::get().to(list_commissions))
                .route(
                    "/marketplace/commissions/{id}",
                    web::get().to(get_commission),
                )
                // Product approvals.
                .route(
                    "/marketplace/products/{product_id}/submit",
                    web::post().to(submit_product),
                )
                .route(
                    "/marketplace/product-approvals",
                    web::get().to(list_product_approvals),
                )
                .route(
                    "/marketplace/product-approvals/{id}",
                    web::get().to(get_product_approval),
                )
                .route(
                    "/marketplace/product-approvals/{id}/approve",
                    web::post().to(approve_product),
                )
                .route(
                    "/marketplace/product-approvals/{id}/reject",
                    web::post().to(reject_product),
                )
                // Payouts (admin).
                .route("/marketplace/payouts", web::get().to(list_payouts))
                .route("/marketplace/payouts/{id}", web::get().to(get_payout))
                .route(
                    "/marketplace/payouts/{id}/approve",
                    web::post().to(approve_payout),
                )
                .route(
                    "/marketplace/payouts/{id}/reject",
                    web::post().to(reject_payout),
                )
                .route(
                    "/marketplace/payouts/{id}/mark-paid",
                    web::post().to(mark_payout_paid),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables", owner: "pricing_tables", description: "List pricing tables (paginated; filter by status, currency).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables", owner: "pricing_tables", description: "Create a pricing table.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}", owner: "pricing_tables", description: "Get one pricing table.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/tables/{id}", owner: "pricing_tables", description: "Update a pricing table.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/tables/{id}", owner: "pricing_tables", description: "Delete a pricing table with its tiers and features.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/publish", owner: "pricing_tables", description: "Publish a pricing table (requires at least one tier).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/unpublish", owner: "pricing_tables", description: "Unpublish a pricing table back to draft.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/render", owner: "pricing_tables", description: "Structured render data for a table (public when published; drafts need pricing_tables:read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/embed", owner: "pricing_tables", description: "Embed snippet data: table id, render URL, and placeholder snippet.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers", owner: "pricing_tables", description: "List a table's tiers in sort order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/tiers", owner: "pricing_tables", description: "Create a tier (first tier is highlighted; highlighting clears other tiers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}", owner: "pricing_tables", description: "Get one tier.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}", owner: "pricing_tables", description: "Update a tier (clearing the only highlight is rejected).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}", owner: "pricing_tables", description: "Delete a tier with its features (deleting the highlighted tier promotes the next one).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features", owner: "pricing_tables", description: "List a tier's feature rows in sort order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features", owner: "pricing_tables", description: "Create a tier feature row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features/{feature_id}", owner: "pricing_tables", description: "Get one tier feature row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features/{feature_id}", owner: "pricing_tables", description: "Update a tier feature row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features/{feature_id}", owner: "pricing_tables", description: "Delete a tier feature row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors", owner: "pricing_tables", description: "List vendors (paginated; filter by status, user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors", owner: "pricing_tables", description: "Apply as a vendor (self-service; admin may file for another user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}", owner: "pricing_tables", description: "Get one vendor (own record or marketplace:read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/marketplace/vendors/{id}", owner: "pricing_tables", description: "Update a vendor (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/approve", owner: "pricing_tables", description: "Approve a pending or suspended vendor.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/reject", owner: "pricing_tables", description: "Reject a pending vendor (reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/suspend", owner: "pricing_tables", description: "Suspend an approved vendor.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/reactivate", owner: "pricing_tables", description: "Reactivate a suspended vendor.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/dashboard", owner: "pricing_tables", description: "Vendor dashboard: sales, orders, commissions, payouts, products (aggregated live).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/commissions", owner: "pricing_tables", description: "List one vendor's commission ledger entries.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/payouts", owner: "pricing_tables", description: "List one vendor's payouts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/payouts", owner: "pricing_tables", description: "Request a payout (capped at the available pending commission balance).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/storefronts/{slug}", owner: "pricing_tables", description: "Public storefront data for an approved vendor (vendor + approved product ids).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commission-rules", owner: "pricing_tables", description: "List commission rules (paginated; filter by scope).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/commission-rules", owner: "pricing_tables", description: "Create a commission rule (vendor, category, or global scope).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commission-rules/{id}", owner: "pricing_tables", description: "Get one commission rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/marketplace/commission-rules/{id}", owner: "pricing_tables", description: "Update a commission rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/marketplace/commission-rules/{id}", owner: "pricing_tables", description: "Delete a commission rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commissions", owner: "pricing_tables", description: "List the commission ledger (paginated; filter by vendor, order, product, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commissions/{id}", owner: "pricing_tables", description: "Get one commission ledger entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/products/{product_id}/submit", owner: "pricing_tables", description: "Vendor submits a product for marketplace approval.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/product-approvals", owner: "pricing_tables", description: "List product approvals (paginated; filter by status, vendor, product).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/product-approvals/{id}", owner: "pricing_tables", description: "Get one product approval.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/product-approvals/{id}/approve", owner: "pricing_tables", description: "Approve a pending product.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/product-approvals/{id}/reject", owner: "pricing_tables", description: "Reject a pending product (reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/payouts", owner: "pricing_tables", description: "List payouts (paginated; filter by vendor, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/payouts/{id}", owner: "pricing_tables", description: "Get one payout.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/payouts/{id}/approve", owner: "pricing_tables", description: "Approve a requested payout.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/payouts/{id}/reject", owner: "pricing_tables", description: "Reject a requested payout (reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/payouts/{id}/mark-paid", owner: "pricing_tables", description: "Mark an approved payout paid and settle pending commissions oldest-first up to the amount.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PricingTablesModule::new()));
}
