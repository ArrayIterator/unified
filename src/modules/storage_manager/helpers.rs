// Caller identity and capability checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::json;

use super::*;

// Caller identity + capability checks (AI-module shape).

/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against.
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

/// Authorize one request: caller identity + one capability. Returns
/// the caller on success so handlers can use the actor id.
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// True when the caller is a PLATFORM admin for the given
/// capability (C3, decision (a) - the platform arm of
/// the owner-or-platform rule): the caller holds the capability
/// AND the request carries no resolved tenant context
/// (`ControlPlaneGuard`). Holding the capability alone is NOT
/// platform identity - a tenant-host caller who holds it is
/// still scoped by the owner arm in the access helpers below.
/// This replaces `is_storage_admin`, which treated the
/// capability alone as admin identity and so let any tenant
/// role holding `storage_manager.manage_*` touch any company's
/// buckets, connections and plans.
pub(crate) fn is_platform_admin(
    ctx: &AppContext,
    role: &str,
    req: &HttpRequest,
    permission: &str,
) -> bool {
    check_capability(ctx, role, permission)
        && unified_core::tenancy::ControlPlaneGuard::is_control_plane_request(req)
}


/// Enforce bucket-level tenant scoping for hub objects. A platform
/// admin (see `is_platform_admin`) may touch any bucket. A
/// non-admin may touch a bucket only when
/// the bucket is company-less (a shared bucket, where object-level
/// scoping still applies) or its `company_id` matches the caller's
/// company. Cross-tenant access answers 404 (the same per-user 404
/// pattern the BYO loaders use) so bucket existence never leaks
/// across tenants.
pub(crate) fn check_bucket_access(
    bucket: &StorageBucket,
    company_id: Option<&str>,
    is_admin: bool,
) -> UnifiedResult<()> {
    if is_admin {
        return Ok(());
    }
    match &bucket.company_id {
        None => Ok(()),
        Some(owner) if company_id == Some(owner.as_str()) => Ok(()),
        Some(_) => Err(UnifiedError::not_found(format!(
            "Storage bucket not found: {}",
            bucket.id
        ))),
    }
}

/// List-filter form of `check_bucket_access`: true when a bucket
/// is visible to the caller in a list. A platform admin (see
/// `is_platform_admin`) sees every bucket; a non-admin sees a
/// bucket only when it is company-less (a shared bucket) or its
/// `company_id` matches the caller's company. A missing bucket is
/// never visible through this helper - list handlers resolve the
/// parent bucket first and hide rows whose bucket is gone from
/// non-admins.
pub(crate) fn bucket_company_visible(
    bucket_company_id: &Option<String>,
    caller_company: &Option<String>,
    is_admin: bool,
) -> bool {
    if is_admin {
        return true;
    }
    match bucket_company_id {
        None => true,
        Some(owner) => caller_company.as_deref() == Some(owner.as_str()),
    }
}

/// Enforce object-level scoping for hub objects. A platform admin
/// (see `is_platform_admin`) may touch any object. A non-admin may
/// touch an object only when they own it (`owner_user`) or it
/// belongs to their company. Cross-tenant access answers 404 so
/// object existence never leaks.
pub(crate) fn check_object_access(
    object: &StoredObject,
    actor: &str,
    company_id: Option<&str>,
    is_admin: bool,
) -> UnifiedResult<()> {
    if is_admin || object.owner_user == actor {
        return Ok(());
    }
    if let Some(company) = company_id {
        if object.company_id.as_deref() == Some(company) {
            return Ok(());
        }
    }
    Err(UnifiedError::not_found(format!(
        "Storage object not found: {}",
        object.key
    )))
}

/// Resolve the company an upload is recorded under. A non-admin
/// may only upload into their own company (the verified caller
/// scope); a caller-supplied `?company_id=` that differs is
/// rejected. An admin may supply any company id.
pub(crate) fn resolve_upload_company(
    requested: Option<&str>,
    caller_company: Option<&str>,
    is_admin: bool,
) -> UnifiedResult<Option<String>> {
    match (requested, is_admin) {
        (Some(value), true) => Ok(Some(valid_scope_id(value)?)),
        (Some(value), false) => {
            let value = valid_scope_id(value)?;
            if caller_company.as_deref() == Some(value.as_str()) {
                Ok(Some(value))
            } else {
                Err(UnifiedError::forbidden(
                    "A non-admin caller may only upload into their own company",
                ))
            }
        }
        (None, _) => Ok(caller_company.map(str::to_string)),
    }
}

pub(crate) fn err_response(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
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


// Tenant edge guard (C3 pilot 2).

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
/// - a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is handled elsewhere).
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

/// Verify a request body's company id against the caller's
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
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3, rule A).
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


