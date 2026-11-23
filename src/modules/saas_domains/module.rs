//! SaaS Domains - module struct, route table and registration.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::hooks::HookBus;
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::MODULE_ENTITLEMENT_DENIED_EVENT;
use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use unified_core::modules::RouteVersionSupport;
use actix_web::web;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::events::EVENT_AI_SUGGEST_SUBDOMAIN_REQUESTED;
use super::events::EVENT_ALIAS_LOOKUP;
use super::events::EVENT_DOMAIN_VERIFICATION_COMPLETED;
use super::events::EVENT_PACKAGE_CANCELLED;
use super::events::EVENT_PACKAGE_PURCHASE_COMPLETED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_EXPIRE_GRACE;
use super::events::EVENT_RESERVATIONS_EXPIRE;
use super::events::EVENT_SITE_PROVISION_REQUESTED;
use super::events::EVENT_SUBDOMAIN_RELEASE_REQUESTED;
use super::events::EVENT_TENANT_RESOLVE_REQUESTED;
use super::events::EVENT_WHITELABEL_DOMAIN_VERIFIED;
use super::handlers::get_settings;
use super::handlers::resolve_debug;
use super::handlers::update_settings;
use super::handlers_aliases::create_alias;
use super::handlers_aliases::delete_alias;
use super::handlers_aliases::get_alias;
use super::handlers_aliases::list_aliases;
use super::handlers_aliases::release_alias;
use super::handlers_aliases::resume_alias;
use super::handlers_aliases::suspend_alias;
use super::handlers_aliases::update_alias;
use super::handlers_reservations::create_reservation;
use super::handlers_reservations::expire_reservations_endpoint;
use super::handlers_reservations::get_reservation;
use super::handlers_reservations::list_reservations;
use super::handlers_reservations::provision_reservation;
use super::handlers_reservations::release_reservation;
use super::handlers_subdomains::approve_subdomain;
use super::handlers_subdomains::create_subdomain;
use super::handlers_subdomains::delete_subdomain;
use super::handlers_subdomains::get_subdomain;
use super::handlers_subdomains::list_subdomains;
use super::handlers_subdomains::reject_subdomain;
use super::handlers_subdomains::release_subdomain;
use super::handlers_subdomains::resume_subdomain;
use super::handlers_subdomains::suspend_subdomain;
use super::hooks::answer_ai_suggest_subdomain;
use super::hooks::apply_entitlement_revoked;
use super::hooks::apply_package_cancelled;
use super::hooks::apply_product_verification_completed;
use super::hooks::apply_tenant_destroy_release;
use super::hooks::apply_verification_completed;
use super::hooks::apply_whitelabel_domain_verified;
use super::hooks::claim_product_subdomain_from_event;
use super::hooks::claim_tenant_subdomain_from_event;
use super::hooks::expire_stale_reservations_hook;
use super::product_subdomains::activate_product_subdomain;
use super::product_subdomains::check_product_subdomain;
use super::product_subdomains::claim_product_subdomain;
use super::product_subdomains::expire_product_subdomains;
use super::product_subdomains::finalize_expired_graces_hook;
use super::product_subdomains::get_product_subdomain;
use super::product_subdomains::list_product_subdomains;
use super::product_subdomains::release_product_subdomain;
use super::product_subdomains::suspend_product_subdomain;
use super::product_subdomains::verify_product_subdomain;
use super::resolve::hook_main_host;
use super::resolve::resolve_host_with;
use super::store::aliases_store;
use super::store::settings;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct SaasDomainsModule;

