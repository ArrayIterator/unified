//! pricing tables - auth.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use serde_json::json;
use super::state::PricingTablesState;
use super::types::*;

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// A vendor acting on their own record: the caller must hold
/// `marketplace:vendor` and own the vendor record, or hold
/// `marketplace:manage` (admin).
pub(crate) fn require_vendor_self_or_admin(
    ctx: &AppContext,
    caller: &Caller,
    vendor: &Vendor,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_MARKETPLACE_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_MARKETPLACE_VENDOR)
        && caller.actor == vendor.user_id
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Only the owning vendor or a marketplace administrator may perform this action",
    ))
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is task 317's job elsewhere).
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
        (None, verified) => Ok(verified),
    }
}

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
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


/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3 task 315):
/// a write requires a resolved company scope.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3 task 320, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
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
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Load a vendor and verify it is in the caller scope: company
/// callers read their own and global vendors only, unresolved
/// callers read global vendors only. A missing vendor answers
/// `not_found`; an out-of-scope vendor answers `forbidden`.
/// Vendor-scoped records (commissions, payouts, product
/// approvals) are checked through their parent vendor with the
/// loaders below, since they carry no company of their own.
pub(crate) fn require_vendor_in_scope(
    state: &PricingTablesState,
    vendor_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Vendor> {
    let vendor = state
        .vendors
        .get(vendor_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
    require_record_in_company_scope(&vendor.company_id, scope)?;
    Ok(vendor)
}

/// True when the vendor a record belongs to is in the caller
/// scope. A record whose vendor is missing is never in scope
/// (fail closed); list handlers use this as a filter.
pub(crate) fn vendor_in_scope(
    state: &PricingTablesState,
    vendor_id: &str,
    scope: &Option<String>,
) -> bool {
    state
        .vendors
        .get(vendor_id)
        .is_some_and(|entry| in_company_scope(&entry.company_id, scope))
}

/// Load a commission and verify its parent vendor is in the
/// caller scope.
pub(crate) fn require_commission_in_scope(
    state: &PricingTablesState,
    commission_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Commission> {
    let commission = state
        .commissions
        .get(commission_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Commission not found"))?;
    require_vendor_in_scope(state, &commission.vendor_id, scope)?;
    Ok(commission)
}

/// Load a payout and verify its parent vendor is in the caller
/// scope.
pub(crate) fn require_payout_in_scope(
    state: &PricingTablesState,
    payout_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Payout> {
    let payout = state
        .payouts
        .get(payout_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Payout not found"))?;
    require_vendor_in_scope(state, &payout.vendor_id, scope)?;
    Ok(payout)
}

/// Load a product approval and verify its parent vendor is in
/// the caller scope.
pub(crate) fn require_product_approval_in_scope(
    state: &PricingTablesState,
    approval_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ProductApproval> {
    let approval = state
        .product_approvals
        .get(approval_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product approval not found"))?;
    require_vendor_in_scope(state, &approval.vendor_id, scope)?;
    Ok(approval)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;
    use chrono::Utc;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
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
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
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

    fn test_vendor(company_id: Option<&str>) -> Vendor {
        Vendor {
            id: "v-1".to_string(),
            user_id: "u-1".to_string(),
            company_id: company_id.map(str::to_string),
            company_name: "Acme".to_string(),
            slug: "acme".to_string(),
            email: "a@b.co".to_string(),
            status: "approved".to_string(),
            application_notes: String::new(),
            review_reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let scope = Some("acme".to_string());
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("other".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).is_err());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_vendor_in_scope_partitions_by_company() {
        let state = PricingTablesState::new();
        state
            .vendors
            .insert("v-1".to_string(), test_vendor(Some("acme")));
        let scope = Some("acme".to_string());
        assert!(require_vendor_in_scope(&state, "v-1", &scope).is_ok());
        assert!(require_vendor_in_scope(&state, "v-1", &Some("other".to_string())).is_err());
        assert!(require_vendor_in_scope(&state, "v-1", &None).is_err());
        assert!(require_vendor_in_scope(&state, "missing", &scope).is_err());
        assert!(!vendor_in_scope(&state, "missing", &scope));
        state
            .vendors
            .insert("v-1".to_string(), test_vendor(None));
        assert!(require_vendor_in_scope(&state, "v-1", &None).is_ok());
        assert!(require_vendor_in_scope(&state, "v-1", &scope).is_err());
    }
}
