// pricing — module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::middleware::TenantContext;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::money::*;
use super::recommendations::*;
use super::discounts::*;
use super::proration::*;
use super::reconciliation::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct PricingModule {
    pub(crate) state: Arc<PricingState>,
}

impl PricingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PricingState::new()),
        }
    }
}

impl Default for PricingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PricingModule {
    fn id(&self) -> &'static str {
        "pricing"
    }

    fn name(&self) -> &'static str {
        "Smart Pricing"
    }

    fn description(&self) -> &'static str {
        "Price recommendations (cost-plus, tiered, bundle), discount and coupon engine with declared evaluation order, per-day proration, and server-side invoice reconciliation with credit notes. All money is integer scaled minor units (NUMERIC(19,4)); the engine never sets a price silently and never mutates an order or an invoice."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["billing", "store", "products"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("pricing", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "pricing.discount.evaluate_requested",
            "pricing",
            Arc::new(move |ctx: &mut HookContext| {
                on_discount_evaluate_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "pricing.invoice.validate_requested",
            "pricing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_validate_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "pricing.ai.recommend_requested",
            "pricing",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_recommend_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/pricing")
                // Recommendations (suggestions only; confirm creates
                // a draft price version, never an active price).
                .route("/recommendations/cost-plus", web::post().to(create_cost_plus_recommendation))
                .route("/recommendations/tiered", web::post().to(create_tiered_recommendation))
                .route("/recommendations/bundle", web::post().to(create_bundle_recommendation))
                .route("/recommendations", web::get().to(list_recommendations))
                .route("/recommendations/{id}", web::get().to(get_recommendation))
                .route("/recommendations/{id}/confirm", web::post().to(confirm_recommendation))
                .route("/recommendations/{id}/narrative", web::get().to(get_recommendation_narrative))
                // Discount rules.
                .route("/rules", web::get().to(list_rules))
                .route("/rules", web::post().to(create_rule))
                .route("/rules/{id}", web::get().to(get_rule))
                .route("/rules/{id}", web::put().to(update_rule))
                .route("/rules/{id}", web::delete().to(delete_rule))
                // Coupons.
                .route("/coupons", web::get().to(list_coupons))
                .route("/coupons", web::post().to(create_coupon))
                .route("/coupons/{id}", web::get().to(get_coupon))
                .route("/coupons/{id}", web::put().to(update_coupon))
                .route("/coupons/{id}", web::delete().to(delete_coupon))
                .route("/coupons/{id}/redeem", web::post().to(redeem_coupon))
                .route("/coupons/{id}/redemptions", web::get().to(list_coupon_redemptions))
                // Discount evaluation.
                .route("/discounts/evaluate", web::post().to(evaluate_discounts_endpoint))
                // Market reference prices.
                .route("/reference-prices", web::get().to(list_reference_prices))
                .route("/reference-prices", web::post().to(create_reference_price))
                .route("/reference-prices/summary", web::get().to(summarize_reference_prices))
                .route("/reference-prices/{id}", web::get().to(get_reference_price))
                .route("/reference-prices/{id}", web::delete().to(delete_reference_price))
                // Price versions and approvals.
                .route("/price-versions", web::get().to(list_price_versions))
                .route("/price-versions", web::post().to(create_price_version))
                .route("/price-versions/{id}", web::get().to(get_price_version))
                .route("/price-versions/{id}/submit", web::post().to(submit_price_version))
                .route("/price-versions/{id}/approve", web::post().to(approve_price_version))
                .route("/price-versions/{id}/reject", web::post().to(reject_price_version))
                .route("/price-versions/{id}/activate", web::post().to(activate_price_version))
                .route("/price-versions/{id}/approvals", web::get().to(list_price_approvals))
                // Tiers.
                .route("/tiers", web::get().to(list_tiers))
                .route("/tiers", web::post().to(create_tier))
                .route("/tiers/price", web::get().to(price_with_stored_tiers))
                .route("/tiers/{id}", web::delete().to(delete_tier))
                // Bundles.
                .route("/bundles", web::get().to(list_bundles))
                .route("/bundles", web::post().to(create_bundle))
                .route("/bundles/{id}", web::get().to(get_bundle))
                .route("/bundles/{id}/price", web::post().to(price_bundle))
                // Proration.
                .route("/proration/compute", web::post().to(compute_proration))
                .route("/proration", web::get().to(list_prorations))
                .route("/proration/{id}", web::get().to(get_proration))
                // Invoice preview, reconciliation, credit notes.
                .route("/invoices/preview", web::post().to(preview_invoice))
                .route("/invoices/reconcile", web::post().to(reconcile_invoice_endpoint))
                .route("/reconciliations", web::get().to(list_reconciliations))
                .route("/reconciliations/{id}", web::get().to(get_reconciliation))
                .route("/credit-notes", web::get().to(list_credit_notes))
                .route("/credit-notes", web::post().to(create_credit_note))
                .route("/credit-notes/{id}", web::get().to(get_credit_note))
                .route("/credit-notes/{id}/reconcile", web::post().to(reconcile_credit_note_endpoint))
                // Settings, currencies, conversion.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/currencies", web::get().to(list_currencies))
                .route("/currencies", web::post().to(create_currency))
                .route("/currencies/{code}", web::put().to(update_currency))
                .route("/convert", web::post().to(convert_currency_endpoint)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/cost-plus", owner: "pricing", description: "Compute a cost-plus price recommendation (suggestion only; shows total cost, margin, suggested price).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/tiered", owner: "pricing", description: "Compute a tiered price recommendation for a quantity, with the tier breakdown.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/bundle", owner: "pricing", description: "Compute a bundle price recommendation, line by line.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/recommendations", owner: "pricing", description: "List price recommendations (paginated; filter by product_id, kind, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/recommendations/{id}", owner: "pricing", description: "Get one price recommendation.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/{id}/confirm", owner: "pricing", description: "Confirm a recommendation: creates a draft price version, never an active price.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/recommendations/{id}/narrative", owner: "pricing", description: "AI narrative draft for a recommendation, built from the engine numbers only.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/rules", owner: "pricing", description: "List discount rules (paginated; filter by kind, status, product_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/rules", owner: "pricing", description: "Create a discount rule with a declared evaluation order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/rules/{id}", owner: "pricing", description: "Get one discount rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing/rules/{id}", owner: "pricing", description: "Update a discount rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/rules/{id}", owner: "pricing", description: "Delete a discount rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/coupons", owner: "pricing", description: "List coupons (paginated; filter by kind, active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/coupons", owner: "pricing", description: "Create a coupon with validity window and usage limits.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/coupons/{id}", owner: "pricing", description: "Get one coupon.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing/coupons/{id}", owner: "pricing", description: "Update a coupon.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/coupons/{id}", owner: "pricing", description: "Delete a coupon.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/coupons/{id}/redeem", owner: "pricing", description: "Record one coupon redemption for a customer (re-checks limits).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/coupons/{id}/redemptions", owner: "pricing", description: "List redemptions of one coupon (paginated; filter by customer_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/discounts/evaluate", owner: "pricing", description: "Evaluate discounts for order lines in declared rule order, with the full trace.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/reference-prices", owner: "pricing", description: "List human-entered market reference prices (paginated; filter by product_id, source).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/reference-prices", owner: "pricing", description: "Record a market reference price with its source and observation date.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/reference-prices/summary", owner: "pricing", description: "Summarize reference prices for one product (count, lowest, highest, median per currency).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/reference-prices/{id}", owner: "pricing", description: "Get one reference price.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/reference-prices/{id}", owner: "pricing", description: "Delete a reference price.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/price-versions", owner: "pricing", description: "List versioned prices (paginated; filter by product_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions", owner: "pricing", description: "Create a draft price version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/price-versions/{id}", owner: "pricing", description: "Get one price version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/submit", owner: "pricing", description: "Submit a draft price version (above threshold goes to pending_approval).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/approve", owner: "pricing", description: "Approve a price version (second approver when above the threshold).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/reject", owner: "pricing", description: "Reject a price version back to draft.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/activate", owner: "pricing", description: "Activate an approved price version (supersedes the previous active one).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/price-versions/{id}/approvals", owner: "pricing", description: "List approval decisions for one price version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/tiers", owner: "pricing", description: "List pricing tiers (paginated; filter by product_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/tiers", owner: "pricing", description: "Create a pricing tier (overlap with an existing tier is rejected).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/tiers/price", owner: "pricing", description: "Price a quantity against the stored tiers for a product.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/tiers/{id}", owner: "pricing", description: "Delete a pricing tier.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/bundles", owner: "pricing", description: "List bundles (paginated; filter by product_id, active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/bundles", owner: "pricing", description: "Create a bundle with components and a bundle discount.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/bundles/{id}", owner: "pricing", description: "Get one bundle with its components.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/bundles/{id}/price", owner: "pricing", description: "Price a stored bundle, line by line.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/proration/compute", owner: "pricing", description: "Compute proration for a plan or seat change, with the exact daily math.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/proration", owner: "pricing", description: "List proration records (paginated; filter by subscription_id, customer_id, kind).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/proration/{id}", owner: "pricing", description: "Get one proration record.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/invoices/preview", owner: "pricing", description: "Preview an invoice with its reconciliation result (admin sees exact figures first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/invoices/reconcile", owner: "pricing", description: "Reconcile an invoice before issuing: every component must match exactly.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/reconciliations", owner: "pricing", description: "List reconciliation runs (paginated; filter by invoice_id, matched).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/reconciliations/{id}", owner: "pricing", description: "Get one reconciliation run.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/credit-notes", owner: "pricing", description: "List credit notes (paginated; filter by invoice_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/credit-notes", owner: "pricing", description: "Create a credit note (lines must add up to the credit amount; issued notes are immutable).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/credit-notes/{id}", owner: "pricing", description: "Get one credit note.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/credit-notes/{id}/reconcile", owner: "pricing", description: "Reconcile a credit note: its lines must equal its amount exactly.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/settings", owner: "pricing", description: "Get the tenant pricing settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing/settings", owner: "pricing", description: "Update the tenant pricing settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/pricing/currencies", owner: "pricing", description: "List pricing currencies (paginated; filter by active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/currencies", owner: "pricing", description: "Add a pricing currency with an optional stored exchange-rate ratio.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/pricing/currencies/{code}", owner: "pricing", description: "Update a pricing currency or its stored exchange rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/pricing/convert", owner: "pricing", description: "Convert an amount with a stored exchange-rate ratio only (missing rate is an error).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PricingModule::new()));
}
