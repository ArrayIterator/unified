// Domains module - module registration and routes.
//
// Split from `src/modules/domains.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::events;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use super::handlers::{create_domain, create_subdomain, delete_domain, get_domain, list_domains, ssl_renew_handler, update_domain, verify_domain};
use super::service::{apply_ssl_event, run_whitelabel_domain_verification, spawn_verification_worker};
use super::state::remember_runtime;
use super::ai_capabilities::AI_CAPABILITIES;

/// Whitelabel request events this module answers. The constants
/// live in the whitelabel module, which does not expose them, so
/// the event names are spelled out here.
const EVENT_WHITELABEL_DOMAIN_REQUESTED: &str = "whitelabel.domain.requested";
const EVENT_WHITELABEL_SENDER_REQUESTED: &str = "whitelabel.sender.requested";

// Module definition.

pub struct DomainsModule;

impl Module for DomainsModule {
    fn id(&self) -> &'static str {
        "domains"
    }

    fn name(&self) -> &'static str {
        "Domain Manager"
    }

    fn description(&self) -> &'static str {
        "Domain and subdomain management with DNS verification, TLS certificate probing, and SSL renewal interconnection."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("domains", AI_CAPABILITIES);
        // Cache the hook bus and database pool for the synchronous
        // hook handlers, which receive neither: the SSL-event listener
        // persists through the pool and the whitelabel listener emits
        // `domain.verify` outcomes through the bus. `init_all` runs
        // after the pool is attached, so both handles are live here.
        remember_runtime(ctx);
        Ok(())
    }

    fn register_hooks(&self, bus: &unified_core::hooks::HookBus) {
        // A completed renewal (from the SSL manager) carries the domain
        // id/name and the new expiry; our own `ssl-renew` request does
        // not, so it cannot flip the status back prematurely.
        for event in [events::SSL_RENEW, events::SSL_SWAP] {
            bus.register(
                event,
                "domains",
                Arc::new(|context: &mut unified_core::hooks::HookContext| {
                    let domain_id = context
                        .get("domain_id")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<Uuid>().ok());
                    let domain_name = context
                        .get("domain")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let expires_at = context
                        .get("ssl_expires_at")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
                    // Only act on completion payloads, not on requests.
                    if expires_at.is_some() || context.get("completed").is_some() {
                        apply_ssl_event(domain_id, domain_name.as_deref(), expires_at);
                    }
                }),
            );
        }
        // A whitelabel branding write emits `whitelabel.domain
        // .requested` with the requested custom domain. Run the same
        // DNS + TLS verification the HTTP endpoint performs and emit
        // the `domain.verify` outcome, which the whitelabel module
        // already listens to (`on_domain_verify`) to move the
        // branding profile to `verified` / `rejected`. The check runs
        // on a worker because hook handlers are synchronous and the
        // verification is async; the handler answers immediately with
        // a `verification_started` result.
        bus.register(
            EVENT_WHITELABEL_DOMAIN_REQUESTED,
            "domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let domain = context
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default();
                if domain.trim().is_empty() {
                    context.add_result(serde_json::json!({
                        "error": "whitelabel.domain.requested requires 'domain'",
                    }));
                    return;
                }
                context.add_result(serde_json::json!({
                    "status": "verification_started",
                    "domain": domain,
                }));
                spawn_verification_worker(run_whitelabel_domain_verification(domain));
            }),
        );
        // A whitelabel branding write also emits
        // `whitelabel.sender.requested` with the sender name/email.
        // The sender domain is verified the same way: its DNS
        // outcome feeds `domain.verify`, which the whitelabel
        // module consumes for the matching custom domain. There is
        // no separate sender-verified event or sender record in this
        // module; when the sender domain is not a managed domain the
        // verification still runs and its outcome is emitted.
        bus.register(
            EVENT_WHITELABEL_SENDER_REQUESTED,
            "domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let sender_email = context
                    .get("sender_email")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default();
                let domain = sender_email
                    .rsplit_once('@')
                    .map(|(_, domain)| domain.trim().to_string())
                    .unwrap_or_default();
                if domain.is_empty() {
                    context.add_result(serde_json::json!({
                        "error": "whitelabel.sender.requested requires a 'sender_email' with a domain",
                    }));
                    return;
                }
                context.add_result(serde_json::json!({
                    "status": "verification_started",
                    "domain": domain,
                }));
                spawn_verification_worker(run_whitelabel_domain_verification(domain));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/domains", owner: "domains", description: "List domains", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/domains", owner: "domains", description: "Create domain", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/domains/{id}", owner: "domains", description: "Get domain", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/domains/{id}", owner: "domains", description: "Update domain", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/domains/{id}", owner: "domains", description: "Delete domain", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/domains/{id}/verify", owner: "domains", description: "Verify domain", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/domains/{id}/ssl-renew", owner: "domains", description: "Ssl renew handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/domains/{id}/subdomains", owner: "domains", description: "Create subdomain", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/domains")
                .route("", web::get().to(list_domains))
                .route("", web::post().to(create_domain))
                .route("/{id}", web::get().to(get_domain))
                .route("/{id}", web::put().to(update_domain))
                .route("/{id}", web::delete().to(delete_domain))
                .route("/{id}/verify", web::post().to(verify_domain))
                .route("/{id}/ssl-renew", web::post().to(ssl_renew_handler))
                .route("/{id}/subdomains", web::post().to(create_subdomain)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DomainsModule));
}
