// hosting module - Helpers.
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
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{BILLING_CYCLES, HostingAccount, HostingPlan, HostingState};
use super::views::{AccountRequirementReport, RequirementItem};

// Helpers.

pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

/// Permission strings for this module's HTTP handlers.
pub(crate) const PERM_HOSTING_VIEW: &str = "hosting.view";
pub(crate) const PERM_HOSTING_MANAGE: &str = "hosting.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// Tenant edge guard (C3 pilot 3).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's `company_id` (`Option<Uuid>`) against the
/// caller's company scope and return the company id to store. Fails
/// closed (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
/// a write requires a resolved company scope. A caller scope that is not a UUID is refused the
/// same way (fail closed: account company ids are UUIDs).
pub(crate) fn require_company_match_uuid(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    let caller = caller_company(tenant, req)?;
    match (body_company_id, &caller) {
        (Some(body), Some(caller_company)) if body.to_string() != *caller_company => {
            Err(UnifiedError::forbidden(
                "company_id does not match the caller's company scope",
            ))
        }
        (Some(body), Some(_)) => Ok(Some(*body)),
        (Some(_), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller_company)) => {
            let id = Uuid::parse_str(caller_company).map_err(|_| {
                UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                )
            })?;
            Ok(Some(id))
        }
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// Verify an update body's `company_id` (`Option<Option<Uuid>>`)
/// against the caller's company scope, given the record's current
/// company. `None` means "no change" (the current company is kept);
/// `Some(None)` is an explicit clear and stays a clear (it is never
/// re-stamped with the caller scope); `Some(Some(id))` names a new
/// company and must equal the caller scope (an unresolved caller
/// may not name one).
pub(crate) fn require_company_match_uuid_update(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<Option<Uuid>>,
    current_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match body_company_id {
        None => Ok(*current_company_id),
        Some(None) => Ok(None),
        Some(Some(_)) => {
            let body = body_company_id.as_ref().and_then(|inner| *inner);
            require_company_match_uuid(tenant, req, &body)
        }
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.map(|id| id.to_string()).as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) => match Uuid::parse_str(caller) {
            Ok(scope) if *record == scope => Ok(()),
            _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
        },
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Sane maxima for plan resources and price: a plan above these is
/// a configuration mistake, not a real bundle. The caps also keep
/// the megabyte conversions in `validate_usage_against_plan` far
/// from integer overflow.
pub(crate) const MAX_PLAN_DISK_GB: i64 = 1_000_000;
pub(crate) const MAX_PLAN_CPU_CORES: f64 = 1024.0;
pub(crate) const MAX_PLAN_RAM_MB: i64 = 16_777_216;
pub(crate) const MAX_PLAN_BANDWIDTH_GB: i64 = 1_000_000;
pub(crate) const MAX_PLAN_PRICE: i64 = 100_000_000;

pub(crate) fn validate_plan_resources(
    disk_gb: i64,
    cpu_cores: f64,
    ram_mb: i64,
    bandwidth_gb: i64,
    price: i64,
) -> UnifiedResult<()> {
    if disk_gb <= 0 {
        return Err(UnifiedError::bad_request("disk_gb must be greater than 0"));
    }
    if disk_gb > MAX_PLAN_DISK_GB {
        return Err(UnifiedError::bad_request(format!(
            "disk_gb must be at most {MAX_PLAN_DISK_GB}"
        )));
    }
    if cpu_cores <= 0.0 || !cpu_cores.is_finite() {
        return Err(UnifiedError::bad_request(
            "cpu_cores must be a finite number greater than 0",
        ));
    }
    if cpu_cores > MAX_PLAN_CPU_CORES {
        return Err(UnifiedError::bad_request(format!(
            "cpu_cores must be at most {MAX_PLAN_CPU_CORES}"
        )));
    }
    if ram_mb <= 0 {
        return Err(UnifiedError::bad_request("ram_mb must be greater than 0"));
    }
    if ram_mb > MAX_PLAN_RAM_MB {
        return Err(UnifiedError::bad_request(format!(
            "ram_mb must be at most {MAX_PLAN_RAM_MB}"
        )));
    }
    if bandwidth_gb <= 0 {
        return Err(UnifiedError::bad_request(
            "bandwidth_gb must be greater than 0",
        ));
    }
    if bandwidth_gb > MAX_PLAN_BANDWIDTH_GB {
        return Err(UnifiedError::bad_request(format!(
            "bandwidth_gb must be at most {MAX_PLAN_BANDWIDTH_GB}"
        )));
    }
    if price < 0 {
        return Err(UnifiedError::bad_request("price must be >= 0"));
    }
    if price > MAX_PLAN_PRICE {
        return Err(UnifiedError::bad_request(format!(
            "price must be at most {}",
            minor_to_major(MAX_PLAN_PRICE, "USD")
        )));
    }
    Ok(())
}

/// Convert integer minor units to major units for error messages.
/// Boundary conversion only; plan prices are minor units.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if unified_core::db::money::is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        let major = amount_minor as f64 / 100.0;
        (major * 100.0).round() / 100.0
    }
}