/// True when a quota row belongs to the caller's scope. A
/// company quota belongs to its company; a user quota belongs to
/// its user; a module quota is platform configuration and is
/// visible to every authorized caller. Any other scope value is
/// never visible (fail closed).
pub(crate) fn quota_in_scope(
    quota: &StorageQuota,
    caller_company: &Option<String>,
    actor: &str,
) -> bool {
    match quota.scope.as_str() {
        "company" => in_company_scope(&Some(quota.scope_id.clone()), caller_company),
        "user" => quota.scope_id == actor,
        "module" => true,
        _ => false,
    }
}

/// Fail closed (403) when a quota row is outside the caller's
/// scope. Single-record reads and mutations call this right after
/// loading the row; list handlers use `quota_in_scope` as a
/// filter instead.
pub(crate) fn require_quota_in_scope(
    quota: &StorageQuota,
    caller_company: &Option<String>,
    actor: &str,
) -> UnifiedResult<()> {
    if quota_in_scope(quota, caller_company, actor) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The quota belongs to a different company",
        ))
    }
}
/// Bucket, object, binding, plan, quota and BYO records carry
/// `company_id: Option<String>`; the shared-bucket design (a
/// company-less bucket is shared) is unchanged - the gate only
/// adds the fail-closed tenant context and the server-resolved
/// caller company (see `check_bucket_access`).


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
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::storage::StorageKind;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_bucket(company_id: Option<&str>) -> StorageBucket {
        let now = Utc::now();
        StorageBucket {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            provider_kind: StorageKind::Local,
            endpoint: String::new(),
            region: String::new(),
            container: String::new(),
            path: String::new(),
            sftp_host: String::new(),
            sftp_port: 0,
            sftp_username: String::new(),
            sftp_auth_kind: String::new(),
            sftp_keyfile: String::new(),
            allowed_hosts: Vec::new(),
            credentials_encrypted: None,
            key_fingerprint: None,
            encryption_at_rest: false,
            enabled: true,
            company_id: company_id.map(str::to_string),
            created_by: "admin".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn test_object(owner_user: &str, company_id: Option<&str>) -> StoredObject {
        let now = Utc::now();
        StoredObject {
            bucket_id: Uuid::now_v7(),
            key: "a/b.txt".to_string(),
            size_bytes: 10,
            sha256_hex: None,
            mime: "text/plain".to_string(),
            owner_module: "files".to_string(),
            owner_user: owner_user.to_string(),
            company_id: company_id.map(str::to_string),
            archived: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn bucket_access_allows_admin_any_bucket_and_owner_company_only() {
        let bucket = test_bucket(Some("acme"));
        assert!(check_bucket_access(&bucket, Some("acme"), false).is_ok());
        assert!(check_bucket_access(&bucket, Some("other"), false).is_err());
        assert_eq!(check_bucket_access(&bucket, Some("other"), false).unwrap_err().code, 404);
        assert!(check_bucket_access(&bucket, None, false).is_err());
        assert!(check_bucket_access(&bucket, Some("other"), true).is_ok());
        // A company-less (shared) bucket passes the bucket check;
        // object-level scoping still applies below.
        assert!(check_bucket_access(&test_bucket(None), Some("other"), false).is_ok());
    }

    #[test]
    fn object_access_allows_owner_company_member_and_admin_only() {
        let object = test_object("u-1", Some("acme"));
        assert!(check_object_access(&object, "u-1", Some("other"), false).is_ok());
        assert!(check_object_access(&object, "u-2", Some("acme"), false).is_ok());
        assert!(check_object_access(&object, "u-2", Some("other"), false).is_err());
        assert_eq!(check_object_access(&object, "u-2", Some("other"), false).unwrap_err().code, 404);
        assert!(check_object_access(&object, "u-2", Some("other"), true).is_ok());
        // A personal object (no company) is visible only to its owner or an admin.
        let personal = test_object("u-1", None);
        assert!(check_object_access(&personal, "u-2", Some("acme"), false).is_err());
        assert!(check_object_access(&personal, "u-1", None, false).is_ok());
    }

    #[test]
    fn upload_company_forces_caller_company_for_non_admins() {
        assert_eq!(
            resolve_upload_company(Some("acme"), Some("acme"), false).unwrap(),
            Some("acme".to_string())
        );
        assert!(resolve_upload_company(Some("other"), Some("acme"), false).is_err());
        assert_eq!(
            resolve_upload_company(Some("other"), Some("acme"), true).unwrap(),
            Some("other".to_string())
        );
        assert_eq!(
            resolve_upload_company(None, Some("acme"), false).unwrap(),
            Some("acme".to_string())
        );
        assert_eq!(resolve_upload_company(None, None, false).unwrap(), None);
    }
}
