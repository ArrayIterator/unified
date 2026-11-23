// Domain Verification — Module definition.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct DomainVerificationModule;

impl Module for DomainVerificationModule {
    fn id(&self) -> &'static str {
        "domain_verification"
    }

    fn name(&self) -> &'static str {
        "Domain Verification"
    }

    fn description(&self) -> &'static str {
        "Custom domain ownership verification through admin-determined DNS records (TXT or CNAME): token issuance with SHA-256 hashed storage, DNS checks via the Network DnsVerifier with settings-driven retries and rate limits, and domain.verification.completed / domain.verification.failed events for the SaaS Domains module."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["domains", "network"]
    }

    fn requires(&self) -> &[&'static str] {
        &["domains", "network"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json", "sha2", "rand"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            EVENT_STATUS_REQUESTED,
            "domain_verification",
            Arc::new(on_status_requested),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/settings", owner: "domain_verification", description: "Get verification settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/domains/verification/settings", owner: "domain_verification", description: "Update verification settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/domains/verification/tokens", owner: "domain_verification", description: "Issue token", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/tokens", owner: "domain_verification", description: "List tokens", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/domains/verification/tokens/{id}", owner: "domain_verification", description: "Revoke token", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/domains/verification/verify", owner: "domain_verification", description: "Verify domain alias", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/attempts", owner: "domain_verification", description: "List attempts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/status", owner: "domain_verification", description: "Get status", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/domains/verification")
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/tokens", web::post().to(issue_token))
                .route("/tokens", web::get().to(list_tokens))
                .route("/tokens/{id}", web::delete().to(revoke_token))
                .route("/verify", web::post().to(verify_domain_alias))
                .route("/attempts", web::get().to(list_attempts))
                .route("/status", web::get().to(get_status)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DomainVerificationModule));
}