pub(crate) fn validate_billing_cycle(cycle: &str) -> UnifiedResult<()> {
    if !BILLING_CYCLES.contains(&cycle) {
        return Err(UnifiedError::bad_request(format!(
            "billing_cycle must be one of: {}",
            BILLING_CYCLES.join(", ")
        )));
    }
    Ok(())
}

pub(crate) fn validate_currency(currency: &str) -> UnifiedResult<String> {
    let normalized = currency.trim().to_uppercase();
    if normalized.len() != 3 || !normalized.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code, for example \"USD\"",
        ));
    }
    Ok(normalized)
}

pub(crate) fn validate_usage_against_plan(
    plan: &HostingPlan,
    disk_used_mb: i64,
    bandwidth_used_mb: i64,
) -> UnifiedResult<()> {
    if disk_used_mb < 0 {
        return Err(UnifiedError::bad_request(
            "disk_used_mb must be >= 0",
        ));
    }
    if bandwidth_used_mb < 0 {
        return Err(UnifiedError::bad_request(
            "bandwidth_used_mb must be >= 0",
        ));
    }
    // Saturating conversions: a plan row that bypassed validation
    // (or was stored before the caps existed) must never overflow
    // the megabyte quota, even in debug builds.
    let disk_quota_mb = plan.disk_gb.saturating_mul(1024);
    if disk_used_mb > disk_quota_mb {
        return Err(UnifiedError::unprocessable(format!(
            "disk_used_mb ({disk_used_mb}) exceeds the plan quota of {disk_quota_mb} MB",
        )));
    }
    let bandwidth_quota_mb = plan.bandwidth_gb.saturating_mul(1024);
    if bandwidth_used_mb > bandwidth_quota_mb {
        return Err(UnifiedError::unprocessable(format!(
            "bandwidth_used_mb ({bandwidth_used_mb}) exceeds the plan quota of {bandwidth_quota_mb} MB",
        )));
    }
    Ok(())
}

/// Allowed account status transitions. `cancelled` is terminal.
pub(crate) fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "active")
            | ("pending", "cancelled")
            | ("active", "suspended")
            | ("active", "cancelled")
            | ("suspended", "active")
            | ("suspended", "cancelled")
    )
}

/// Authoritative domain row from the database, when a pool is
/// attached. Returns `Ok(None)` when there is no pool or no such
/// domain. A pool checkout or query failure is propagated as an
/// error instead of being collapsed into `None`, so callers can
/// tell "no such domain" apart from "the database is unreachable".
pub(crate) fn domain_from_db(ctx: &AppContext, name: &str) -> UnifiedResult<Option<Domain>> {
    let Some(pool) = db_pool(ctx) else {
        return Ok(None);
    };
    use unified_core::db::schema::domains::dsl as dom_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    dom_dsl::domains
        .filter(dom_dsl::name.eq(name))
        .select(Domain::as_select())
        .first(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load domain: {error}")))
}

pub(crate) fn parse_expiry(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<DateTime<Utc>>().ok())
}

