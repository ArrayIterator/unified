//! SaaS Domains - tenant and product-subdomain host resolution.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::HttpRequest;
use chrono::Utc;
use diesel::prelude::*;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use super::events::BLOCKED_RELEASED;
use super::events::BLOCKED_SUSPENDED;
use super::events::BLOCKED_UNVERIFIED;
use super::events::EVENT_PACKAGE_MODULES_REQUESTED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_RESOLVE_SCOPED;
use super::events::EVENT_RESERVATION_EXPIRED;
use super::settings::SaasDomainsSettings;
use super::store::aliases_store;
use super::store::allocations_store;
use super::store::product_subdomains_store;
use super::store::reservations_store;
use super::store::resolved_main_host;
use super::store::set_resolved_main_host;
use super::store::settings;
use super::types::ProductScope;

// ---------------------------------------------------------------------------
// Tenant resolution — the contract the tenant middleware depends on.
// ---------------------------------------------------------------------------

/// Normalize a Host header value: strip any `:port`, lowercase,
/// strip one trailing dot.
pub(crate) fn normalize_host(raw: &str) -> String {
    let host = raw.trim().to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    match host.rsplit_once(':') {
        Some((bare, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            bare.to_string()
        }
        _ => host,
    }
}

/// Main-site host: the configured `saas_domains.main_host` when set,
/// otherwise the host part of the first configured listen address
/// (`app.tcp`), otherwise `localhost`. The fallback result is
/// persisted in the store (see `set_resolved_main_host`) so the
/// hook handler resolves the same control-plane host through
/// `hook_main_host` instead of running its own fallback chain.
fn main_host(ctx: &AppContext, cfg: &SaasDomainsSettings) -> String {
    if !cfg.main_host.trim().is_empty() {
        return cfg.main_host.trim().to_lowercase();
    }
    let config = ctx.config.read().expect("config lock poisoned");
    for listen in &config.app.tcp {
        if let Some((host, _)) = listen.rsplit_once(':') {
            if !host.trim().is_empty() {
                let resolved = host.trim().to_lowercase();
                set_resolved_main_host(&resolved);
                return resolved;
            }
        }
    }
    set_resolved_main_host("localhost");
    "localhost".to_string()
}

/// The control-plane host for callers without an `AppContext`
/// (hook handlers): the configured `main_host` setting, else the
/// main host `resolve_host` resolved and persisted at startup (or
/// on its first call), else `localhost`. This is the same single
/// resolution `resolve_host` uses — the hook path never invents
/// its own fallback chain, so an unset `main_host` with a real
/// listen host can no longer resolve as a tenant.
pub(crate) fn hook_main_host(cfg: &SaasDomainsSettings) -> String {
    if !cfg.main_host.trim().is_empty() {
        return cfg.main_host.trim().to_lowercase();
    }
    let persisted = resolved_main_host();
    if persisted.is_empty() {
        "localhost".to_string()
    } else {
        persisted
    }
}

/// Resolve a host to a tenant. Resolution order (all driven by the
/// configured rules in `SaasDomainsSettings`, in this fixed order):
///   (a) main-site host -> no result (control plane, caller falls through)
///   (b) `<label>.<wildcard_base>` -> active allocation -> tenant result
///   (c) verified custom-domain alias -> tenant result
///   (d) pending/unverified alias or suspended allocation/alias ->
///       `blocked` result (callers must return a clear error and
///       NEVER serve tenant data for a blocked host)
///   (e) otherwise no result.
/// Every lookup is by exact host match only, so resolution can never
/// return one tenant's data for another tenant's host.
pub fn resolve_host(ctx: &AppContext, raw_host: &str) -> Option<Value> {
    let cfg = settings();
    resolve_host_with(raw_host, &cfg, &main_host(ctx, &cfg))
}

/// Shared resolution core: the hook handler and `resolve_host` both
/// run this, so the resolution order and the blocked reasons live in
/// exactly one place. `main_host_value` is the control-plane host
/// the caller resolved (full chain in `resolve_host`; the shared
/// `hook_main_host` resolution in the hook handler, which has no
/// AppContext to read the listen addresses from).
pub(crate) fn resolve_host_with(
    raw_host: &str,
    cfg: &SaasDomainsSettings,
    main_host_value: &str,
) -> Option<Value> {
    let host = normalize_host(raw_host);
    if host.is_empty() {
        return None;
    }
    if host == main_host_value {
        return None;
    }
    // Product subdomains resolve before generic allocations: a host
    // claimed for one product serves only that product. The result
    // keeps kind "subdomain" so the existing tenant middleware
    // accepts it, and carries the product scope in extra fields.
    // A released host inside its grace period is blocked with the
    // new reason `released`; a released host past its grace period
    // returns no result here, so the label is free for reclaim.
    if let Some(result) = resolve_product_subdomain_value(&host) {
        return Some(result);
    }
    if !cfg.wildcard_base.is_empty() {
        let suffix = format!(".{}", cfg.wildcard_base.trim().to_lowercase());
        if let Some(label) = host.strip_suffix(&suffix) {
            if !label.is_empty() && !label.contains('.') {
                let allocation = allocations_store()
                    .iter()
                    .find(|entry| entry.value().hostname == host)
                    .map(|entry| entry.value().clone());
                if let Some(allocation) = allocation {
                    match allocation.status.as_str() {
                        "active" => {
                            return Some(json!({
                                "tenant_id": allocation.company_id,
                                "company_id": allocation.company_id,
                                "site_id": allocation.site_id,
                                "kind": "subdomain",
                                "verified": true,
                                "host": host,
                            }));
                        }
                        "suspended" | "pending_approval" | "rejected" => {
                            let reason = if allocation.status == "suspended" {
                                BLOCKED_SUSPENDED
                            } else {
                                BLOCKED_UNVERIFIED
                            };
                            return Some(json!({
                                "kind": "blocked",
                                "verified": false,
                                "reason": reason,
                                "host": host,
                            }));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let alias = aliases_store()
        .iter()
        .find(|entry| entry.value().domain == host)
        .map(|entry| entry.value().clone());
    if let Some(alias) = alias {
        match alias.status.as_str() {
            "verified" => {
                return Some(json!({
                    "tenant_id": alias.company_id,
                    "company_id": alias.company_id,
                    "site_id": alias.site_id,
                    "kind": "custom_domain",
                    "verified": true,
                    "host": host,
                }));
            }
            "suspended" => {
                return Some(json!({
                    "kind": "blocked",
                    "verified": false,
                    "reason": BLOCKED_SUSPENDED,
                    "host": host,
                }));
            }
            _ => {
                return Some(json!({
                    "kind": "blocked",
                    "verified": false,
                    "reason": BLOCKED_UNVERIFIED,
                    "host": host,
                }));
            }
        }
    }
    None
}

/// Blocked-resolution object shared by the product-subdomain
/// branch. The `reason` is one of the stable wire-contract codes
/// (`unverified_domain`, `suspended`) or the new product code
/// `released` (see `BLOCKED_RELEASED`).
fn blocked_resolution(host: &str, reason: &str) -> Value {
    json!({
        "kind": "blocked",
        "verified": false,
        "reason": reason,
        "host": host,
    })
}

/// Resolve one host against the product-subdomain store only.
/// Returns the tenant result (kind `subdomain` plus product-scope
/// fields), a blocked result, or nothing when the host is not a
/// product subdomain (or is a released one past its grace period).
/// The host matches either the subdomain host or the verified
/// custom domain attached to the record.
fn resolve_product_subdomain_value(host: &str) -> Option<Value> {
    let row = product_subdomains_store().iter().find_map(|entry| {
        let record = entry.value();
        if record.host == host || record.custom_domain.as_deref() == Some(host) {
            Some(record.clone())
        } else {
            None
        }
    })?;
    let is_custom_domain = row.custom_domain.as_deref() == Some(host);
    match row.status.as_str() {
        "active" => {
            if row.verified_at.is_none() {
                return Some(blocked_resolution(host, BLOCKED_UNVERIFIED));
            }
            Some(json!({
                "tenant_id": row.company_id,
                "company_id": row.company_id,
                "site_id": row.site_id,
                "kind": "subdomain",
                "verified": true,
                "host": host,
                "scope": "product",
                "product_id": row.product_id,
                "package_id": row.package_id,
                "product_subdomain_id": row.id,
                "custom_domain": is_custom_domain,
            }))
        }
        "suspended" => Some(blocked_resolution(host, BLOCKED_SUSPENDED)),
        "released" => {
            let in_grace = row.grace_until.map_or(true, |until| until > Utc::now());
            if in_grace {
                Some(blocked_resolution(host, BLOCKED_RELEASED))
            } else {
                None
            }
        }
        _ => Some(blocked_resolution(host, BLOCKED_UNVERIFIED)),
    }
}

/// Resolve a host to its product scope, emitting
/// `saas.product_subdomain.resolve_scoped` when the host resolves
/// to an active product subdomain. This is the entry point the
/// tenant middleware (or its product-scope extension) calls when it
/// needs the scope, not just the tenant: `resolve_host` already
/// carries the same scope fields for the plain tenant path.
pub fn resolve_product_host(ctx: &AppContext, raw_host: &str) -> Option<Value> {
    let host = normalize_host(raw_host);
    let result = resolve_product_subdomain_value(&host)?;
    if result.get("scope").and_then(Value::as_str) == Some("product")
        && result.get("verified").and_then(Value::as_bool) == Some(true)
    {
        ctx.hooks.emit(
            EVENT_PRODUCT_SUBDOMAIN_RESOLVE_SCOPED,
            json!({
                "host": host,
                "company_id": result.get("company_id"),
                "product_id": result.get("product_id"),
                "package_id": result.get("package_id"),
                "product_subdomain_id": result.get("product_subdomain_id"),
            }),
        );
    }
    Some(result)
}

/// The live product scope for a host, if the host is an active,
/// verified product subdomain. Blocked hosts return nothing here;
/// use `resolve_product_subdomain_value` when the blocked reason
/// itself matters.
pub fn product_scope_for_host(raw_host: &str) -> Option<ProductScope> {
    let host = normalize_host(raw_host);
    let row = product_subdomains_store().iter().find_map(|entry| {
        let record = entry.value();
        if record.host == host || record.custom_domain.as_deref() == Some(host.as_str()) {
            Some(record.clone())
        } else {
            None
        }
    })?;
    if row.status != "active" || row.verified_at.is_none() {
        return None;
    }
    Some(ProductScope {
        company_id: row.company_id,
        product_id: row.product_id,
        package_id: row.package_id,
        product_subdomain_id: row.id,
        host,
    })
}

/// True when the scope covers `module_id`: a direct product match,
/// or the scope's package lists the module. Package membership is
/// asked through `saas_packages.package.modules_requested`
/// (`{"package_id"}`; an answer carries `{"module_ids": [...]}`);
/// when nothing answers, only a direct product match counts.
fn product_scope_allows(ctx: &AppContext, scope: &ProductScope, module_id: &str) -> bool {
    if scope.product_id == module_id {
        return true;
    }
    let Some(package_id) = &scope.package_id else {
        return false;
    };
    let answer = ctx.hooks.emit(
        EVENT_PACKAGE_MODULES_REQUESTED,
        json!({ "package_id": package_id }),
    );
    answer.results.iter().any(|result| {
        result
            .get("module_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(module_id)))
    })
}

/// Enforce the product scope for one request. Returns the scope
/// when the request arrived on an active product subdomain that
/// covers `module_id`, `None` when the request did not arrive on a
/// product subdomain at all (main host and generic tenant hosts are
/// unaffected), and an error otherwise: 403 when the product host
/// is blocked (suspended, unverified, or inside its release
/// grace), 404 when the host is scoped to a different product.
/// The 404 is deliberate: it never confirms which product the host
/// does serve. Product-module handlers call this at the top with
/// their own module id; the integrator wires the same check into
/// the tenant middleware (see the integration notes).
pub fn require_product_scope(
    ctx: &AppContext,
    req: &HttpRequest,
    module_id: &str,
) -> UnifiedResult<Option<ProductScope>> {
    let Some(host) = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let normalized = normalize_host(&host);
    let Some(resolution) = resolve_product_subdomain_value(&normalized) else {
        return Ok(None);
    };
    if resolution.get("kind").and_then(Value::as_str) == Some("blocked") {
        let reason = resolution
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or(BLOCKED_UNVERIFIED);
        return Err(UnifiedError::forbidden(format!(
            "Product subdomain host is blocked ({reason}); the product is not served here"
        ))
        .with_metadata("code", Value::String(reason.to_string())));
    }
    let Some(scope) = product_scope_for_host(&normalized) else {
        return Err(UnifiedError::forbidden(
            "Product subdomain host is not active; the product is not served here",
        ));
    };
    if product_scope_allows(ctx, &scope, module_id) {
        Ok(Some(scope))
    } else {
        Err(
            UnifiedError::not_found("This product is not available on this subdomain")
                .with_metadata("code", Value::String("product_scope.mismatch".to_string())),
        )
    }
}

/// Expire every reservation whose `reserved_until` has passed.
/// Returns the expired ids. Called by the scheduler hook handler
/// and by the admin expire endpoint.
pub(crate) fn expire_stale_reservations(ctx: &AppContext) -> Vec<Uuid> {
    let now = Utc::now();
    let mut expired: Vec<Uuid> = Vec::new();
    for mut entry in reservations_store().iter_mut() {
        let row = entry.value_mut();
        if row.status == "reserved" && row.reserved_until <= now {
            row.status = "expired".to_string();
            row.updated_at = now;
            expired.push(row.id);
        }
    }
    for id in &expired {
        if let Some(row) = reservations_store().get(id) {
            ctx.hooks.emit(
                EVENT_RESERVATION_EXPIRED,
                json!({
                    "reservation_id": row.id,
                    "domain": row.domain,
                    "company_id": row.company_id,
                }),
            );
        }
    }
    expired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_host_strips_port_and_trailing_dot() {
        assert_eq!(normalize_host("Example.COM:8443"), "example.com");
        assert_eq!(normalize_host("app.example.com."), "app.example.com");
        assert_eq!(normalize_host("  EXAMPLE.com "), "example.com");
        assert_eq!(normalize_host("example.com:8080"), "example.com");
    }

    #[test]
    fn product_scope_for_unknown_host_is_none() {
        assert!(product_scope_for_host("definitely-not-registered.example.com").is_none());
    }

    #[test]
    fn hook_main_host_resolution_order() {
        // One sequential test: the persisted host is global state,
        // so the order is asserted in a single test, never across
        // parallel tests.
        // Configured setting wins over everything.
        let mut cfg = SaasDomainsSettings::default();
        cfg.main_host = "Example.COM ".to_string();
        assert_eq!(hook_main_host(&cfg), "example.com");
        // Empty setting + persisted resolution -> persisted host.
        let cfg = SaasDomainsSettings::default();
        set_resolved_main_host("listen.example.com");
        assert_eq!(hook_main_host(&cfg), "listen.example.com");
        // Empty setting + nothing persisted -> localhost.
        set_resolved_main_host("");
        assert_eq!(hook_main_host(&cfg), "localhost");
    }
}
