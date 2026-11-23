// Domains module - in-memory state.
//
// Split from `src/modules/domains.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::DbPool;
use unified_core::db::models::Domain;
use actix_web::HttpRequest;
use dashmap::DashMap;
use serde_json::json;
use std::sync::OnceLock;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Permissions (checked on every handler through the `role.check`
// hook, the same shape backup/workflows/whitelabel use).
// ---------------------------------------------------------------------------

/// Read-only domain access: list and get.
pub(crate) const PERM_DOMAINS_VIEW: &str = "domains.view";

/// Domain management: create, update, delete, verify, SSL renewal,
/// and subdomain creation.
pub(crate) const PERM_DOMAINS_MANAGE: &str = "domains.manage";

/// Caller identity taken from the `X-User-Id` / `X-Role` headers.
/// Only the role is needed here: every domains check is a
/// permission check, and the actor id is validated as present but
/// otherwise not recorded by this module.
pub(crate) struct Caller {
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let role = verified.role().to_string();
    Ok(Caller { role })
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

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
/// — a request must not carry one company in its tenant context and
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
        (None, verified) => Ok(verified),
    }
}

/// Verify a request body's `company_id` (`Option<Uuid>`) against the
/// caller's company scope and return the company id to store. Fails
/// closed (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row. A caller scope that is not a UUID is refused the
/// same way (fail closed: domain company ids are UUIDs).
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
        (None, None) => Ok(None),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying exactly that company match; unscoped (legacy) records
/// are hidden from scoped callers.
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.map(|id| id.to_string()).as_deref() == Some(company.as_str()),
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

// ---------------------------------------------------------------------------
// Runtime handles cached at `init` time.
//
// Hook handlers are synchronous and receive no `AppContext`, yet the
// SSL-event and whitelabel listeners need the hook bus (to emit
// `domain.verify` outcomes) and the database pool (to persist SSL
// updates). `init_all` runs after the database pool is attached, so
// `DomainsModule::init` caches both here. Everything degrades
// gracefully when a handle was never cached: persistence falls back
// to the in-memory store and event emission is skipped.
// ---------------------------------------------------------------------------

struct CachedRuntime {
    hooks: std::sync::Arc<HookBus>,
    db: Option<DbPool>,
}

static RUNTIME: OnceLock<CachedRuntime> = OnceLock::new();

pub(crate) fn remember_runtime(ctx: &AppContext) {
    let db = ctx.db.read().expect("db lock poisoned").clone();
    let _ = RUNTIME.set(CachedRuntime {
        hooks: ctx.hooks.clone(),
        db,
    });
}

pub(crate) fn cached_hooks() -> Option<std::sync::Arc<HookBus>> {
    RUNTIME.get().map(|runtime| runtime.hooks.clone())
}

pub(crate) fn cached_db_pool() -> Option<DbPool> {
    RUNTIME.get().and_then(|runtime| runtime.db.clone())
}

// ---------------------------------------------------------------------------
// In-memory store (used when no database pool is attached).
// ---------------------------------------------------------------------------

pub(crate) static DOMAINS: OnceLock<DashMap<Uuid, Domain>> = OnceLock::new();

pub(crate) fn domains_store() -> &'static DashMap<Uuid, Domain> {
    DOMAINS.get_or_init(DashMap::new)
}

pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

pub(crate) fn paginate<T: serde::Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        let scope = Some("acme".to_string());
        let acme = Uuid::now_v7();
        assert!(in_company_scope(&Some(acme), &Some(acme.to_string())));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &scope));
        assert!(!in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &None));
    }

    #[test]
    fn require_company_match_uuid_refuses_body_company_from_unresolved_callers() {
        let company = Uuid::now_v7();
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company_match_uuid(&tenant, &req, &Some(company)).is_err());
        assert_eq!(
            require_company_match_uuid(&tenant, &req, &None).unwrap(),
            None
        );
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
    fn paginate_returns_the_requested_page() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }
}
