// bi module -- In-memory state and small access helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for Business Intelligence. The hook bus is
/// attached in `init` (the bus lives in `AppContext`); handlers
/// tolerate a missing bus by skipping event emission. Durable rows
/// for these types are defined by migration
/// `2026-11-22-000042_bi`; until that migration is executed,
/// reports, schedules, and dashboards do not survive a restart.
#[derive(Clone)]
pub struct BiState {
    pub reports: Arc<DashMap<Uuid, BiReport>>,
    pub schedules: Arc<DashMap<Uuid, BiReportSchedule>>,
    pub dashboards: Arc<DashMap<Uuid, BiKpiDashboard>>,
    pub source_records: Arc<DashMap<Uuid, SourceRecord>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for BiState {
    fn default() -> Self {
        Self {
            reports: Arc::new(DashMap::new()),
            schedules: Arc::new(DashMap::new()),
            dashboards: Arc::new(DashMap::new()),
            source_records: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl BiState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "bi",
                "category": "bi",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai and events modules.
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
/// (same fail-closed shape as the ai and events modules).
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

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check
/// (C3 pilot 3).
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
/// middleware-resolved tenant context are sources. This replaces the
/// old header-first `resolve_caller_company` (C3 pilot 3).
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

/// Verify a request body's `company_id` against the caller's
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

/// True when a stored record belongs to the caller's company scope.
/// BI records carry `company_id` as a required `String` (empty
/// means a global row). With no caller scope (an unresolved caller)
/// only global records match: every company-owned record is denied
/// (the shared `CallerScope::Unresolved` rule). With a scope,
/// records carrying exactly that company match, plus global rows,
/// which are shared content visible to any resolved scope
/// (C3 task 320, rule A).
pub(crate) fn in_company_scope_str(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_empty(),
        Some(company) => record_company_id.is_empty() || record_company_id == company,
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope_str` as a filter instead.
pub(crate) fn require_record_in_company_scope_str(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope_str(record_company_id, caller_company) {
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
pub(crate) fn require_record_writable_in_company_scope_str(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match caller_company {
        Some(caller)
            if !record_company_id.is_empty() && record_company_id == caller.as_str() =>
        {
            Ok(())
        }
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// True when a caller may access a record owned by `company_id`.
/// Manage-capable callers (server-verified capability) may access
/// any company; a company caller may only access records of their
/// own company; a caller with no resolvable company may access
/// global (empty-`company_id`) records only — platform access is
/// never the default for a missing company (the shared
/// `CallerScope::Unresolved` rule).
pub(crate) fn can_access_company(
    caller_company: Option<&str>,
    company_id: &str,
    can_manage: bool,
) -> bool {
    if can_manage {
        return true;
    }
    match caller_company {
        Some(company) => company == company_id,
        None => company_id.is_empty(),
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

#[cfg(test)]
mod tests {
    use super::can_access_company;

    #[test]
    fn can_access_company_allows_global_records_to_unresolved_callers_only() {
        assert!(can_access_company(None, "", false));
        assert!(!can_access_company(None, "co-1", false));
        assert!(can_access_company(Some("co-1"), "co-1", false));
        assert!(!can_access_company(Some("co-1"), "co-2", false));
        assert!(can_access_company(None, "co-1", true));
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

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

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = tenant_request("co-1", Some("co-1"));
        let tenant = require_tenant(&req).unwrap();
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("co-1")
        );
    }

    #[test]
    fn caller_company_rejects_tenant_and_verified_company_mismatch() {
        let req = tenant_request("co-1", Some("co-2"));
        let tenant = require_tenant(&req).unwrap();
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_falls_back_to_verified_caller_company() {
        let req = tenant_request("", Some("co-1"));
        let tenant = require_tenant(&req).unwrap();
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("co-1")
        );
    }

    #[test]
    fn caller_company_is_none_when_nothing_resolves() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).unwrap();
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_str_is_fail_closed_for_unresolved_callers() {
        assert!(in_company_scope_str("", &None));
        assert!(!in_company_scope_str("co-1", &None));
        assert!(in_company_scope_str("co-1", &Some("co-1".to_string())));
        assert!(!in_company_scope_str("co-1", &Some("co-2".to_string())));
        assert!(in_company_scope_str("", &Some("co-1".to_string())));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        assert!(
            require_company_match_for(&None, &Some("co-1".to_string())).is_err()
        );
        assert!(
            require_company_match_for(&Some("co-1".to_string()), &Some("co-1".to_string()))
                .is_ok()
        );
        assert!(
            require_company_match_for(&Some("co-1".to_string()), &Some("co-2".to_string()))
                .is_err()
        );
        // An omitted body company adopts the caller scope.
        assert_eq!(
            require_company_match_for(&Some("co-1".to_string()), &None).unwrap(),
            Some("co-1".to_string())
        );
        assert!(require_company_match_for(&None, &None).is_err());
    }
}
