// cards - Module registration: struct, defaults, routes, and hooks.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

// Module.

pub struct CardsModule {
    state: Arc<CardsState>,
}

impl CardsModule {
    pub fn new() -> Self {
        Self { state: Arc::new(CardsState::new()) }
    }
}

impl Default for CardsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CardsModule {
    fn id(&self) -> &'static str {
        "cards"
    }

    fn name(&self) -> &'static str {
        "Card Validation"
    }

    fn description(&self) -> &'static str {
        "Card validation: Luhn checksum, brand detection from standard IIN ranges, expiry and CVV checks, administrator-defined custom brand rules, test-number handling (live/sandbox), and daily validation counts. Stateless: never stores a PAN or CVV; only masked PANs leave the module."
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_validate_requested(state.clone(), ctx));
        bus.register(CARDS_VALIDATE_REQUESTED, "cards", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/cards")
                // Literal segments first: they must win over `/{id}`.
                .route("/validate", web::post().to(validate))
                .route("/brands", web::get().to(list_brands))
                .route("/brands/{id}", web::get().to(get_brand))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/custom-rules", web::get().to(list_custom_rules))
                .route("/custom-rules", web::post().to(create_custom_rule))
                .route("/custom-rules/{id}", web::get().to(get_custom_rule))
                .route("/custom-rules/{id}", web::put().to(update_custom_rule))
                .route("/custom-rules/{id}", web::delete().to(delete_custom_rule))
                .route("/counts", web::get().to(list_counts)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/cards/validate", owner: "cards", description: "Validate a card number (Luhn, brand, length, expiry, CVV, test-number mode); returns the masked PAN and generic reason codes only.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cards/brands", owner: "cards", description: "List standard card brands with lengths, CVV length, active and enabled state for this tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cards/brands/{id}", owner: "cards", description: "Get one standard card brand.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cards/settings", owner: "cards", description: "Get the tenant card validation settings (test mode, expiry bound, GPN, disabled brands).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/cards/settings", owner: "cards", description: "Update the tenant card validation settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cards/custom-rules", owner: "cards", description: "List custom brand rules (paginated; filter by enabled, overrides_standard_brand; search by name or brand).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cards/custom-rules", owner: "cards", description: "Create a custom brand rule (IIN prefixes/ranges, lengths, CVV length, optional standard-brand override).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cards/custom-rules/{id}", owner: "cards", description: "Get one custom brand rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/cards/custom-rules/{id}", owner: "cards", description: "Update a custom brand rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/cards/custom-rules/{id}", owner: "cards", description: "Delete a custom brand rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cards/counts", owner: "cards", description: "List daily validation counts (paginated; filter by date, matched_rule, result). No PANs.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

/// Register the module. The integrator wires this into
/// `modules::register_all` (this file only defines the module; it
/// does not edit `mod.rs`).
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CardsModule::new()));
}
