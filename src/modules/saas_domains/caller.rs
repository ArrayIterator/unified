//! SaaS Domains - caller identity, capability checks and claim conflicts.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::HttpRequest;
use diesel::prelude::*;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use super::events::CAP_MANAGE;
use super::events::CAP_READ;
use super::events::CAP_SELF_ASSIGN;
use super::store::aliases_store;
use super::store::allocations_store;
use super::store::db_pool;
use super::store::reservations_store;

// ---------------------------------------------------------------------------
// Caller identity and capability checks (properties-module shape).
// ---------------------------------------------------------------------------

pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
    pub(crate) company_id: Option<Uuid>,
}

pub(crate) fn caller_from(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    Ok(Caller {
        actor: verified.user_id.clone(),
        role: verified.role().to_string(),
        company_id: verified
            .company_id
            .as_deref()
            .and_then(|value| value.parse::<Uuid>().ok()),
    })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authorize work on one tenant's records. Admins (manage
/// capability) may act for any tenant. Otherwise the caller needs
/// the self-assign capability AND a verified caller company that
/// matches the record's company; anything else is forbidden, so one
/// tenant can never touch another tenant's domains.
pub(crate) fn authorize_company(
    ctx: &AppContext,
    caller: &Caller,
    company_id: Uuid,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, CAP_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, CAP_SELF_ASSIGN) && caller.company_id == Some(company_id)
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Caller may only act on its own company's domains",
    ))
}

/// Authorize a list read. Admins see everything; other callers must
/// hold the read capability and are force-scoped to their own
/// company (the verified caller's server-resolved company);
/// returns the forced scope.
pub(crate) fn authorize_list(ctx: &AppContext, caller: &Caller) -> UnifiedResult<Option<Uuid>> {
    if check_capability(ctx, &caller.role, CAP_MANAGE) {
        return Ok(None);
    }
    if check_capability(ctx, &caller.role, CAP_READ) {
        return caller
            .company_id
            .ok_or_else(|| {
                UnifiedError::forbidden("Non-admin list reads require a verified caller company")
            })
            .map(Some);
    }
    Err(UnifiedError::forbidden(format!(
        "Role \"{}\" does not hold permission \"{}\"",
        caller.role, CAP_READ
    )))
}

// ---------------------------------------------------------------------------
// Cross-module conflict check: one domain maps to at most one tenant.
// ---------------------------------------------------------------------------

/// True when `domain` is already claimed anywhere: our own
/// reservations / aliases / allocations, or the Domains module's
/// table (Diesel read of the `domains` table).
/// `except_reservation`, `except_alias`, and `except_allocation`
/// exclude the record being updated. Released / expired / rejected
/// records do not block a domain.
///
/// NOTE: an earlier version of this check also emitted the
/// `domain.lookup` / `saas.site.lookup` hook events, but no module
/// in the repo listens to either event, so that leg always answered
/// "not claimed" and was deleted. If the domains or saas modules
/// ever gain listeners for those events, the hook leg can be
/// restored here — implementing the listeners themselves is a
/// cross-module follow-up outside this module's scope.
pub(crate) fn domain_claimed(
    ctx: &AppContext,
    domain: &str,
    except_reservation: Option<Uuid>,
    except_alias: Option<Uuid>,
    except_allocation: Option<Uuid>,
) -> bool {
    if reservations_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == domain
            && (row.status == "reserved" || row.status == "active")
            && Some(row.id) != except_reservation
    }) {
        return true;
    }
    if aliases_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == domain && row.status != "released" && Some(row.id) != except_alias
    }) {
        return true;
    }
    if allocations_store().iter().any(|entry| {
        let row = entry.value();
        row.hostname == domain
            && row.status != "released"
            && row.status != "rejected"
            && Some(row.id) != except_allocation
    }) {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::domains::dsl as dom_dsl;
        if let Ok(mut conn) = pool.get() {
            if let Ok(true) = dom_dsl::domains
                .filter(dom_dsl::name.eq(domain))
                .select(diesel::dsl::count_star())
                .first::<i64>(&mut conn)
                .map(|count| count > 0)
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn conflict_error(domain: &str) -> UnifiedError {
    UnifiedError::conflict(format!(
        "Domain \"{domain}\" is already claimed by another tenant"
    ))
}

/// Reject tenant-resolved requests from this control-plane
/// handler (C3 task 315, decision (a)): platform writes go only
/// through an explicit, audited platform scope. Mirrors
/// `tenancy::ControlPlaneGuard` — a request that resolved to a
/// tenant host is rejected outright (403); the guard's predicate
/// (`ControlPlaneGuard::is_control_plane_request`) is the single
/// source of truth, so this never drifts from the guard.
pub(crate) fn require_control_plane(req: &HttpRequest) -> UnifiedResult<()> {
    if unified_core::tenancy::ControlPlaneGuard::is_control_plane_request(req) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Tenant requests cannot access the control plane",
        )
        .with_metadata(
            "kind",
            serde_json::Value::String("tenant_control_plane_blocked".to_string()),
        ))
    }
}