/// Requirement check for one account. Each check reports its source,
/// so a caller can tell an authoritative database answer apart from a
/// hook-observed one and from an unverifiable one.
pub(crate) fn check_account_requirements(
    state: &Arc<HostingState>,
    ctx: &AppContext,
    account: &HostingAccount,
) -> AccountRequirementReport {
    let mut checks: Vec<RequirementItem> = Vec::new();

    // Plan must still exist (plan deletion is refused while accounts
    // reference the plan, so this is normally always ok).
    match state.plans.get(&account.plan_id) {
        Some(plan) => {
            checks.push(RequirementItem {
                code: "plan_exists".to_string(),
                label: "Hosting plan exists".to_string(),
                status: "ok".to_string(),
                detail: format!("Plan \"{}\" is registered.", plan.name),
                source: "local_state".to_string(),
            });
            // Store product linked to the plan. The link is
            // optional: a plan without one is sold outside the
            // Store, so a missing link is ok, not a failure.
            match &plan.store_product_id {
                None => checks.push(RequirementItem {
                    code: "store_product_linked".to_string(),
                    label: "Store product linked".to_string(),
                    status: "ok".to_string(),
                    detail: "The plan has no linked store product; the link is optional."
                        .to_string(),
                    source: "local_state".to_string(),
                }),
                Some(product_id) => match state.known_products.get(product_id) {
                    Some(published) => {
                        if *published.value() {
                            checks.push(RequirementItem {
                                code: "store_product_linked".to_string(),
                                label: "Store product linked".to_string(),
                                status: "ok".to_string(),
                                detail: format!(
                                    "Store product \"{product_id}\" is published."
                                ),
                                source: "hook_cache".to_string(),
                            });
                        } else {
                            checks.push(RequirementItem {
                                code: "store_product_linked".to_string(),
                                label: "Store product linked".to_string(),
                                status: "failed".to_string(),
                                detail: format!(
                                    "Store product \"{product_id}\" is unpublished."
                                ),
                                source: "hook_cache".to_string(),
                            });
                        }
                    }
                    None => checks.push(RequirementItem {
                        code: "store_product_linked".to_string(),
                        label: "Store product linked".to_string(),
                        status: "unknown".to_string(),
                        detail: format!(
                            "Store product \"{product_id}\" has not been observed via store.product.published; the product may exist but was published before this process started or while the hook had no listener."
                        ),
                        source: "unverified".to_string(),
                    }),
                },
            }
        }
        None => {
            checks.push(RequirementItem {
                code: "plan_exists".to_string(),
                label: "Hosting plan exists".to_string(),
                status: "failed".to_string(),
                detail: "The plan this account refers to no longer exists.".to_string(),
                source: "local_state".to_string(),
            });
            checks.push(RequirementItem {
                code: "store_product_linked".to_string(),
                label: "Store product linked".to_string(),
                status: "failed".to_string(),
                detail: "Cannot check the store product link without the plan.".to_string(),
                source: "local_state".to_string(),
            });
        }
    }

    // Domain registration + SSL state: database first, hook-observed
    // cache second, unknown otherwise. A database error is surfaced
    // as a failed check, never as "unknown": the database is
    // attached, so a failed lookup is a real failure, not missing
    // information.
    match domain_from_db(ctx, &account.domain) {
        Ok(Some(domain)) => {
            if domain.is_active {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "ok".to_string(),
                    detail: format!("Domain \"{}\" is registered and active.", domain.name),
                    source: "database".to_string(),
                });
            } else {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "failed".to_string(),
                    detail: format!("Domain \"{}\" is registered but inactive.", domain.name),
                    source: "database".to_string(),
                });
            }
            match domain.ssl_status.as_str() {
                "active" => {
                    let expired = domain
                        .ssl_expires_at
                        .map(|expiry| expiry <= Utc::now())
                        .unwrap_or(false);
                    if expired {
                        checks.push(RequirementItem {
                            code: "ssl_ready".to_string(),
                            label: "SSL ready".to_string(),
                            status: "failed".to_string(),
                            detail: "The domain certificate is expired; renew it first."
                                .to_string(),
                            source: "database".to_string(),
                        });
                    } else {
                        checks.push(RequirementItem {
                            code: "ssl_ready".to_string(),
                            label: "SSL ready".to_string(),
                            status: "ok".to_string(),
                            detail: "The domain has an active certificate.".to_string(),
                            source: "database".to_string(),
                        });
                    }
                }
                other => checks.push(RequirementItem {
                    code: "ssl_ready".to_string(),
                    label: "SSL ready".to_string(),
                    status: "failed".to_string(),
                    detail: format!(
                        "The domain SSL status is \"{other}\"; verify the domain or renew its certificate first."
                    ),
                    source: "database".to_string(),
                }),
            }
        }
        Ok(None) => match state.domain_observations.get(&account.domain) {
            Some(observation) if !observation.exists => {
                // A failed domain.verify report: the DNS check did
                // not pass, so the domain is not verified and the
                // failure must block activation.
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "failed".to_string(),
                    detail: format!(
                        "Domain \"{}\" failed verification: the DNS check did not pass.",
                        account.domain
                    ),
                    source: "hook_cache".to_string(),
                });
                checks.push(RequirementItem {
                    code: "ssl_ready".to_string(),
                    label: "SSL ready".to_string(),
                    status: "failed".to_string(),
                    detail: "SSL state cannot be confirmed: domain verification failed."
                        .to_string(),
                    source: "hook_cache".to_string(),
                });
            }
            Some(observation) => {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "ok".to_string(),
                    detail: format!(
                        "Domain \"{}\" was observed via a domain.verify event.",
                        account.domain
                    ),
                    source: "hook_cache".to_string(),
                });
                let expired = observation
                    .ssl_expires_at
                    .map(|expiry| expiry <= Utc::now())
                    .unwrap_or(false);
                if observation.ssl_state == "active" && !expired {
                    checks.push(RequirementItem {
                        code: "ssl_ready".to_string(),
                        label: "SSL ready".to_string(),
                        status: "ok".to_string(),
                        detail: "The domain has an active certificate (observed via hook event)."
                            .to_string(),
                        source: "hook_cache".to_string(),
                    });
                } else if expired {
                    checks.push(RequirementItem {
                        code: "ssl_ready".to_string(),
                        label: "SSL ready".to_string(),
                        status: "failed".to_string(),
                        detail: "The observed domain certificate is expired; renew it first."
                            .to_string(),
                        source: "hook_cache".to_string(),
                    });
                } else {
                    checks.push(RequirementItem {
                        code: "ssl_ready".to_string(),
                        label: "SSL ready".to_string(),
                        status: "failed".to_string(),
                        detail: format!(
                            "The observed domain SSL state is \"{}\"; verify the domain or renew its certificate first.",
                            observation.ssl_state
                        ),
                        source: "hook_cache".to_string(),
                    });
                }
            }
            None => {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "unknown".to_string(),
                    detail: "No database pool is attached and no domain.verify event has been observed for this domain, so registration cannot be confirmed. Attach a database or verify the domain first.".to_string(),
                    source: "unverified".to_string(),
                });
                checks.push(RequirementItem {
                    code: "ssl_ready".to_string(),
                    label: "SSL ready".to_string(),
                    status: "unknown".to_string(),
                    detail: "SSL state cannot be confirmed without a database pool or an observed domain.verify / ssl.renewed event.".to_string(),
                    source: "unverified".to_string(),
                });
            }
        },
        Err(error) => {
            checks.push(RequirementItem {
                code: "domain_registered".to_string(),
                label: "Domain registered".to_string(),
                status: "failed".to_string(),
                detail: format!(
                    "Database lookup for domain \"{}\" failed: {error}",
                    account.domain
                ),
                source: "database".to_string(),
            });
            checks.push(RequirementItem {
                code: "ssl_ready".to_string(),
                label: "SSL ready".to_string(),
                status: "failed".to_string(),
                detail: format!(
                    "SSL state for domain \"{}\" cannot be checked: the database lookup failed.",
                    account.domain
                ),
                source: "database".to_string(),
            });
        }
    }

    // Paid store order, when the account was bought through the Store.
    // Claims are keyed by (order id, plan id): one order can pay for
    // several plans, so the claim is looked up by both fields.
    if let Some(order_id) = &account.store_order_id {
        match state
            .order_claims
            .get(&(order_id.clone(), account.plan_id))
        {
            Some(_claim) => {
                checks.push(RequirementItem {
                    code: "store_order_paid".to_string(),
                    label: "Store order paid".to_string(),
                    status: "ok".to_string(),
                    detail: format!(
                        "Store order \"{order_id}\" was paid for this plan."
                    ),
                    source: "hook_cache".to_string(),
                });
            }
            None if state
                .order_claims
                .iter()
                .any(|entry| entry.key().0 == *order_id) =>
            {
                checks.push(RequirementItem {
                    code: "store_order_paid".to_string(),
                    label: "Store order paid".to_string(),
                    status: "failed".to_string(),
                    detail: format!(
                        "Store order \"{order_id}\" was paid for a different hosting plan."
                    ),
                    source: "hook_cache".to_string(),
                });
            }
            None => checks.push(RequirementItem {
                code: "store_order_paid".to_string(),
                label: "Store order paid".to_string(),
                status: "unknown".to_string(),
                detail: format!(
                    "Store order \"{order_id}\" has not been observed via store.order.completed / order.paid; the order may predate this process or may not be paid yet."
                ),
                source: "unverified".to_string(),
            }),
        }
    }

    let has_failed = checks.iter().any(|check| check.status == "failed");
    let has_unknown = checks.iter().any(|check| check.status == "unknown");
    let status = if has_failed {
        "not_ready".to_string()
    } else if has_unknown {
        "unknown".to_string()
    } else {
        "ok".to_string()
    };
    AccountRequirementReport {
        account_id: account.id,
        domain: account.domain.clone(),
        can_activate: status == "ok",
        status,
        checks,
    }
}

