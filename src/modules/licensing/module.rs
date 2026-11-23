//! Licensing - Module definition: routes, hooks, and registration.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use crate::modules::ai;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct LicensingModule;

impl Module for LicensingModule {
    fn id(&self) -> &'static str {
        "licensing"
    }

    fn name(&self) -> &'static str {
        "Licensing"
    }

    fn description(&self) -> &'static str {
        "License key management: generation, validation, activation seats, suspension, revocation, and expiry alerts. Licensing as a service: per-product key formats and settings, API-key authenticated verification, activation history and seat management, grace periods and renewal reminders, upgrades, offline tokens, git/API/file distribution channels, and a hash-chained license event log."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("licensing", AI_CAPABILITIES);
        let _ = LICENSING_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            "license.validate_requested",
            "licensing",
            Arc::new(on_license_validate_requested),
        );
        bus.register(
            LICENSE_EXPIRY_CHECK_REQUESTED,
            "licensing",
            Arc::new(on_license_expiry_check_requested),
        );
        bus.register("order.created", "licensing", Arc::new(on_order_created));
        bus.register(
            "store.order.refunded",
            "licensing",
            Arc::new(on_order_refunded),
        );
        bus.register(
            STORE_ORDER_UPGRADED,
            "licensing",
            Arc::new(on_order_upgraded),
        );
        bus.register(
            BILLING_PLAN_CHANGED,
            "licensing",
            Arc::new(on_order_upgraded),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses", owner: "licensing", description: "List licenses (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses", owner: "licensing", description: "Create license (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/validate", owner: "licensing", description: "Validate handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/activate", owner: "licensing", description: "Activate handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/deactivate", owner: "licensing", description: "Deactivate handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}", owner: "licensing", description: "Get license (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/licensing/licenses/{id}", owner: "licensing", description: "Update license (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/licensing/licenses/{id}", owner: "licensing", description: "Delete license (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/suspend", owner: "licensing", description: "Suspend handler (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/expiry-check", owner: "licensing", description: "Run the license expiry scan now (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/revoke", owner: "licensing", description: "Revoke handler (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/activations", owner: "licensing", description: "List activations (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            // Licensing as a service (batch 7).
            RouteInfo { method: "GET", path: "/api/v1/licensing/license-products", owner: "licensing", description: "List license products (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/license-products", owner: "licensing", description: "Create license product (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/license-products/{id}", owner: "licensing", description: "Get license product (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/licensing/license-products/{id}", owner: "licensing", description: "Update license product (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/licensing/license-products/{id}", owner: "licensing", description: "Delete license product (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/public/verify", owner: "licensing", description: "Public license verification (API key, licensing:read)", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/activation-history", owner: "licensing", description: "List activation history (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/activations/{activation_id}/deactivate", owner: "licensing", description: "Remotely deactivate one activation (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/seats", owner: "licensing", description: "Change license seat count (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/events", owner: "licensing", description: "List hash-chained license events (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/events/verify", owner: "licensing", description: "Verify the hash-chained license event log (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/offline-token", owner: "licensing", description: "Issue a signed offline token (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/offline-verify", owner: "licensing", description: "Verify a signed offline token without a database lookup (token is the credential)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/offline-tokens", owner: "licensing", description: "List offline tokens (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/offline-tokens/{token_id}/revoke", owner: "licensing", description: "Revoke an offline token server-side (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/distributions", owner: "licensing", description: "List git distributions (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/distributions", owner: "licensing", description: "Create git distribution (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/distributions/{id}", owner: "licensing", description: "Get git distribution (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/licensing/distributions/{id}", owner: "licensing", description: "Update git distribution (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/licensing/distributions/{id}", owner: "licensing", description: "Delete git distribution (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/distribute/git/{product}/access", owner: "licensing", description: "Confirm a download is licensed for a git-distributed product (API key, licensing:read)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/licensing/distribute/git/{product}/webhook", owner: "licensing", description: "Git provider webhook receiver (provider HMAC signature)", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/licensing/products/{product_id}/releases", owner: "licensing", description: "Upload a release for a licensed product (admin, licensing:manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/products/{product_id}/releases", owner: "licensing", description: "List releases for a licensed product (admin, licensing:view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/distribute/releases/{release_id}/download", owner: "licensing", description: "Issue a signed download link for a release (valid license required)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/licensing/distribute/releases/{release_id}/file", owner: "licensing", description: "Serve a release through a signed download link (HMAC token)", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/licensing")
                .route("/licenses", web::get().to(list_licenses))
                .route("/licenses", web::post().to(create_license))
                .route("/validate", web::post().to(validate_handler))
                .route("/activate", web::post().to(activate_handler))
                .route("/deactivate", web::post().to(deactivate_handler))
                .route("/licenses/{id}", web::get().to(get_license))
                .route("/licenses/{id}", web::put().to(update_license))
                .route("/licenses/{id}", web::delete().to(delete_license))
                .route("/licenses/{id}/suspend", web::post().to(suspend_handler))
                .route("/licenses/{id}/revoke", web::post().to(revoke_handler))
                .route("/licenses/expiry-check", web::post().to(expiry_check_handler))
                .route(
                    "/licenses/{id}/activations",
                    web::get().to(list_activations),
                )
                .route("/license-products", web::get().to(list_license_products))
                .route(
                    "/license-products",
                    web::post().to(create_license_product),
                )
                .route(
                    "/license-products/{id}",
                    web::get().to(get_license_product),
                )
                .route(
                    "/license-products/{id}",
                    web::put().to(update_license_product),
                )
                .route(
                    "/license-products/{id}",
                    web::delete().to(delete_license_product),
                )
                .route("/public/verify", web::post().to(public_verify_handler))
                .route(
                    "/licenses/{id}/activation-history",
                    web::get().to(list_activation_history),
                )
                .route(
                    "/licenses/{id}/activations/{activation_id}/deactivate",
                    web::post().to(deactivate_activation),
                )
                .route("/licenses/{id}/seats", web::post().to(update_seats))
                .route(
                    "/licenses/{id}/events",
                    web::get().to(list_license_events),
                )
                .route(
                    "/licenses/{id}/events/verify",
                    web::get().to(verify_license_events),
                )
                .route(
                    "/licenses/{id}/offline-token",
                    web::post().to(issue_offline_token),
                )
                .route("/offline-verify", web::post().to(offline_verify_handler))
                .route(
                    "/licenses/{id}/offline-tokens",
                    web::get().to(list_offline_tokens),
                )
                .route(
                    "/licenses/{id}/offline-tokens/{token_id}/revoke",
                    web::post().to(revoke_offline_token),
                )
                .route("/distributions", web::get().to(list_distributions))
                .route("/distributions", web::post().to(create_distribution))
                .route(
                    "/distributions/{id}",
                    web::get().to(get_distribution),
                )
                .route(
                    "/distributions/{id}",
                    web::put().to(update_distribution),
                )
                .route(
                    "/distributions/{id}",
                    web::delete().to(delete_distribution),
                )
                .route(
                    "/distribute/git/{product}/access",
                    web::get().to(git_access_handler),
                )
                .route(
                    "/distribute/git/{product}/webhook",
                    web::post().to(git_webhook_handler),
                )
                .route(
                    "/products/{product_id}/releases",
                    web::post().to(create_release),
                )
                .route(
                    "/products/{product_id}/releases",
                    web::get().to(list_releases),
                )
                .route(
                    "/distribute/releases/{release_id}/download",
                    web::get().to(download_release),
                )
                .route(
                    "/distribute/releases/{release_id}/file",
                    web::get().to(download_release_file),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LicensingModule));
}
