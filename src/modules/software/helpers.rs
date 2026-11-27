//! Caller identity and capability checks for software handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{HttpRequest};
use serde_json::{json};
use super::state::SoftwareState;
use super::types::{Release, ReleaseFile, Software, SoftwareVersion};

// Caller identity and capability checks.

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
/// Same header-based shape as the products, events, and community
/// modules.
pub(crate) struct Caller {
    #[allow(dead_code)]
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
/// (Same fail-closed shape as the products and events modules.)
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

// Tenant edge guard (C3 pilot 3, group C).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller()` or any capability
/// check.
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


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
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
/// scope. Shared rule (`CallerScope::can_access_record`): a company
/// caller reads its own rows plus global (unscoped) rows; an
/// unresolved caller reads global rows only.
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => {
            record_company_id.is_none() || record_company_id.as_deref() == Some(company.as_str())
        }
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
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
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


// Parent scope: versions, releases and files inherit the vendor
// company of the software record they belong to. A version with no
// software link, and a release/file whose parent chain is missing,
// is global (no company to scope by).

/// The vendor company of a version's parent software record, or
/// `None` for a standalone version / a dangling software link.
pub(crate) fn version_company(state: &SoftwareState, version: &SoftwareVersion) -> Option<String> {
    match &version.software_id {
        Some(id) => state
            .software
            .get(id)
            .and_then(|entry| entry.value().vendor_company_id.clone()),
        None => None,
    }
}

/// The vendor company of a release's parent version (via its
/// software record), or `None` when the chain carries no company.
pub(crate) fn release_company(state: &SoftwareState, release: &Release) -> Option<String> {
    state
        .versions
        .get(&release.version_id)
        .map(|entry| version_company(state, entry.value()))
        .unwrap_or(None)
}

/// The vendor company of a release file's parent release, or `None`
/// when the chain carries no company.
pub(crate) fn file_company(state: &SoftwareState, file: &ReleaseFile) -> Option<String> {
    state
        .releases
        .get(&file.release_id)
        .map(|entry| release_company(state, entry.value()))
        .unwrap_or(None)
}

/// Load a software record and verify it is in the caller scope: a
/// missing record and an out-of-scope record both surface as
/// `not_found` for the load itself only when the caller has no
/// scope information to distinguish them - here a missing record
/// is 404 and an out-of-scope record is 403, matching the
/// module's existing foreign-record handling.
pub(crate) fn require_software_in_scope(
    state: &SoftwareState,
    software_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Software> {
    let software = state
        .software
        .get(software_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
    require_record_in_company_scope(&software.vendor_company_id, scope)?;
    Ok(software)
}

/// Load a version and verify its parent software is in the caller
/// scope (standalone versions are global).
pub(crate) fn require_version_in_scope(
    state: &SoftwareState,
    version_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<SoftwareVersion> {
    let version = state
        .versions
        .get(version_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Version not found"))?;
    require_record_in_company_scope(&version_company(state, &version), scope)?;
    Ok(version)
}

/// Load a release and verify its parent chain is in the caller scope.
pub(crate) fn require_release_in_scope(
    state: &SoftwareState,
    release_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Release> {
    let release = state
        .releases
        .get(release_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
    require_record_in_company_scope(&release_company(state, &release), scope)?;
    Ok(release)
}

/// Load a release file (which must belong to the given release) and
/// verify its parent chain is in the caller scope.
pub(crate) fn require_file_in_scope(
    state: &SoftwareState,
    release_id: &str,
    file_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ReleaseFile> {
    let file = state
        .release_files
        .get(file_id)
        .filter(|entry| entry.value().release_id == release_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Release file not found"))?;
    require_record_in_company_scope(&file_company(state, &file), scope)?;
    Ok(file)
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
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header must never choose the company: only the
        // tenant context and the verified caller are sources.
        let req = TestRequest::default()
            .insert_header(("X-Company-Id", "forged"))
            .to_http_request();
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
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_follows_the_shared_rule() {
        let scope = Some("acme".to_string());
        // A company caller reads its own rows plus global rows.
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        // An unresolved caller reads global rows only.
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("acme".to_string()), &None));
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
    fn version_company_inherits_the_parent_software_vendor() {
        use crate::modules::software::types::VersionRequirements;
        use chrono::Utc;
        let state = SoftwareState::new();
        state.software.insert(
            "sw-1".to_string(),
            Software {
                id: "sw-1".to_string(),
                slug: "sw-1".to_string(),
                name: "Sw 1".to_string(),
                kind: "plugin".to_string(),
                description: String::new(),
                vendor_company_id: Some("acme".to_string()),
                group_id: None,
                category_id: None,
                status: "draft".to_string(),
                homepage_url: None,
                repository_url: None,
                documentation_url: None,
                screenshots: Vec::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        let linked = SoftwareVersion {
            id: "ver-1".to_string(),
            product: "sw-1".to_string(),
            version: "1.0.0".to_string(),
            changelog: String::new(),
            file_id: None,
            license_required: false,
            software_id: Some("sw-1".to_string()),
            requirements: VersionRequirements::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(
            version_company(&state, &linked).as_deref(),
            Some("acme")
        );
        let standalone = SoftwareVersion {
            software_id: None,
            ..linked.clone()
        };
        assert_eq!(version_company(&state, &standalone), None);
        // An out-of-scope caller cannot load the linked version,
        // but the standalone version is global.
        assert!(require_version_in_scope(&state, "ver-1", &Some("acme".to_string())).is_err());
        state.versions.insert(linked.id.clone(), linked);
        assert!(require_version_in_scope(&state, "ver-1", &Some("acme".to_string())).is_ok());
        assert!(require_version_in_scope(&state, "ver-1", &Some("other".to_string())).is_err());
        assert!(require_version_in_scope(&state, "ver-1", &None).is_err());
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
    use std::sync::Arc;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", "software:manage"));
        assert!(require_permission(&ctx, "admin", "software:manage").is_err());
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            std::sync::Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        assert!(require_permission(&ctx, "admin", "software:manage").is_ok());
    }
}
