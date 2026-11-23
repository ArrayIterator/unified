// hosting module — Module definition.
//
// Split from the former single-file module `src/modules/hosting.rs`.
// The module root (`hosting.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{shared_state, HostingState};
use crate::modules::certificates::cert_events::{SSL_RENEWED, SSL_SWAPPED};
use crate::modules::ai;
use super::handlers_plans::{create_plan, delete_plan, get_plan, list_plans, update_plan};
use super::handlers_accounts::{activate_account, cancel_account, create_account, delete_account, get_account, get_account_requirements, list_accounts, suspend_account, unsuspend_account, update_account};
use super::hooks::{observe_domain_verify, observe_ssl_completion, record_order_claim};
use super::ai_capabilities::{AI_CAPABILITIES};

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct HostingModule {
    pub(crate) state: Arc<HostingState>,
}

impl HostingModule {
    pub fn new() -> Self {
        // Shared with the cloud module (see `types::shared_state`):
        // both scopes read and write the same plans and accounts.
        Self {
            state: shared_state(),
        }
    }
}

impl Default for HostingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for HostingModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("hosting", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "hosting"
    }

    fn name(&self) -> &'static str {
        "Hosting Manager"
    }

    fn description(&self) -> &'static str {
        "Hosting plans and accounts: sellable resource bundles linked to store products, accounts bound to a registered domain with SSL state, and a requirements check before activation."
    }

    fn requires(&self) -> &[&'static str] {
        // Store (plans are sold as store products), Domains (accounts
        // are bound to a registered domain; SSL state is surfaced
        // through the domains module and its ssl.renewed /
        // ssl.swapped / ssl.swap events — there is no separate ssl
        // module id), Network
        // (domain syntax checks and the DNS / TLS probe behind domain
        // verification).
        &["store", "domains", "network"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde", "uuid", "chrono", "actix-web", "dashmap", "diesel"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "store.product.published",
            "hosting",
            Arc::new(move |context: &mut HookContext| {
                if let Some(product_id) = context.get("id").and_then(Value::as_str) {
                    state
                        .known_products
                        .insert(product_id.to_string(), true);
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.product.unpublished",
            "hosting",
            Arc::new(move |context: &mut HookContext| {
                if let Some(product_id) = context.get("id").and_then(Value::as_str) {
                    state
                        .known_products
                        .insert(product_id.to_string(), false);
                }
            }),
        );
        // A paid order for a hosting plan becomes a provisioning
        // claim; both the completion event and the payment event are
        // observed so the claim exists as early as possible.
        for event in ["store.order.completed", "order.paid"] {
            let state = self.state.clone();
            bus.register(
                event,
                "hosting",
                Arc::new(move |context: &mut HookContext| {
                    record_order_claim(&state, &context.data);
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            "domain.verify",
            "hosting",
            Arc::new(move |context: &mut HookContext| {
                observe_domain_verify(&state, &context.data);
            }),
        );
        // Only completed SSL events are observed: `ssl.renewed` /
        // `ssl.swapped` (certificate lifecycle completions) and the
        // core `ssl.swap` completion event. The `ssl.renew` request
        // event is deliberately not subscribed: a renewal request is
        // not a completed renewal, and observing it would mark SSL
        // as active before any certificate exists.
        for event in [SSL_RENEWED, SSL_SWAPPED, events::SSL_SWAP] {
            let state = self.state.clone();
            bus.register(
                event,
                "hosting",
                Arc::new(move |context: &mut HookContext| {
                    observe_ssl_completion(&state, &context.data);
                }),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/hosting")
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/accounts", web::get().to(list_accounts))
                .route("/accounts", web::post().to(create_account))
                .route("/accounts/{id}", web::get().to(get_account))
                .route("/accounts/{id}", web::put().to(update_account))
                .route("/accounts/{id}", web::delete().to(delete_account))
                .route(
                    "/accounts/{id}/requirements",
                    web::get().to(get_account_requirements),
                )
                .route(
                    "/accounts/{id}/activate",
                    web::post().to(activate_account),
                )
                .route(
                    "/accounts/{id}/suspend",
                    web::post().to(suspend_account),
                )
                .route(
                    "/accounts/{id}/unsuspend",
                    web::post().to(unsuspend_account),
                )
                .route(
                    "/accounts/{id}/cancel",
                    web::post().to(cancel_account),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/plans",
                owner: "hosting",
                description: "List hosting plans",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/plans",
                owner: "hosting",
                description: "Create a hosting plan",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/plans/{id}",
                owner: "hosting",
                description: "Get one hosting plan",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/hosting/plans/{id}",
                owner: "hosting",
                description: "Update a hosting plan",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/hosting/plans/{id}",
                owner: "hosting",
                description: "Delete a hosting plan",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/accounts",
                owner: "hosting",
                description: "List hosting accounts",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts",
                owner: "hosting",
                description: "Create a hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/accounts/{id}",
                owner: "hosting",
                description: "Get one hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/hosting/accounts/{id}",
                owner: "hosting",
                description: "Update a hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/hosting/accounts/{id}",
                owner: "hosting",
                description: "Delete a pending or cancelled hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/accounts/{id}/requirements",
                owner: "hosting",
                description: "Check a hosting account's activation requirements",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/activate",
                owner: "hosting",
                description: "Activate a hosting account after its requirements pass",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/suspend",
                owner: "hosting",
                description: "Suspend a hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/unsuspend",
                owner: "hosting",
                description: "Unsuspend a hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/cancel",
                owner: "hosting",
                description: "Cancel a hosting account",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HostingModule::new()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookBus;

    #[test]
    fn ssl_hook_subscriptions_observe_completions_not_requests() {
        let module = HostingModule::new();
        let bus = HookBus::new();
        module.register_hooks(&bus);
        assert!(bus.has("ssl.renewed"));
        assert!(bus.has("ssl.swapped"));
        assert!(bus.has("ssl.swap"));
        assert!(!bus.has("ssl.renew"));
    }
}
