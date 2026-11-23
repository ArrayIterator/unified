// Contracts — Module.

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
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ContractsModule {
    state: ContractsState,
}

impl ContractsModule {
    pub fn new() -> Self {
        Self {
            state: ContractsState::default(),
        }
    }
}

impl Default for ContractsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ContractsModule {
    fn id(&self) -> &'static str {
        "contracts"
    }

    fn name(&self) -> &'static str {
        "Contracts & E-Signature"
    }

    fn description(&self) -> &'static str {
        "Contract templates with placeholders, contracts with a validated status machine, in-app token-based e-signing with signing order and a full audit trail, expiry and renewal alerts, and a billing linkage event when a contract becomes active. In-app signing only: no certified third-party e-signature provider, no qualified certificates, no eIDAS/QES claim; invoices are never created here."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "billing", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "billing", "notifications", "scheduler"]
    }

    /// Contracts is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("contracts", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EXPIRY_ALERT_REQUESTED,
            "contracts",
            Arc::new(move |ctx: &mut HookContext| {
                on_expiry_alert_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/contracts")
                .app_data(state)
                .route("", web::get().to(list_contracts))
                .route("", web::post().to(create_contract))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{id}", web::get().to(get_template))
                .route("/templates/{id}", web::patch().to(update_template))
                .route("/templates/{id}", web::delete().to(delete_template))
                .route("/templates/{id}/render", web::post().to(render_template_handler))
                .route("/sign/{token}", web::get().to(get_signing_link))
                .route("/sign/{token}", web::post().to(sign_via_link))
                .route("/sign/{token}/decline", web::post().to(decline_via_link))
                .route("/{id}", web::get().to(get_contract))
                .route("/{id}", web::patch().to(update_contract))
                .route("/{id}", web::delete().to(delete_contract))
                .route("/{id}/send", web::post().to(send_contract))
                .route("/{id}/activate", web::post().to(activate_contract))
                .route("/{id}/void", web::post().to(void_contract))
                .route("/{id}/terminate", web::post().to(terminate_contract))
                .route("/{id}/check-expiry", web::post().to(check_expiry_contract))
                .route("/{id}/renew", web::post().to(renew_contract))
                .route("/{id}/audit", web::get().to(list_audit))
                .route("/{id}/signers", web::get().to(list_signers))
                .route("/{id}/signers", web::post().to(add_signer))
                .route("/{id}/signers/{signer_id}", web::delete().to(remove_signer))
                .route(
                    "/{id}/signers/{signer_id}/resend",
                    web::post().to(resend_signer_token),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/contracts", owner: "contracts", description: "List contracts (paginated, filter by status, template_id, counterparty_email, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts", owner: "contracts", description: "Create a contract (draft).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/contracts/{id}", owner: "contracts", description: "Get one contract (effective status derived at read time).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/contracts/{id}", owner: "contracts", description: "Update a draft contract only; sealed contracts are rejected (void + renew instead).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/contracts/{id}", owner: "contracts", description: "Delete a draft contract only.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/send", owner: "contracts", description: "Send a draft contract (draft -> sent; at least one signer required; expiry alerts armed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/activate", owner: "contracts", description: "Activate a signed contract (signed -> active; emits billing.contract_activated, no invoice created here).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/void", owner: "contracts", description: "Void a contract with a reason (audited; terminal).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/terminate", owner: "contracts", description: "Terminate a contract early with a reason (audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/check-expiry", owner: "contracts", description: "Materialise expiry now when the end date passed (also driven by the scheduler listener).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/renew", owner: "contracts", description: "Create a new draft contract copying terms, parties, and value of this one.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/contracts/{id}/audit", owner: "contracts", description: "Full audit trail of a contract (paginated, filter by action, actor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/contracts/{id}/signers", owner: "contracts", description: "List signers of a contract (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/signers", owner: "contracts", description: "Add a signer (draft / sent only; returns the raw signing token once).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/contracts/{id}/signers/{signer_id}", owner: "contracts", description: "Remove a pending signer (draft / sent only; a signer who signed cannot be removed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/signers/{signer_id}/resend", owner: "contracts", description: "Issue a fresh signing token (old token stops working; audited; returns the raw token once).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/contracts/sign/{token}", owner: "contracts", description: "TOKEN-GATED: read the contract a signer was asked to sign (first view is audited; unknown/expired tokens answer 404).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/contracts/sign/{token}", owner: "contracts", description: "TOKEN-GATED: sign (requires confirm=true; sequential order enforced; last signature seals the contract).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/contracts/sign/{token}/decline", owner: "contracts", description: "TOKEN-GATED: decline with a reason (audited).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/contracts/templates", owner: "contracts", description: "List contract templates (paginated, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/templates", owner: "contracts", description: "Create a contract template (unknown placeholders in the body are rejected at save).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/contracts/templates/{id}", owner: "contracts", description: "Get one contract template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/contracts/templates/{id}", owner: "contracts", description: "Update a contract template (body and placeholders validated together).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/contracts/templates/{id}", owner: "contracts", description: "Delete a template not used by any contract.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/contracts/templates/{id}/render", owner: "contracts", description: "Render a template with supplied values (values are escaped; missing or unknown placeholders rejected).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/contracts/settings", owner: "contracts", description: "Get expiry alert settings (alert_days offsets).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/contracts/settings", owner: "contracts", description: "Update expiry alert settings (alert_days, descending, 1..365).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ContractsModule::new()));
}
