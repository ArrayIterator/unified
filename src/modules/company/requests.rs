// Company module - request bodies and pagination.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{Company, CompanySite, NewCompany, NewCompanySite};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;
use super::*;

// Request types.

#[derive(Debug, Deserialize)]
pub(crate) struct CreateCompanyRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateCompanyRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) slug: Option<String>,
    #[serde(default)]
    pub(crate) is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSiteRequest {
    pub(crate) company_id: Uuid,
    pub(crate) name: String,
    pub(crate) domain: String,
    #[serde(default)]
    pub(crate) is_primary: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSiteRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// The site domain is immutable: sending a different value is a 409.
    #[serde(default)]
    pub(crate) domain: Option<String>,
    #[serde(default)]
    pub(crate) is_primary: Option<bool>,
    #[serde(default)]
    pub(crate) is_active: Option<bool>,
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

// Caller identity and capability checks (same trusted-parameter
// pattern as the tax module: X-User-Id plus X-Role headers,
// `role.check` hook, fail closed). Every handler checks a
// capability first, before any database or store access.

/// Capability names checked through the `role.check` hook.
pub const PERM_COMPANY_VIEW: &str = "company:view";

pub const PERM_COMPANY_MANAGE: &str = "company:manage";

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

// Tenant edge guard (C3 pilot 3).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Company records ARE the scope (companies and their sites), so a
/// `company_id` filter would be circular here: the guard is a
/// fail-closed gate only, and no per-record company check is
/// applied. A company-scoped caller managing companies is a
/// control-plane concern, not a C3 record partition.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
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

    fn params(page: u32, per_page: u32) -> PaginationParams {
        PaginationParams {
            page: Some(page),
            per_page: Some(per_page),
            ..Default::default()
        }
    }

    #[test]
    fn paginate_first_page() {
        let page = paginate(vec![1u32, 2, 3, 4, 5], &params(1, 2));
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
        assert_eq!(page.pagination.page, 1);
    }

    #[test]
    fn paginate_middle_and_last_pages() {
        let second = paginate(vec![1u32, 2, 3, 4, 5], &params(2, 2));
        assert_eq!(second.items, vec![3, 4]);
        let last = paginate(vec![1u32, 2, 3, 4, 5], &params(3, 2));
        assert_eq!(last.items, vec![5]);
    }

    #[test]
    fn paginate_past_end_is_empty_but_keeps_total() {
        let page = paginate(vec![1u32, 2, 3], &params(5, 2));
        assert!(page.items.is_empty());
        assert_eq!(page.pagination.total, 3);
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
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
        assert!(!check_capability(&ctx, "admin", PERM_COMPANY_MANAGE));
        let error = require_permission(&ctx, "admin", PERM_COMPANY_MANAGE).unwrap_err();
        assert_eq!(error.code, 403);
    }
}
