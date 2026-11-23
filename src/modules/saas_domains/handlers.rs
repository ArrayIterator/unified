//! SaaS Domains - resolve-debug and settings handlers.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::caller::caller_from;
use super::caller::require_permission;
use super::events::CAP_MANAGE;
use super::events::EVENT_SETTINGS_UPDATED;
use super::requests::ResolveQuery;
use super::requests::UpdateSettingsRequest;
use super::resolve::normalize_host;
use super::resolve::resolve_host;
use super::settings::SaasDomainsSettings;
use super::store::settings;
use super::store::settings_store;

// ---------------------------------------------------------------------------
// Resolve-debug and settings handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_debug(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let host = normalize_host(&query.host);
        let resolution = resolve_host(&ctx, &query.host);
        Ok(json!({
            "host": host,
            "resolved": resolution.is_some(),
            "resolution": resolution,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasDomainsSettings> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        Ok(settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasDomainsSettings> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let mut next = settings();
        if let Some(value) = &body.wildcard_base {
            next.wildcard_base = value.trim().to_lowercase();
        }
        if let Some(value) = &body.main_host {
            next.main_host = value.trim().to_lowercase();
        }
        if let Some(value) = &body.reserved_labels {
            next.reserved_labels = value
                .iter()
                .map(|label| label.trim().to_lowercase())
                .collect();
        }
        if let Some(value) = &body.label_pattern {
            next.label_pattern = value.clone();
        }
        if let Some(value) = body.max_subdomains_per_tenant {
            next.max_subdomains_per_tenant = value;
        }
        if let Some(value) = body.require_subdomain_approval {
            next.require_subdomain_approval = value;
        }
        if let Some(value) = body.default_reservation_ttl_hours {
            next.default_reservation_ttl_hours = value;
        }
        if let Some(value) = &body.alias_target_types {
            next.alias_target_types = value
                .iter()
                .map(|kind| kind.trim().to_lowercase())
                .collect();
        }
        if let Some(value) = body.max_product_subdomains_per_tenant {
            next.max_product_subdomains_per_tenant = value;
        }
        if let Some(value) = body.max_product_subdomains_per_product {
            next.max_product_subdomains_per_product = value;
        }
        if let Some(value) = body.default_product_subdomain_grace_days {
            next.default_product_subdomain_grace_days = value;
        }
        if let Some(value) = body.require_product_subdomain_approval {
            next.require_product_subdomain_approval = value;
        }
        if let Some(value) = &body.product_subdomain_suggestion_suffixes {
            next.product_subdomain_suggestion_suffixes = value
                .iter()
                .map(|suffix| suffix.trim().to_lowercase())
                .collect();
        }
        next.validate()?;
        *settings_store()
            .write()
            .expect("saas_domains settings lock poisoned") = next.clone();
        ctx.hooks.emit(
            EVENT_SETTINGS_UPDATED,
            json!({
                "wildcard_base": next.wildcard_base,
                "main_host": next.main_host,
                "max_subdomains_per_tenant": next.max_subdomains_per_tenant,
                "require_subdomain_approval": next.require_subdomain_approval,
                "default_reservation_ttl_hours": next.default_reservation_ttl_hours,
                "alias_target_types": next.alias_target_types,
                "reserved_labels": next.reserved_labels,
                "max_product_subdomains_per_tenant": next.max_product_subdomains_per_tenant,
                "max_product_subdomains_per_product": next.max_product_subdomains_per_product,
                "default_product_subdomain_grace_days": next.default_product_subdomain_grace_days,
                "require_product_subdomain_approval": next.require_product_subdomain_approval,
                "product_subdomain_suggestion_suffixes": next.product_subdomain_suggestion_suffixes,
            }),
        );
        Ok(next)
    })();
    json::respond(result, debug)
}
