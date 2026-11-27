//! Loyalty - module struct, route table and registration.
//!
//! Part of the `loyalty` module split; see `src/modules/loyalty.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::hooks::HookContext;
use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use unified_core::modules::RouteVersionSupport;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::constants::STORE_DISCOUNT_APPLIED;
use super::constants::STORE_DISCOUNT_FAILED;
use super::constants::STORE_ORDER_COMPLETED;
use super::constants::USER_CREATED;
use super::handlers::activate_program;
use super::handlers::adjust_points;
use super::handlers::cancel_redemption;
use super::handlers::create_earn_rule;
use super::handlers::create_program;
use super::handlers::create_tier;
use super::handlers::deactivate_program;
use super::handlers::delete_earn_rule;
use super::handlers::delete_program;
use super::handlers::delete_tier;
use super::handlers::earn_points;
use super::handlers::expire_points;
use super::handlers::get_balance;
use super::handlers::get_earn_rule;
use super::handlers::get_program;
use super::handlers::get_tier;
use super::handlers::get_tier_for_customer;
use super::handlers::list_earn_rules;
use super::handlers::list_ledger;
use super::handlers::list_programs;
use super::handlers::list_tiers;
use super::handlers::my_balance;
use super::handlers::redeem_points;
use super::handlers::update_earn_rule;
use super::handlers::update_program;
use super::handlers::update_tier;
use super::hooks::on_store_discount_applied;
use super::hooks::on_store_discount_failed;
use super::hooks::on_store_order_completed;
use super::hooks::on_user_created;
use super::types::LoyaltyState;
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct LoyaltyModule {
    state: LoyaltyState,
}

impl LoyaltyModule {
    pub fn new() -> Self {
        Self {
            state: LoyaltyState::default(),
        }
    }
}

