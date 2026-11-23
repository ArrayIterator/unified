// affiliates module — Module definition and registration.
//
// Split from the former single-file module `src/modules/affiliates.rs`.
// The module root (`affiliates.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{STORE_ORDER_COMPLETED, STORE_ORDER_REFUNDED, USER_CREATED};
use super::types::{Affiliate, AffiliatesState};
use super::helpers::caller;
use super::hooks::{on_store_order_completed, on_store_order_refunded, on_user_created};
use super::handlers_affiliates::{approve_affiliate, create_affiliate, create_commission_rule, create_link, delete_affiliate, delete_commission_rule, delete_link, get_affiliate, get_link, list_affiliates, list_commission_rules, list_links, reactivate_affiliate, reject_affiliate, suspend_affiliate, update_affiliate};
use super::handlers_tracking::{approve_commission, approve_payout, create_conversion, get_dashboard, list_clicks, list_commissions, list_conversions, list_payouts, mark_payout_paid, my_dashboard, reject_payout, request_payout, track_click, void_commission};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AffiliatesModule {
    pub(crate) state: AffiliatesState,
}

impl AffiliatesModule {
    pub fn new() -> Self {
        Self {
            state: AffiliatesState::default(),
        }
    }
}

impl Default for AffiliatesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AffiliatesModule {
    fn id(&self) -> &'static str {
        "affiliates"
    }

    fn name(&self) -> &'static str {
        "Affiliate / Referral"
    }

    fn description(&self) -> &'static str {
        "Affiliate and referral program: affiliate applications with an approval workflow, unique referral links and codes, click tracking with fraud flags for review only, signup and purchase conversions with last-click attribution, percent or fixed commissions per product or global default, and payout requests emitted to billing (no money moves here)."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "store", "billing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json", "sha2", "rand"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("affiliates", AI_CAPABILITIES);
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
            USER_CREATED,
            "affiliates",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_COMPLETED,
            "affiliates",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_REFUNDED,
            "affiliates",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_refunded(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/affiliates")
                .app_data(state)
                .route("", web::get().to(list_affiliates))
                .route("", web::post().to(create_affiliate))
                .route("/track", web::post().to(track_click))
                .route("/conversions", web::post().to(create_conversion))
                .route("/me/dashboard", web::get().to(my_dashboard))
                .route("/links/{link_id}", web::get().to(get_link))
                .route("/links/{link_id}", web::delete().to(delete_link))
                .route("/commissions/{commission_id}/approve", web::post().to(approve_commission))
                .route("/commissions/{commission_id}/void", web::post().to(void_commission))
                .route("/payouts/{payout_id}/approve", web::post().to(approve_payout))
                .route("/payouts/{payout_id}/mark-paid", web::post().to(mark_payout_paid))
                .route("/payouts/{payout_id}/reject", web::post().to(reject_payout))
                .route("/{id}", web::get().to(get_affiliate))
                .route("/{id}", web::patch().to(update_affiliate))
                .route("/{id}", web::delete().to(delete_affiliate))
                .route("/{id}/approve", web::post().to(approve_affiliate))
                .route("/{id}/reject", web::post().to(reject_affiliate))
                .route("/{id}/suspend", web::post().to(suspend_affiliate))
                .route("/{id}/reactivate", web::post().to(reactivate_affiliate))
                .route("/{id}/links", web::get().to(list_links))
                .route("/{id}/links", web::post().to(create_link))
                .route("/{id}/commission-rules", web::get().to(list_commission_rules))
                .route("/{id}/commission-rules", web::post().to(create_commission_rule))
                .route(
                    "/{id}/commission-rules/{rule_id}",
                    web::delete().to(delete_commission_rule),
                )
                .route("/{id}/clicks", web::get().to(list_clicks))
                .route("/{id}/conversions", web::get().to(list_conversions))
                .route("/{id}/commissions", web::get().to(list_commissions))
                .route("/{id}/payouts", web::get().to(list_payouts))
                .route("/{id}/payouts", web::post().to(request_payout))
                .route("/{id}/dashboard", web::get().to(get_dashboard)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/affiliates", owner: "affiliates", description: "List affiliates (paginated, filter by status, company_id, user_id, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates", owner: "affiliates", description: "Apply as an affiliate (own user id; a manager may apply for someone else).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}", owner: "affiliates", description: "Get one affiliate (manager or the affiliate themselves).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/affiliates/{id}", owner: "affiliates", description: "Update an affiliate (manage; commission defaults, payout method).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/affiliates/{id}", owner: "affiliates", description: "Delete an affiliate with no conversions, commissions, or payouts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/approve", owner: "affiliates", description: "Approve a pending affiliate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/reject", owner: "affiliates", description: "Reject a pending affiliate (reason optional).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/suspend", owner: "affiliates", description: "Suspend an approved affiliate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/reactivate", owner: "affiliates", description: "Reactivate a suspended affiliate back to approved.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/links", owner: "affiliates", description: "List referral links of an affiliate (manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/links", owner: "affiliates", description: "Create a referral link (code auto-generated when omitted; manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/links/{link_id}", owner: "affiliates", description: "Get one referral link (manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/affiliates/links/{link_id}", owner: "affiliates", description: "Delete a referral link with no clicks (manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/commission-rules", owner: "affiliates", description: "List per-product commission rules of an affiliate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/commission-rules", owner: "affiliates", description: "Create a per-product commission rule (manage; one rule per product).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/affiliates/{id}/commission-rules/{rule_id}", owner: "affiliates", description: "Delete a per-product commission rule (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/track", owner: "affiliates", description: "PUBLIC: record a referral click (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/clicks", owner: "affiliates", description: "List clicks of an affiliate (paginated, filter flagged; manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/conversions", owner: "affiliates", description: "List conversions of an affiliate (paginated, filter kind, flagged; manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/conversions", owner: "affiliates", description: "Record a conversion manually (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/commissions", owner: "affiliates", description: "List commissions of an affiliate (paginated, filter status, flagged; manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/commissions/{commission_id}/approve", owner: "affiliates", description: "Approve a pending commission (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/commissions/{commission_id}/void", owner: "affiliates", description: "Void a pending or approved commission with a reason (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/payouts", owner: "affiliates", description: "List payouts of an affiliate (paginated, filter status; manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/payouts", owner: "affiliates", description: "Request a payout for approved commissions (manager or owner; emits affiliates.payout.requested for billing).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/payouts/{payout_id}/approve", owner: "affiliates", description: "Approve a requested payout (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/payouts/{payout_id}/mark-paid", owner: "affiliates", description: "Mark an approved payout paid and its commissions paid (manage; after billing moved the money).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/affiliates/payouts/{payout_id}/reject", owner: "affiliates", description: "Reject a requested or approved payout and release its commissions (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/dashboard", owner: "affiliates", description: "Affiliate dashboard: clicks, conversions, earnings by commission status, payout history (manager or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/affiliates/me/dashboard", owner: "affiliates", description: "The caller's own affiliate dashboard.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AffiliatesModule::new()));
}
