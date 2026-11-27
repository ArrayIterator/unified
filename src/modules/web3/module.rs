// web3 -- Module struct, Module impl and registration.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::handlers_anchors::*;
use super::handlers_networks::*;
use super::handlers_settings::*;
use super::handlers_wallets::*;
use super::handlers_watched::*;
use super::polling::on_poll_requested;
use super::state::*;
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct Web3Module {
    state: Arc<Web3State>,
}

impl Web3Module {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Web3State::new()),
        }
    }

    /// Access to the module state, for the scheduler wiring that calls
    /// `poll_watched_addresses`.
    pub fn state(&self) -> Arc<Web3State> {
        self.state.clone()
    }
}

impl Default for Web3Module {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Web3Module {
    fn id(&self) -> &'static str {
        "web3"
    }

    fn name(&self) -> &'static str {
        "Web3 / Blockchain"
    }

    fn description(&self) -> &'static str {
        "Blockchain openness and traceability: watched addresses, on-chain transaction history, tamper-evident record anchoring, a public transparency ledger, read-only wallet linking, and read-only balances across EVM, Stellar, and Cardano networks."
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &[
            "reqwest",
            "serde_json",
            "sha2",
            "hmac",
            "base64",
            "async-trait",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some(ProductCategory::Infrastructure.as_str())
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = WEB3_CTX.set(stash_ctx(ctx));
        ai::register_ai_capabilities("web3", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "web3.poll_requested",
            "web3",
            Arc::new(move |ctx: &mut HookContext| on_poll_requested(state.clone(), ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/networks",
                owner: "web3",
                description: "List chain networks",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/networks",
                owner: "web3",
                description: "Create chain network (admin)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/networks/{id}",
                owner: "web3",
                description: "Get chain network",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/web3/networks/{id}",
                owner: "web3",
                description: "Update chain network (admin)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/web3/networks/{id}",
                owner: "web3",
                description: "Delete chain network (admin)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/networks/{id}/status",
                owner: "web3",
                description: "Network status (latest block, sync state)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/networks/{id}/fee-estimate",
                owner: "web3",
                description: "Fee estimate for a network",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/watched-addresses",
                owner: "web3",
                description: "List watched addresses",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/watched-addresses",
                owner: "web3",
                description: "Watch an address",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/web3/watched-addresses/{id}",
                owner: "web3",
                description: "Stop watching an address",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/watched-addresses/{id}/poll",
                owner: "web3",
                description: "Poll one watched address",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/poll",
                owner: "web3",
                description: "Poll all watched addresses",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/transactions",
                owner: "web3",
                description: "List observed transactions",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/transactions/{id}",
                owner: "web3",
                description: "Get one observed transaction",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/anchors",
                owner: "web3",
                description: "Request an on-chain anchor for a record hash",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/anchors",
                owner: "web3",
                description: "List anchors",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/anchors/{id}",
                owner: "web3",
                description: "Get one anchor",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/anchors/{id}/submit",
                owner: "web3",
                description: "Submit a pending anchor to the anchoring relay",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/anchors/{id}/refresh",
                owner: "web3",
                description: "Refresh an anchor against the chain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/anchors/verify",
                owner: "web3",
                description: "Verify a record against its anchor",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/ledger",
                owner: "web3",
                description: "Transparency ledger (authenticated)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/public/ledger",
                owner: "web3",
                description: "Public transparency ledger (default-deny, admin-enabled)",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/wallets",
                owner: "web3",
                description: "List linked wallets",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/wallets",
                owner: "web3",
                description: "Link a wallet address (read-only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/wallets/{id}",
                owner: "web3",
                description: "Get one linked wallet",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/web3/wallets/{id}",
                owner: "web3",
                description: "Unlink a wallet",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/wallets/{id}/challenge",
                owner: "web3",
                description: "Issue a wallet ownership challenge",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/wallets/{id}/verify",
                owner: "web3",
                description: "Verify wallet ownership from a challenge proof",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/wallets/{id}/balances",
                owner: "web3",
                description: "List balance snapshots for a wallet",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/web3/wallets/{id}/balances/refresh",
                owner: "web3",
                description: "Refresh balances for a wallet",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/web3/settings",
                owner: "web3",
                description: "Get web3 settings (admin)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/web3/settings",
                owner: "web3",
                description: "Update web3 settings (admin)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/web3")
                .route("/networks", web::get().to(list_networks))
                .route("/networks", web::post().to(create_network))
                .route("/networks/{id}", web::get().to(get_network))
                .route("/networks/{id}", web::patch().to(update_network))
                .route("/networks/{id}", web::delete().to(delete_network))
                .route("/networks/{id}/status", web::get().to(network_status))
                .route(
                    "/networks/{id}/fee-estimate",
                    web::get().to(network_fee_estimate),
                )
                .route("/watched-addresses", web::get().to(list_watched_addresses))
                .route("/watched-addresses", web::post().to(create_watched_address))
                .route(
                    "/watched-addresses/{id}",
                    web::delete().to(delete_watched_address),
                )
                .route(
                    "/watched-addresses/{id}/poll",
                    web::post().to(poll_watched_address),
                )
                .route("/poll", web::post().to(poll_all_watched))
                .route("/transactions", web::get().to(list_transactions))
                .route("/transactions/{id}", web::get().to(get_transaction))
                .route("/anchors", web::post().to(create_anchor))
                .route("/anchors", web::get().to(list_anchors))
                .route("/anchors/{id}", web::get().to(get_anchor))
                .route("/anchors/{id}/submit", web::post().to(submit_anchor))
                .route("/anchors/{id}/refresh", web::post().to(refresh_anchor))
                .route("/anchors/verify", web::post().to(verify_anchor))
                .route("/ledger", web::get().to(list_ledger))
                .route("/public/ledger", web::get().to(public_ledger))
                .route("/wallets", web::get().to(list_wallets))
                .route("/wallets", web::post().to(link_wallet))
                .route("/wallets/{id}", web::get().to(get_wallet))
                .route("/wallets/{id}", web::delete().to(unlink_wallet))
                .route("/wallets/{id}/challenge", web::post().to(challenge_wallet))
                .route("/wallets/{id}/verify", web::post().to(verify_wallet))
                .route(
                    "/wallets/{id}/balances",
                    web::get().to(list_wallet_balances),
                )
                .route(
                    "/wallets/{id}/balances/refresh",
                    web::post().to(refresh_wallet_balances),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(Web3Module::new()));
}