impl Default for LoyaltyModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LoyaltyModule {
    fn id(&self) -> &'static str {
        "loyalty"
    }

    fn name(&self) -> &'static str {
        "Loyalty / Rewards"
    }

    fn description(&self) -> &'static str {
        "Customer loyalty and rewards: points programs with earn rules for purchases, referrals, reviews, and signups, tiers by lifetime points or spend, a ledger-backed points balance where every movement is a row, expiry as a ledger movement, and redemptions for order discounts or products. Staff recognition points live in the gamification module with a separate ledger and never mix with these customer points."
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
        &["users", "roles", "store"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("loyalty", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_COMPLETED,
            "loyalty",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            USER_CREATED,
            "loyalty",
            Arc::new(move |ctx: &mut HookContext| on_user_created(Arc::clone(&state), ctx)),
        );
        // Discount confirmation: a discount redemption starts
        // `pending` and is only completed when the store confirms
        // it applied the discount (or cancelled + refunded when
        // the store reports it could not). The store does not emit
        // these events yet; until it does, pending redemptions are
        // cancelled (refunded) by a manager.
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_DISCOUNT_APPLIED,
            "loyalty",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_discount_applied(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_DISCOUNT_FAILED,
            "loyalty",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_discount_failed(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/loyalty")
                .app_data(state)
                .route("/programs", web::get().to(list_programs))
                .route("/programs", web::post().to(create_program))
                .route(
                    "/redemptions/{redemption_id}/cancel",
                    web::post().to(cancel_redemption),
                )
                .route("/programs/{id}", web::get().to(get_program))
                .route("/programs/{id}", web::patch().to(update_program))
                .route("/programs/{id}", web::delete().to(delete_program))
                .route("/programs/{id}/activate", web::post().to(activate_program))
                .route(
                    "/programs/{id}/deactivate",
                    web::post().to(deactivate_program),
                )
                .route("/programs/{id}/earn-rules", web::get().to(list_earn_rules))
                .route(
                    "/programs/{id}/earn-rules",
                    web::post().to(create_earn_rule),
                )
                .route(
                    "/programs/{id}/earn-rules/{rule_id}",
                    web::get().to(get_earn_rule),
                )
                .route(
                    "/programs/{id}/earn-rules/{rule_id}",
                    web::patch().to(update_earn_rule),
                )
                .route(
                    "/programs/{id}/earn-rules/{rule_id}",
                    web::delete().to(delete_earn_rule),
                )
                .route("/programs/{id}/tiers", web::get().to(list_tiers))
                .route("/programs/{id}/tiers", web::post().to(create_tier))
                .route("/programs/{id}/tiers/{tier_id}", web::get().to(get_tier))
                .route(
                    "/programs/{id}/tiers/{tier_id}",
                    web::patch().to(update_tier),
                )
                .route(
                    "/programs/{id}/tiers/{tier_id}",
                    web::delete().to(delete_tier),
                )
                .route("/programs/{id}/earn", web::post().to(earn_points))
                .route("/programs/{id}/adjust", web::post().to(adjust_points))
                .route("/programs/{id}/expire", web::post().to(expire_points))
                .route("/programs/{id}/balance/me", web::get().to(my_balance))
                .route(
                    "/programs/{id}/balance/{customer_id}",
                    web::get().to(get_balance),
                )
                .route(
                    "/programs/{id}/tier/{customer_id}",
                    web::get().to(get_tier_for_customer),
                )
                .route("/programs/{id}/ledger", web::get().to(list_ledger))
                .route("/programs/{id}/redeem", web::post().to(redeem_points)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs",
                owner: "loyalty",
                description: "List loyalty programs (paginated, filter by company_id, active).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs",
                owner: "loyalty",
                description: "Create a loyalty program (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}",
                owner: "loyalty",
                description: "Get one loyalty program.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/loyalty/programs/{id}",
                owner: "loyalty",
                description: "Update a loyalty program (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/loyalty/programs/{id}",
                owner: "loyalty",
                description: "Delete a loyalty program with no ledger rows (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/activate",
                owner: "loyalty",
                description: "Activate a program (deactivates the other active program of the same company; manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/deactivate",
                owner: "loyalty",
                description: "Deactivate a program (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/earn-rules",
                owner: "loyalty",
                description: "List earn rules of a program (paginated, filter kind, active).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/earn-rules",
                owner: "loyalty",
                description: "Create an earn rule (manage; one active rule per kind).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/earn-rules/{rule_id}",
                owner: "loyalty",
                description: "Get one earn rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/loyalty/programs/{id}/earn-rules/{rule_id}",
                owner: "loyalty",
                description: "Update an earn rule (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/loyalty/programs/{id}/earn-rules/{rule_id}",
                owner: "loyalty",
                description: "Delete an earn rule (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/tiers",
                owner: "loyalty",
                description: "List tiers of a program (paginated, ordered by sort_order).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/tiers",
                owner: "loyalty",
                description: "Create a tier (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/tiers/{tier_id}",
                owner: "loyalty",
                description: "Get one tier.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/loyalty/programs/{id}/tiers/{tier_id}",
                owner: "loyalty",
                description: "Update a tier (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/loyalty/programs/{id}/tiers/{tier_id}",
                owner: "loyalty",
                description: "Delete a tier (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/earn",
                owner: "loyalty",
                description: "Award points to a customer (manage; creates a ledger earn row).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/adjust",
                owner: "loyalty",
                description: "Signed points adjustment for a customer (manage; creates a ledger adjust row).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/expire",
                owner: "loyalty",
                description: "Run the expiry sweep for a program (manage; expiry is a ledger row, never a delete).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/balance/{customer_id}",
                owner: "loyalty",
                description: "Balance, lifetime points, lifetime spend, and tier of a customer (manager or the customer).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/balance/me",
                owner: "loyalty",
                description: "The caller's own balance and tier in a program.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/tier/{customer_id}",
                owner: "loyalty",
                description: "Tier evaluation of a customer (manager or the customer).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/loyalty/programs/{id}/ledger",
                owner: "loyalty",
                description: "Ledger rows of a program (paginated, filter customer_id, kind, earn_kind; a customer sees only their own rows).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/programs/{id}/redeem",
                owner: "loyalty",
                description: "Redeem points for an order discount or a product (manager or the customer; a discount redemption starts pending and emits loyalty.discount.requested for the store, completing only on store.discount.applied).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/loyalty/redemptions/{redemption_id}/cancel",
                owner: "loyalty",
                description: "Cancel a pending or completed redemption and refund its points through a ledger row (manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LoyaltyModule::new()));
}
