// bank_transfer - Module registration: struct, defaults, routes, and hooks.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

// Module definition.

pub struct BankTransferModule {
    state: Arc<BankTransferState>,
}

impl BankTransferModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BankTransferState::new()),
        }
    }
}

impl Default for BankTransferModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BankTransferModule {
    fn id(&self) -> &'static str {
        "bank_transfer"
    }

    fn name(&self) -> &'static str {
        "Bank Transfer"
    }

    fn description(&self) -> &'static str {
        "Manual bank transfer payments: admin-configured bank accounts per currency, customer proof upload with amount matching, administrator verification with a full audit trail, and split-bill part verification. Approval emits payment.captured with provider bank_transfer."
    }

    fn requires(&self) -> &[&'static str] {
        &["payments", "store", "billing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde", "serde_json", "chrono", "uuid", "dashmap"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("bank_transfer", AI_CAPABILITIES);
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
            "payment.initiated",
            "bank_transfer",
            Arc::new(move |ctx: &mut HookContext| on_payment_initiated(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "bank_transfer.proof.requested",
            "bank_transfer",
            Arc::new(move |ctx: &mut HookContext| on_proof_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/bank-transfer")
                .route("", web::get().to(get_status))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/accounts", web::get().to(list_accounts))
                .route("/accounts", web::post().to(create_account))
                .route("/accounts/currency/{currency}", web::get().to(list_accounts_for_currency))
                .route("/accounts/{id}", web::get().to(get_account))
                .route("/accounts/{id}", web::put().to(update_account))
                .route("/accounts/{id}", web::delete().to(delete_account))
                .route("/verifications", web::get().to(list_verifications))
                .route("/verifications", web::post().to(create_verification))
                .route("/verifications/{id}", web::get().to(get_verification))
                .route("/verifications/{id}/proof", web::post().to(submit_proof))
                .route("/verifications/{id}/verify", web::post().to(verify_transfer))
                .route("/verifications/{id}/reject", web::post().to(reject_transfer))
                .route("/verifications/{id}/cancel", web::post().to(cancel_verification))
                .route("/verifications/{id}/audit", web::get().to(list_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer", owner: "bank_transfer", description: "Module status: account counts and verifications by status", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/settings", owner: "bank_transfer", description: "Read manual verification settings (amount tolerance, proof expiry, reference requirement)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/bank-transfer/settings", owner: "bank_transfer", description: "Update manual verification settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/accounts", owner: "bank_transfer", description: "List bank accounts (numbers masked)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/accounts", owner: "bank_transfer", description: "Create a bank account for one currency", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/accounts/currency/{currency}", owner: "bank_transfer", description: "List active bank accounts for one currency (checkout picker)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/accounts/{id}", owner: "bank_transfer", description: "Get one bank account (number masked)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/bank-transfer/accounts/{id}", owner: "bank_transfer", description: "Update a bank account", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bank-transfer/accounts/{id}", owner: "bank_transfer", description: "Delete a bank account (refused while open verifications reference it)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/verifications", owner: "bank_transfer", description: "List transfer verifications with pagination and filters", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications", owner: "bank_transfer", description: "Open a transfer verification for an order (or a split part)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/verifications/{id}", owner: "bank_transfer", description: "Get one verification with masked account and full audit trail", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/proof", owner: "bank_transfer", description: "Submit payment proof (file reference, amount, transfer date, reference number)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/verify", owner: "bank_transfer", description: "Approve a transfer (emits payment.captured with provider bank_transfer)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/reject", owner: "bank_transfer", description: "Reject a transfer with a required reason (emits payment.failed, notifies the customer)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/cancel", owner: "bank_transfer", description: "Cancel an open verification", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/verifications/{id}/audit", owner: "bank_transfer", description: "List the audit trail of one verification", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BankTransferModule::new()));
}
