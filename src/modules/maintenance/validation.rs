// Maintenance module — input validation, pagination, and sorting.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn parse_window_id(raw: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid maintenance window id \"{raw}\"")))
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same trusted-parameter
// pattern as the tax module: X-User-Id plus X-Role headers,
// `role.check` hook, fail closed). Every handler checks a
// capability first, before any state refresh or mutation.
// ---------------------------------------------------------------------------

/// Capability names checked through the `role.check` hook.
pub const PERM_MAINTENANCE_VIEW: &str = "maintenance:view";

pub const PERM_MAINTENANCE_MANAGE: &str = "maintenance:manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
pub struct Caller {
    pub actor: String,
    pub role: String,
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only — there is no per-record company check to
/// apply. Real partitioning needs a `tenant_id` on the record, a
/// C11 follow-up, not a C3 gate.
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
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above — records here carry
/// no company field, so there is nothing to partition on read.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

pub(crate) fn validate_window_input(
    title: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    affected_services: &[String],
) -> UnifiedResult<()> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > 200 {
        return Err(UnifiedError::bad_request(
            "title must be at most 200 characters",
        ));
    }
    if ends_at <= starts_at {
        return Err(UnifiedError::bad_request(
            "ends_at must be after starts_at",
        ));
    }
    if starts_at < Utc::now() {
        return Err(UnifiedError::bad_request(
            "starts_at must not be in the past",
        ));
    }
    if affected_services.len() > 50 {
        return Err(UnifiedError::bad_request(
            "affected_services must list at most 50 services",
        ));
    }
    for service in affected_services {
        if service.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "affected_services must not contain empty entries",
            ));
        }
        if service.chars().count() > 100 {
            return Err(UnifiedError::bad_request(
                "each affected service name must be at most 100 characters",
            ));
        }
    }
    Ok(())
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

pub(crate) fn sort_windows(mut items: Vec<MaintenanceWindow>, params: &PaginationParams) -> Vec<MaintenanceWindow> {
    let field = params.allowed_sort(&["starts_at", "ends_at", "title", "created_at"], "starts_at");
    match field.as_str() {
        "ends_at" => items.sort_by(|a, b| a.ends_at.cmp(&b.ends_at)),
        "title" => items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        _ => items.sort_by(|a, b| a.starts_at.cmp(&b.starts_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    items
}

#[cfg(test)]
mod tests {

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

    fn window(title: &str, start_hour: i64, end_hour: i64) -> MaintenanceWindow {
        let base = Utc::now();
        MaintenanceWindow {
            id: Uuid::new_v4(),
            title: title.to_string(),
            description: String::new(),
            starts_at: base + chrono::Duration::hours(start_hour),
            ends_at: base + chrono::Duration::hours(end_hour),
            affected_services: Vec::new(),
            status: STATUS_SCHEDULED.to_string(),
            created_at: base,
            updated_at: base,
            activated_at: None,
            completed_at: None,
        }
    }

    fn params(sort: Option<&str>, order: Option<&str>) -> PaginationParams {
        PaginationParams {
            sort: sort.map(str::to_string),
            order: order.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn parse_window_id_accepts_uuids_only() {
        let id = Uuid::new_v4();
        assert_eq!(parse_window_id(&id.to_string()).unwrap(), id);
        assert!(parse_window_id("not-a-uuid").is_err());
        assert!(parse_window_id("").is_err());
    }

    #[test]
    fn validate_window_input_accepts_a_well_formed_window() {
        let now = Utc::now();
        let start = now + chrono::Duration::hours(1);
        let later = now + chrono::Duration::hours(2);
        assert!(validate_window_input("Deploy", start, later, &["api".to_string()]).is_ok());
    }

    #[test]
    fn validate_window_input_rejects_bad_titles() {
        let now = Utc::now();
        let start = now + chrono::Duration::hours(1);
        let later = now + chrono::Duration::hours(2);
        assert!(validate_window_input("", start, later, &[]).is_err());
        assert!(validate_window_input("   ", start, later, &[]).is_err());
        assert!(validate_window_input(&"x".repeat(201), start, later, &[]).is_err());
        assert!(validate_window_input(&"x".repeat(200), start, later, &[]).is_ok());
    }

    #[test]
    fn validate_window_input_rejects_a_start_in_the_past() {
        let now = Utc::now();
        let past_start = now - chrono::Duration::hours(1);
        let future_end = now + chrono::Duration::hours(1);
        assert!(validate_window_input("T", past_start, future_end, &[]).is_err());
        let future_start = now + chrono::Duration::hours(1);
        let later_end = now + chrono::Duration::hours(2);
        assert!(validate_window_input("T", future_start, later_end, &[]).is_ok());
    }

    #[test]
    fn validate_window_input_rejects_bad_ranges_and_services() {
        let now = Utc::now();
        assert!(validate_window_input("T", now, now, &[]).is_err());
        assert!(validate_window_input("T", now, now - chrono::Duration::hours(1), &[]).is_err());
        let too_many = vec!["svc".to_string(); 51];
        let later = now + chrono::Duration::hours(1);
        assert!(validate_window_input("T", now, later, &too_many).is_err());
        assert!(validate_window_input("T", now, later, &["".to_string()]).is_err());
        assert!(validate_window_input("T", now, later, &["x".repeat(101)]).is_err());
    }

    #[test]
    fn sort_windows_defaults_to_start_time() {
        let items = vec![window("b", 5, 6), window("a", 1, 2), window("c", 3, 4)];
        let sorted = sort_windows(items, &params(None, None));
        assert_eq!(sorted[0].title, "a");
        assert_eq!(sorted[1].title, "c");
        assert_eq!(sorted[2].title, "b");
    }

    #[test]
    fn sort_windows_by_title_descending() {
        let items = vec![window("b", 5, 6), window("a", 1, 2)];
        let sorted = sort_windows(items, &params(Some("title"), Some("desc")));
        assert_eq!(sorted[0].title, "b");
        assert_eq!(sorted[1].title, "a");
    }

    #[test]
    fn sort_windows_ignores_unknown_sort_fields() {
        let items = vec![window("b", 5, 6), window("a", 1, 2)];
        let sorted = sort_windows(items, &params(Some("bogus"), None));
        assert_eq!(sorted[0].title, "a");
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(caller(&bare).is_err());
        let no_role = actix_web::test::TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let authed = actix_web::test::verified_request("alice", "admin");
        let found = caller(&authed).unwrap();
        assert_eq!(found.actor, test_user_uuid("alice").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_answerer() {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        assert!(!check_capability(&ctx, "admin", PERM_MAINTENANCE_MANAGE));
        let error = require_permission(&ctx, "admin", PERM_MAINTENANCE_MANAGE).unwrap_err();
        assert_eq!(error.code, 403);
    }

    /// Build a request carrying a resolved tenant context plus a
    /// verified caller, for the tenant-gate tests below.
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
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
    fn caller_company_prefers_tenant_company() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        // No tenant company: the verified caller's company is the scope.
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &uuid::Uuid::now_v7().to_string(),
            Some(&uuid::Uuid::now_v7().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), company);
    }
}
