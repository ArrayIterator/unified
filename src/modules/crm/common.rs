// crm — common.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::handlers_contacts::*;
use super::handlers_leads::*;
use super::handlers_activities::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
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

pub(crate) fn valid_email(email: &str) -> bool {
    // Same strict shape as the forms/appointments validators: exactly
    // one `@`, a non-empty local part, and a dotted domain that does
    // not start or end with a dot.
    let email = email.trim();
    if email.is_empty() || email.len() > 254 || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Trim an optional string field; an empty or whitespace-only value
/// becomes `None` so optional fields can be cleared on update and
/// are never stored untrimmed or as `""`.
pub(crate) fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

// ---------------------------------------------------------------------------
// Caller and capabilities (same pattern as todos: fail closed).
// ---------------------------------------------------------------------------

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
#[allow(dead_code)]
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
        (None, verified) => Ok(verified.clone()),
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

/// Resolve the caller and require `permission` in one step, for use
/// at the top of every handler.
///
/// NOTE: contacts and leads carry no tenant/company owner field, so
/// capability gating is the only scoping these routes have today.
/// Tenant scoping needs a schema change (an owner field on both
/// records plus a migration) and is recorded as a follow-up; until
/// then these routes must not be exposed beyond a trusted network.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<()> {
    let call = caller(req)?;
    require_permission(ctx, &call.role, permission)
}

pub(crate) fn valid_stage(stage: &str) -> bool {
    matches!(
        stage,
        "new" | "contacted" | "qualified" | "negotiation" | "won" | "lost" | "converted"
    )
}

pub(crate) const PIPELINE_STAGES: &[&str] = &[
    "new",
    "contacted",
    "qualified",
    "negotiation",
    "won",
    "lost",
    "converted",
];

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let call = caller(&req).unwrap();
        assert_eq!(call.actor, test_user_uuid("user-1").to_string());
        assert_eq!(call.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_CRM_READ));
        assert!(!check_capability(&ctx, "admin", PERM_CRM_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_CRM_READ).is_err());
        let req = verified_request("user-1", "admin");
        assert!(authorize(&ctx, &req, PERM_CRM_READ).is_err());
    }

    #[test]
    fn emails_need_an_at_sign_and_a_dotted_domain() {
        assert!(valid_email("ada@example.com"));
        assert!(valid_email("  ada@example.co  "));
        assert!(!valid_email("ada@example"));
        assert!(!valid_email("ada@"));
        assert!(!valid_email("@example.com"));
        assert!(!valid_email("plain"));
        assert!(!valid_email("a b@example.com"));
    }

    #[test]
    fn emails_reject_malformed_domains() {
        // A second `@`, a leading dot and a trailing dot in the domain
        // are all rejected by the stricter forms/appointments shape.
        assert!(!valid_email("ada@@example.com"));
        assert!(!valid_email("ada@.example.com"));
        assert!(!valid_email("ada@example.com."));
        assert!(!valid_email("ada@exam@ple.com"));
    }

    #[test]
    fn optional_fields_trim_and_clear_on_empty() {
        assert_eq!(
            clean_optional(Some("  Acme  ".to_string())),
            Some("Acme".to_string())
        );
        assert_eq!(clean_optional(Some("".to_string())), None);
        assert_eq!(clean_optional(Some("   ".to_string())), None);
        assert_eq!(clean_optional(None), None);
    }

    #[test]
    fn stages_accept_only_the_pipeline_values() {
        for stage in [
            "new",
            "contacted",
            "qualified",
            "negotiation",
            "won",
            "lost",
            "converted",
        ] {
            assert!(valid_stage(stage), "{stage} should be valid");
        }
        assert!(!valid_stage("pending"));
        assert!(!valid_stage("New"));
        assert!(!valid_stage(""));
    }

    #[test]
    fn search_matches_case_insensitively_or_passes_without_a_query() {
        let with = PaginationParams {
            search: Some("ADA".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Ada Lovelace", &with));
        assert!(!matches_search("Grace Hopper", &with));
        let blank = PaginationParams {
            search: Some("   ".to_string()),
            ..Default::default()
        };
        assert!(matches_search("anything", &blank));
        assert!(matches_search("anything", &PaginationParams::default()));
    }

    #[test]
    fn pagination_slices_pages_and_reports_totals() {
        let items: Vec<i64> = (1..=5).collect();
        let page_two = paginate(
            items.clone(),
            &PaginationParams {
                page: Some(2),
                per_page: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(page_two.items, vec![3, 4]);
        assert_eq!(page_two.pagination.total, 5);
        assert_eq!(page_two.pagination.total_pages, 3);
        let beyond = paginate(
            items.clone(),
            &PaginationParams {
                page: Some(9),
                per_page: Some(2),
                ..Default::default()
            },
        );
        assert!(beyond.items.is_empty());
        assert_eq!(beyond.pagination.total, 5);
        let all = paginate(items.clone(), &PaginationParams::default());
        assert_eq!(all.items, vec![1, 2, 3, 4, 5]);
        assert_eq!(all.pagination.total_pages, 1);
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