#[cfg(test)]
mod tests {

    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(
                uuid::Uuid::now_v7(),
                Vec::new(),
            )
            .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_then_verified_caller() {
        let company = Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some("other-company"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(&Uuid::now_v7().to_string(), Some(&Uuid::now_v7().to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let scope = Some("acme".to_string());
        let acme = Uuid::now_v7();
        assert!(in_company_scope(&Some(acme), &Some(acme.to_string())));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &None));
    }

    #[test]
    fn require_company_match_uuid_refuses_writes_from_unresolved_callers() {
        let company = Uuid::now_v7();
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company_match_uuid(&tenant, &req, &Some(company)).is_err());
        assert!(require_company_match_uuid(&tenant, &req, &None).is_err());
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            require_company_match_uuid(&tenant, &req, &Some(company)).unwrap(),
            Some(company)
        );
        assert_eq!(
            require_company_match_uuid(&tenant, &req, &None).unwrap(),
            Some(company)
        );
        assert!(require_company_match_uuid(&tenant, &req, &Some(Uuid::now_v7())).is_err());
    }

    #[test]
    fn require_company_match_uuid_update_keeps_clear_and_no_change() {
        let company = Uuid::now_v7();
        let other = Uuid::now_v7();
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        // No change keeps the current company.
        assert_eq!(
            require_company_match_uuid_update(&tenant, &req, &None, &Some(company)).unwrap(),
            Some(company)
        );
        // An explicit clear stays a clear (never re-stamped).
        assert_eq!(
            require_company_match_uuid_update(&tenant, &req, &Some(None), &Some(company)).unwrap(),
            None
        );
        // A newly named company must equal the caller scope.
        assert_eq!(
            require_company_match_uuid_update(&tenant, &req, &Some(Some(company)), &Some(other))
                .unwrap(),
            Some(company)
        );
        assert!(
            require_company_match_uuid_update(&tenant, &req, &Some(Some(other)), &Some(company))
                .is_err()
        );
        // An unresolved caller may not name a company on update.
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(
            require_company_match_uuid_update(&tenant, &req, &Some(Some(company)), &None).is_err()
        );
    }

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use super::super::types::{DomainObservation, OrderClaim};
    use crate::modules::cloud::hosting::HostingPlan;
    use chrono::Utc;

    fn plan() -> HostingPlan {
        HostingPlan {
            id: Uuid::new_v4(),
            name: "Basic".to_string(),
            description: "Basic plan".to_string(),
            disk_gb: 10,
            cpu_cores: 2.0,
            ram_mb: 1024,
            bandwidth_gb: 100,
            price: 999,
            currency: "USD".to_string(),
            billing_cycle: "monthly".to_string(),
            store_product_id: None,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(minor_to_major(0, "USD"), 0.0);
    }

    #[test]
    fn validate_plan_resources_accepts_a_sane_plan() {
        assert!(validate_plan_resources(10, 2.0, 1024, 100, 999).is_ok());
        assert!(validate_plan_resources(1, 0.5, 1, 1, 0).is_ok());
    }

    #[test]
    fn validate_plan_resources_rejects_non_positive_resources() {
        assert!(validate_plan_resources(0, 2.0, 1024, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, 0.0, 1024, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, 2.0, 0, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, 2.0, 1024, 0, 9.99).is_err());
        assert!(validate_plan_resources(10, 2.0, 1024, 100, -1.0).is_err());
        assert!(validate_plan_resources(10, 2.0, 1024, 100, f64::NAN).is_err());
    }

    #[test]
    fn validate_billing_cycle_accepts_only_the_known_cycles() {
        assert!(validate_billing_cycle("monthly").is_ok());
        assert!(validate_billing_cycle("yearly").is_ok());
        assert!(validate_billing_cycle("weekly").is_err());
        assert!(validate_billing_cycle("").is_err());
    }

    #[test]
    fn validate_currency_normalises_to_uppercase_iso_codes() {
        assert_eq!(validate_currency("usd").unwrap(), "USD");
        assert_eq!(validate_currency(" EUR ").unwrap(), "EUR");
        assert!(validate_currency("US").is_err());
        assert!(validate_currency("US1").is_err());
    }

    #[test]
    fn allowed_transition_matches_the_account_lifecycle() {
        for (from, to) in [
            ("pending", "active"),
            ("pending", "cancelled"),
            ("active", "suspended"),
            ("active", "cancelled"),
            ("suspended", "active"),
            ("suspended", "cancelled"),
        ] {
            assert!(allowed_transition(from, to), "{from} -> {to} must be allowed");
        }
        for (from, to) in [
            ("pending", "pending"),
            ("pending", "suspended"),
            ("active", "pending"),
            ("active", "active"),
            ("suspended", "suspended"),
            ("cancelled", "active"),
            ("cancelled", "cancelled"),
        ] {
            assert!(!allowed_transition(from, to), "{from} -> {to} must be rejected");
        }
    }

    #[test]
    fn validate_usage_against_plan_accepts_usage_within_quota() {
        let plan = plan();
        assert!(validate_usage_against_plan(&plan, 5120, 20480).is_ok());
        // Exactly at quota is allowed.
        assert!(validate_usage_against_plan(&plan, 10 * 1024, 100 * 1024).is_ok());
    }

    #[test]
    fn validate_usage_against_plan_rejects_over_quota_and_negative_usage() {
        let plan = plan();
        assert!(validate_usage_against_plan(&plan, 10 * 1024 + 1, 0).is_err());
        assert!(validate_usage_against_plan(&plan, 0, 100 * 1024 + 1).is_err());
        assert!(validate_usage_against_plan(&plan, -1, 0).is_err());
        assert!(validate_usage_against_plan(&plan, 0, -1).is_err());
    }

    #[test]
    fn validate_plan_resources_rejects_values_above_the_caps() {
        assert!(validate_plan_resources(1_000_001, 2.0, 1024, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, 1024.5, 1024, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, f64::NAN, 1024, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, 2.0, 16_777_217, 100, 9.99).is_err());
        assert!(validate_plan_resources(10, 2.0, 1024, 1_000_001, 9.99).is_err());
        assert!(validate_plan_resources(10, 2.0, 1024, 100, 1_000_000.01).is_err());
        // Exactly at the caps is allowed.
        assert!(validate_plan_resources(
            MAX_PLAN_DISK_GB,
            MAX_PLAN_CPU_CORES,
            MAX_PLAN_RAM_MB,
            MAX_PLAN_BANDWIDTH_GB,
            MAX_PLAN_PRICE
        )
        .is_ok());
    }

    #[test]
    fn validate_usage_against_plan_does_not_overflow_on_huge_plans() {
        // A plan row that bypassed validation must not overflow the
        // megabyte conversion (debug builds panic on overflow).
        let mut huge = plan();
        huge.disk_gb = i64::MAX;
        huge.bandwidth_gb = i64::MAX;
        assert!(validate_usage_against_plan(&huge, 0, 0).is_ok());
        assert!(validate_usage_against_plan(&huge, i64::MAX, i64::MAX).is_ok());
    }

    fn test_ctx() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn account(plan_id: Uuid) -> HostingAccount {
        HostingAccount {
            id: Uuid::new_v4(),
            plan_id,
            domain: "example.com".to_string(),
            company_id: None,
            status: "pending".to_string(),
            ssl_state: "none".to_string(),
            disk_used_mb: 0,
            bandwidth_used_mb: 0,
            store_order_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn check_code(report: &AccountRequirementReport, code: &str) -> String {
        report
            .checks
            .iter()
            .find(|check| check.code == code)
            .map(|check| check.status.clone())
            .unwrap_or_else(|| "missing".to_string())
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-1", "admin");
        let caller = caller(&req).unwrap();
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        let ctx = test_ctx();
        assert!(!check_capability(&ctx, "admin", PERM_HOSTING_VIEW));
        assert!(!check_capability(&ctx, "admin", PERM_HOSTING_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_HOSTING_VIEW).is_err());
    }

    #[test]
    fn domain_from_db_returns_none_without_a_pool() {
        let ctx = test_ctx();
        assert_eq!(domain_from_db(&ctx, "example.com").unwrap(), None);
    }

    #[test]
    fn missing_store_product_link_is_ok_because_the_link_is_optional() {
        let ctx = test_ctx();
        let state = Arc::new(HostingState::new());
        let plan = plan();
        assert!(plan.store_product_id.is_none());
        state.plans.insert(plan.id, plan.clone());
        let report = check_account_requirements(&state, &ctx, &account(plan.id));
        assert_eq!(check_code(&report, "store_product_linked"), "ok");
    }

    #[test]
    fn failed_domain_observation_blocks_activation() {
        let ctx = test_ctx();
        let state = Arc::new(HostingState::new());
        let plan = plan();
        state.plans.insert(plan.id, plan.clone());
        state.domain_observations.insert(
            "example.com".to_string(),
            DomainObservation {
                exists: false,
                ssl_state: "failed".to_string(),
                ssl_expires_at: None,
                observed_at: Utc::now(),
            },
        );
        let report = check_account_requirements(&state, &ctx, &account(plan.id));
        assert_eq!(check_code(&report, "domain_registered"), "failed");
        assert_eq!(check_code(&report, "ssl_ready"), "failed");
        assert!(!report.can_activate);
    }

    #[test]
    fn order_claim_is_found_by_order_and_plan_together() {
        let ctx = test_ctx();
        let state = Arc::new(HostingState::new());
        let plan_a = plan();
        let mut plan_b = plan();
        plan_b.id = Uuid::new_v4();
        plan_b.name = "Pro".to_string();
        state.plans.insert(plan_a.id, plan_a.clone());
        state.plans.insert(plan_b.id, plan_b.clone());
        // One order paid for both plans: both claims must survive.
        for plan in [&plan_a, &plan_b] {
            state.order_claims.insert(
                ("order-1".to_string(), plan.id),
                OrderClaim {
                    order_id: "order-1".to_string(),
                    plan_id: plan.id,
                    user_id: "user-1".to_string(),
                    company_id: None,
                },
            );
        }
        let mut account_b = account(plan_b.id);
        account_b.store_order_id = Some("order-1".to_string());
        let report_b = check_account_requirements(&state, &ctx, &account_b);
        assert_eq!(check_code(&report_b, "store_order_paid"), "ok");
        // An account on a third plan sharing the same order id paid
        // for a different plan, not for its own.
        let plan_c = Uuid::new_v4();
        let mut account_c = account(plan_c);
        account_c.store_order_id = Some("order-1".to_string());
        let report_c = check_account_requirements(&state, &ctx, &account_c);
        assert_eq!(check_code(&report_c, "store_order_paid"), "failed");
    }
}