impl Module for SaasDomainsModule {
    fn id(&self) -> &'static str {
        "saas_domains"
    }

    fn name(&self) -> &'static str {
        "SaaS Domain Management"
    }

    fn description(&self) -> &'static str {
        "Preregister domain reservations, custom domain aliases, tenant subdomain allocation, per-product subdomains with entitlement-gated claims and product-scoped tenant resolution, and tenant resolution for SaaS sites."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["domains", "saas", "company"]
    }

    fn requires(&self) -> &[&'static str] {
        &["domains", "saas", "company"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Tenant resolution (middleware contract): answer with the
        // resolution object, or answer nothing so the caller falls
        // through to its next resolver. Exact host match only.
        bus.register(
            EVENT_TENANT_RESOLVE_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let Some(host) = context.get("host").and_then(Value::as_str) else {
                    return;
                };
                // Hook handlers have no AppContext, so the
                // control-plane host here comes from the shared
                // `hook_main_host` resolution: the configured
                // saas_domains.main_host setting, else the main
                // host `resolve_host` resolved from the listen
                // addresses and persisted, else "localhost". It is
                // the same single resolution `resolve_host` uses,
                // never a second fallback chain.
                let cfg = settings();
                let main = hook_main_host(&cfg);
                if let Some(result) = resolve_host_with(host, &cfg, &main) {
                    context.add_result(result);
                }
            }),
        );

        // Alias lookup by domain or id.
        bus.register(
            EVENT_ALIAS_LOOKUP,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let by_id = context
                    .get("alias_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok());
                let by_domain = context
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_lowercase());
                let found = aliases_store().iter().find_map(|entry| {
                    let row = entry.value();
                    let matches = by_id.map_or(false, |id| row.id == id)
                        || by_domain
                            .as_deref()
                            .map_or(false, |domain| row.domain == domain);
                    if matches { Some(row.clone()) } else { None }
                });
                if let Some(alias) = found {
                    if let Ok(value) = serde_json::to_value(&alias) {
                        context.add_result(value);
                    }
                }
            }),
        );

        // Scheduler sweep: expire past-due reservations.
        bus.register(
            EVENT_RESERVATIONS_EXPIRE,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let expired = expire_stale_reservations_hook();
                context.add_result(json!({
                    "expired": expired.len(),
                    "ids": expired,
                }));
            }),
        );

        // Verification completion flips a pending alias to verified.
        bus.register(
            EVENT_DOMAIN_VERIFICATION_COMPLETED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                // Domain names are case-insensitive: normalise
                // before passing the name in, so a mixed-case event
                // domain still matches the stored lowercase domain.
                let domain = context
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_lowercase());
                let domain = domain.as_deref();
                let alias_id = context
                    .get("alias_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok());
                let verified = context
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                apply_verification_completed(domain, alias_id, verified);
            }),
        );

        // Verification completion also flips a matching product
        // subdomain to verified. The verification module sends
        // `status: "verified"` (no `verified` boolean); both shapes
        // are accepted here.
        bus.register(
            EVENT_DOMAIN_VERIFICATION_COMPLETED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                // Domain names are case-insensitive: normalise
                // before passing the name in (see the alias listener
                // above).
                let domain = context
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_lowercase());
                let domain = domain.as_deref();
                let product_subdomain_id = context
                    .get("product_subdomain_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok());
                let status = context.get("status").and_then(Value::as_str);
                let verified = context
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                apply_product_verification_completed(
                    domain,
                    product_subdomain_id,
                    status,
                    verified,
                );
            }),
        );

        // Package purchase completed: claim intent only when the
        // payload attests entitlement was validated
        // (`entitlement_verified: true`) and names a subdomain
        // label. Anything else is skipped; the dashboard claim
        // endpoint is the normal path.
        bus.register(
            EVENT_PACKAGE_PURCHASE_COMPLETED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                if context.get("entitlement_verified").and_then(Value::as_bool) != Some(true) {
                    return;
                }
                let Some(company_id) = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    return;
                };
                let Some(product_id) = context
                    .get("product_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("module_id").and_then(Value::as_str))
                else {
                    return;
                };
                let Some(label) = context.get("subdomain").and_then(Value::as_str) else {
                    return;
                };
                let package_id = context.get("package_id").and_then(Value::as_str);
                let claimed_by = context
                    .get("buyer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("system:purchase");
                if let Some(id) = claim_product_subdomain_from_event(
                    company_id, product_id, package_id, label, claimed_by,
                ) {
                    context.add_result(json!({
                        "product_subdomain_id": id,
                        "claimed": true,
                    }));
                }
            }),
        );

        // Package cancelled: start release (grace) for the
        // package's product subdomains.
        bus.register(
            EVENT_PACKAGE_CANCELLED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let Some(company_id) = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    return;
                };
                let Some(package_id) = context.get("package_id").and_then(Value::as_str) else {
                    return;
                };
                apply_package_cancelled(company_id, package_id);
            }),
        );

        // Entitlement revoked: suspend the affected product
        // subdomains so a revoked purchase stops serving. Both the
        // license event and the module-entitlement denial are
        // listened to; a payload without a tenant scope is skipped.
        for event in [events::LICENSE_REVOKED, MODULE_ENTITLEMENT_DENIED_EVENT] {
            bus.register(
                event,
                "saas_domains",
                Arc::new(move |context: &mut unified_core::hooks::HookContext| {
                    let company_id = context
                        .get("company_id")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<Uuid>().ok());
                    let product_id = context
                        .get("product_id")
                        .and_then(Value::as_str)
                        .or_else(|| context.get("module_id").and_then(Value::as_str))
                        .or_else(|| context.get("product").and_then(Value::as_str));
                    let package_id = context.get("package_id").and_then(Value::as_str);
                    apply_entitlement_revoked(company_id, product_id, package_id);
                }),
            );
        }

        // Site-provision request: claim intent from a provisioning
        // flow. Two trusted sources, each validated by its own
        // module before emitting:
        //   - `saas_packages`: a PRODUCT subdomain claim (requires
        //     `product_id`; that module validates entitlements in
        //     its provisioning step before emitting). This closes
        //     the open item the packages module records ("the
        //     domains flow has no provision-request listener
        //     today").
        //   - `tenants`: a TENANT-level subdomain claim (requires
        //     the tenant id; no `product_id`). The tenants module
        //     emits it only from its provisioning pipeline, for a
        //     tenant it created under a package; the claim is a
        //     generic allocation, not a product subdomain. Only an
        //     `active` allocation counts as claimed: a
        //     `pending_approval` allocation is created (or found)
        //     but reported unclaimed, so the tenant keeps
        //     `subdomain_pending: true` until an admin approves it.
        // Any other source is declined with `subdomain_pending:
        // true` and a reason, never claimed.
        bus.register(
            EVENT_SITE_PROVISION_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let source = context.get("source").and_then(Value::as_str);
                if source == Some("tenants") {
                    let Some(company_id) = context
                        .get("company_id")
                        .and_then(Value::as_str)
                        .or_else(|| context.get("tenant_id").and_then(Value::as_str))
                        .and_then(|value| value.parse::<Uuid>().ok())
                    else {
                        context.add_result(json!({
                            "claimed": false,
                            "subdomain_pending": true,
                            "reason": "provision request carries no company_id or tenant_id",
                        }));
                        return;
                    };
                    let Some(label) = context
                        .get("subdomain")
                        .and_then(Value::as_str)
                        .or_else(|| context.get("label").and_then(Value::as_str))
                    else {
                        context.add_result(json!({
                            "claimed": false,
                            "subdomain_pending": true,
                            "reason": "provision request carries no subdomain label",
                        }));
                        return;
                    };
                    match claim_tenant_subdomain_from_event(company_id, label) {
                        Some((id, status)) if status == "active" => {
                            context.add_result(json!({
                                "allocation_id": id,
                                "claimed": true,
                                "subdomain_pending": false,
                                "status": status,
                            }));
                        }
                        Some((id, status)) => {
                            context.add_result(json!({
                                "allocation_id": id,
                                "claimed": false,
                                "subdomain_pending": true,
                                "status": status,
                                "reason": "subdomain allocation created but pending approval",
                            }));
                        }
                        None => {
                            context.add_result(json!({
                                "claimed": false,
                                "subdomain_pending": true,
                                "reason": "label unavailable, quota reached, or wildcard base unconfigured",
                            }));
                        }
                    }
                    return;
                }
                if source != Some("saas_packages") {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request source is not a trusted provisioning module",
                    }));
                    return;
                }
                let Some(company_id) = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request carries no company_id",
                    }));
                    return;
                };
                let Some(product_id) = context
                    .get("product_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("module_id").and_then(Value::as_str))
                else {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request carries no product_id",
                    }));
                    return;
                };
                let Some(label) = context.get("subdomain").and_then(Value::as_str) else {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request carries no subdomain label",
                    }));
                    return;
                };
                let package_id = context.get("package_id").and_then(Value::as_str);
                match claim_product_subdomain_from_event(
                    company_id,
                    product_id,
                    package_id,
                    label,
                    "system:saas_packages",
                ) {
                    Some(id) => {
                        context.add_result(json!({
                            "product_subdomain_id": id,
                            "claimed": true,
                            "subdomain_pending": false,
                        }));
                    }
                    None => {
                        context.add_result(json!({
                            "claimed": false,
                            "subdomain_pending": true,
                            "reason": "label unavailable, quota reached, or record already exists",
                        }));
                    }
                }
            }),
        );

        // Tenant-destroy revoke: release every subdomain of the
        // destroyed tenant (generic allocations immediately,
        // product subdomains into their release grace period).
        // The destroy flow already verified the schema drop and
        // stopped serving at suspension, so this listener only
        // moves records; it never serves or blocks traffic itself.
        bus.register(
            EVENT_SUBDOMAIN_RELEASE_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let Some(company_id) = context
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("company_id").and_then(Value::as_str))
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    return;
                };
                let (allocations, products) = apply_tenant_destroy_release(company_id);
                context.add_result(json!({
                    "tenant_id": company_id,
                    "released_allocations": allocations,
                    "released_product_subdomains": products,
                }));
            }),
        );

        // Whitelabel verified custom domain: attach it to the
        // tenant's product subdomain, inheriting the product scope.
        bus.register(
            EVENT_WHITELABEL_DOMAIN_VERIFIED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let Some(company_id) = context.get("company_id").and_then(Value::as_str) else {
                    return;
                };
                let Some(domain) = context.get("domain").and_then(Value::as_str) else {
                    return;
                };
                apply_whitelabel_domain_verified(company_id, domain);
            }),
        );

        // AI capability hook: suggest available labels for a
        // product name. Suggestions only; nothing is reserved.
        bus.register(
            EVENT_AI_SUGGEST_SUBDOMAIN_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                answer_ai_suggest_subdomain(context);
            }),
        );

        // Scheduler sweep: finalize releases whose grace ended.
        bus.register(
            EVENT_PRODUCT_SUBDOMAIN_EXPIRE_GRACE,
            "saas_domains",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let finalized = finalize_expired_graces_hook();
                context.add_result(json!({
                    "finalized": finalized.len(),
                    "ids": finalized,
                }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/reservations",
                owner: "saas_domains",
                description: "Create domain reservation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/reservations",
                owner: "saas_domains",
                description: "List domain reservations",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/reservations/{id}",
                owner: "saas_domains",
                description: "Get domain reservation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/reservations/{id}/release",
                owner: "saas_domains",
                description: "Release domain reservation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/reservations/{id}/provision",
                owner: "saas_domains",
                description: "Provision reserved domain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/reservations/expire",
                owner: "saas_domains",
                description: "Expire stale reservations",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/aliases",
                owner: "saas_domains",
                description: "Create custom domain alias",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/aliases",
                owner: "saas_domains",
                description: "List custom domain aliases",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/aliases/{id}",
                owner: "saas_domains",
                description: "Get custom domain alias",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/saas/domains/aliases/{id}",
                owner: "saas_domains",
                description: "Update custom domain alias",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/aliases/{id}/suspend",
                owner: "saas_domains",
                description: "Suspend custom domain alias",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/aliases/{id}/resume",
                owner: "saas_domains",
                description: "Resume suspended custom domain alias (admin only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/aliases/{id}/release",
                owner: "saas_domains",
                description: "Release custom domain alias",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/saas/domains/aliases/{id}",
                owner: "saas_domains",
                description: "Delete custom domain alias",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/subdomains",
                owner: "saas_domains",
                description: "Allocate tenant subdomain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains/check",
                owner: "saas_domains",
                description: "Check availability of a product-subdomain label",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains",
                owner: "saas_domains",
                description: "Claim a product subdomain for a purchased product or package",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/product-subdomains",
                owner: "saas_domains",
                description: "List product subdomains",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/product-subdomains/{id}",
                owner: "saas_domains",
                description: "Get product subdomain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains/{id}/verify",
                owner: "saas_domains",
                description: "Mark product subdomain verified after domain verification confirms it",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains/{id}/activate",
                owner: "saas_domains",
                description: "Activate a verified product subdomain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains/{id}/suspend",
                owner: "saas_domains",
                description: "Suspend a product subdomain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains/{id}/release",
                owner: "saas_domains",
                description: "Start release of a product subdomain (grace period first)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/product-subdomains/expire",
                owner: "saas_domains",
                description: "Finalize product-subdomain releases whose grace period ended",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/subdomains",
                owner: "saas_domains",
                description: "List subdomain allocations",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/subdomains/{id}",
                owner: "saas_domains",
                description: "Get subdomain allocation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/subdomains/{id}/approve",
                owner: "saas_domains",
                description: "Approve subdomain allocation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/subdomains/{id}/reject",
                owner: "saas_domains",
                description: "Reject subdomain allocation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/subdomains/{id}/suspend",
                owner: "saas_domains",
                description: "Suspend subdomain allocation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/subdomains/{id}/resume",
                owner: "saas_domains",
                description: "Resume suspended subdomain allocation (admin only)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/saas/domains/subdomains/{id}/release",
                owner: "saas_domains",
                description: "Release subdomain allocation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/saas/domains/subdomains/{id}",
                owner: "saas_domains",
                description: "Delete subdomain allocation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/resolve",
                owner: "saas_domains",
                description: "Resolve debug for a host",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/saas/domains/settings",
                owner: "saas_domains",
                description: "Get SaaS domains settings",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/saas/domains/settings",
                owner: "saas_domains",
                description: "Update SaaS domains settings",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/saas/domains")
                .route("/reservations", web::post().to(create_reservation))
                .route("/reservations", web::get().to(list_reservations))
                .route(
                    "/reservations/expire",
                    web::post().to(expire_reservations_endpoint),
                )
                .route("/reservations/{id}", web::get().to(get_reservation))
                .route(
                    "/reservations/{id}/release",
                    web::post().to(release_reservation),
                )
                .route(
                    "/reservations/{id}/provision",
                    web::post().to(provision_reservation),
                )
                .route("/aliases", web::post().to(create_alias))
                .route("/aliases", web::get().to(list_aliases))
                .route("/aliases/{id}", web::get().to(get_alias))
                .route("/aliases/{id}", web::put().to(update_alias))
                .route("/aliases/{id}", web::delete().to(delete_alias))
                .route("/aliases/{id}/suspend", web::post().to(suspend_alias))
                .route("/aliases/{id}/resume", web::post().to(resume_alias))
                .route("/aliases/{id}/release", web::post().to(release_alias))
                .route("/subdomains", web::post().to(create_subdomain))
                .route("/subdomains", web::get().to(list_subdomains))
                .route("/subdomains/{id}", web::get().to(get_subdomain))
                .route("/subdomains/{id}", web::delete().to(delete_subdomain))
                .route(
                    "/subdomains/{id}/approve",
                    web::post().to(approve_subdomain),
                )
                .route("/subdomains/{id}/reject", web::post().to(reject_subdomain))
                .route(
                    "/subdomains/{id}/suspend",
                    web::post().to(suspend_subdomain),
                )
                .route(
                    "/subdomains/{id}/resume",
                    web::post().to(resume_subdomain),
                )
                .route(
                    "/subdomains/{id}/release",
                    web::post().to(release_subdomain),
                )
                .route(
                    "/product-subdomains/check",
                    web::post().to(check_product_subdomain),
                )
                .route(
                    "/product-subdomains/expire",
                    web::post().to(expire_product_subdomains),
                )
                .route(
                    "/product-subdomains",
                    web::post().to(claim_product_subdomain),
                )
                .route(
                    "/product-subdomains",
                    web::get().to(list_product_subdomains),
                )
                .route(
                    "/product-subdomains/{id}",
                    web::get().to(get_product_subdomain),
                )
                .route(
                    "/product-subdomains/{id}/verify",
                    web::post().to(verify_product_subdomain),
                )
                .route(
                    "/product-subdomains/{id}/activate",
                    web::post().to(activate_product_subdomain),
                )
                .route(
                    "/product-subdomains/{id}/suspend",
                    web::post().to(suspend_product_subdomain),
                )
                .route(
                    "/product-subdomains/{id}/release",
                    web::post().to(release_product_subdomain),
                )
                .route("/resolve", web::get().to(resolve_debug))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SaasDomainsModule));
}
