//! import export - auth.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use uuid::Uuid;
use super::events::PERM_IMPORT_EXPORT_MANAGE;

/// Column-name fragments that are never allowed in an import file.
/// Matching is case-insensitive on the normalized (lowercase,
/// non-alphanumeric-stripped) column name.
pub(crate) const FORBIDDEN_COLUMN_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "privatekey",
    "private_key",
    "credential",
    "hash",
    "salt",
    "totp",
    "otpsecret",
];

/// Root element names accepted for an XML import document.
pub(crate) const XML_ROOT_ELEMENTS: &[&str] = &["rows", "export", "items", "dataset", "records"];

/// Child element names accepted as one row inside an XML document.
pub(crate) const XML_ROW_ELEMENTS: &[&str] = &["row", "item", "record"];

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
/// (Same fail-closed shape as the AI module's capability check.)
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role": role, "permission": permission, "allowed": false }),
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

/// Parse one company source value as a UUID. Company ids in this
/// module are UUIDs; a source that carries a non-UUID value (or the
/// nil UUID, which is never a company) is treated as absent -- its
/// namespace is not this module's -- never as a company. The
/// fail-closed rule in `require_caller_company` still refuses
/// company-less non-manage callers outright.
fn parse_company_id(raw: &str) -> Option<Uuid> {
    Uuid::parse_str(raw.trim()).ok().filter(|id| !id.is_nil())
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both resolve to different
/// companies. The `X-Company-Id` header is NEVER trusted here: only
/// the verified caller (company resolved from the database at
/// authentication time) and the middleware-resolved tenant context
/// are sources. The `user.company_requested` hook remains a last
/// resort inside `require_caller_company` only (see there).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<Uuid>> {
    let tenant_company = tenant.company_id.as_deref().and_then(parse_company_id);
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .as_deref()
        .and_then(parse_company_id);
    match (tenant_company, verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company)),
        (None, verified) => Ok(verified),
    }
}
/// The caller's company id, resolved server-side only through the
/// `user.company_requested` hook: the answering module looks the
/// company up from the authenticated user id, so the caller cannot
/// choose their own tenant. The `X-Company-Id` header is
/// deliberately never read here: it is client-asserted, and
/// trusting it let any caller holding the import/export
/// permission claim any company id (or none) and read another
/// tenant's data. `None` when no handler answers the hook.
///
/// Last-resort source only: `require_caller_company` tries the
/// verified caller company (via `caller_company`) first and falls
/// back to this hook when neither the tenant context nor the
/// verified caller resolves a company.
///
/// The hook answer is validated against the caller: an answer is
/// accepted only when it carries no `user_id` or the caller's own
/// `user_id`, whether it arrives by mutating the payload (a
/// `company_id` key) or as a result object. A malformed answer,
/// or one naming another user, is ignored, never trusted.
///
/// The request is kept in the signature so every call site (and
/// the re-exported API) is unchanged; the headers it carries are
/// untrusted and play no part in tenant resolution.
pub(crate) fn resolve_caller_company(ctx: &AppContext, req: &HttpRequest, caller: &Caller) -> Option<Uuid> {
    let _ = req;
    let context = ctx.hooks.emit(
        "user.company_requested",
        serde_json::json!({ "user_id": caller.actor }),
    );
    let answer_belongs_to_caller = |user: Option<&str>| match user {
        None => true,
        Some(user) => user == caller.actor,
    };
    let parse_company = |value: Option<&serde_json::Value>| {
        value
            .and_then(|value| value.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
    };
    if answer_belongs_to_caller(context.get("user_id").and_then(|value| value.as_str())) {
        if let Some(company) = parse_company(context.get("company_id")) {
            return Some(company);
        }
    }
    for result in &context.results {
        if !answer_belongs_to_caller(result.get("user_id").and_then(|value| value.as_str())) {
            continue;
        }
        if let Some(company) = parse_company(result.get("company_id")) {
            return Some(company);
        }
    }
    None
}

/// Resolve the caller's company and fail closed for callers who
/// are not platform admins. Resolution order (C3 pilot 3): the
/// tenant context's company first, then the verified caller's
/// server-resolved company (`caller_company`), then the
/// `user.company_requested` hook as a last resort
/// (`resolve_caller_company`). A caller who holds the manage
/// capability may legitimately have no company (platform scope),
/// so `None` stays allowed for them. Any other caller with no
/// server-side-resolved company is refused with 403 instead of
/// being treated as a company-less admin who could see every
/// tenant's jobs, mappings, schedules, and exported rows.
///
/// Cross-module gap, by design: no module answers
/// `user.company_requested` today, so until the users module
/// registers an answerer that looks the company up from the
/// authenticated user id, every non-manage caller is refused
/// here rather than silently scoped to nothing or everything.
pub(crate) fn require_caller_company(
    ctx: &AppContext,
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    caller: &Caller,
) -> UnifiedResult<Option<Uuid>> {
    let company = caller_company(tenant, req)?
        .or_else(|| resolve_caller_company(ctx, req, caller));
    if company.is_none() && !check_capability(ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE) {
        return Err(UnifiedError::forbidden(
            "Caller has no company: the authenticated user belongs to no company resolvable through the tenant context, the verified caller, or user.company_requested, so tenant-scoped import/export is refused",
        ));
    }
    Ok(company)
}

/// True when a caller in `caller_company` may see a record owned by
/// `record_company`. A manage-capable caller sees everything; a
/// company caller only sees records of their own company, never
/// platform-wide records owned by nobody. A caller with no
/// resolved company sees nothing at all: the old `(None, _) =>
/// true` arm treated "no company" as platform admin, so any
/// permission holder who sent no company id could read every
/// tenant's records.
pub(crate) fn can_view_company(
    caller_company: Option<Uuid>,
    record_company: Option<Uuid>,
    can_manage: bool,
) -> bool {
    if can_manage {
        return true;
    }
    match (caller_company, record_company) {
        (Some(caller), Some(record)) => caller == record,
        (Some(_), None) | (None, _) => false,
    }
}

/// Keep only the records a caller may see. Used by every list
/// endpoint so tenant scoping cannot be forgotten per handler.
pub(crate) fn scope_visible<T: Clone>(
    items: Vec<T>,
    company_of: impl Fn(&T) -> Option<Uuid>,
    caller_company: Option<Uuid>,
    can_manage: bool,
) -> Vec<T> {
    items
        .into_iter()
        .filter(|item| can_view_company(caller_company, company_of(item), can_manage))
        .collect()
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
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;
    use std::sync::Arc;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_caller() -> Caller {
        Caller { actor: "user-1".to_string(), role: "member".to_string() }
    }

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
            unified_core::auth::Caller::verified(test_user_uuid("user-1"), Vec::new())
                .with_roles(vec!["member".to_string()]);
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    fn tenant_ctx(req: &HttpRequest) -> unified_core::tenancy::TenantContext {
        unified_core::tenancy::TenantContext::from_http_request(req)
            .expect("tenant context must resolve")
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "member")
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    #[test]
    fn can_view_company_no_company_caller_sees_nothing() {
        let other = Uuid::now_v7();
        assert!(!can_view_company(None, Some(other), false));
        assert!(!can_view_company(None, None, false));
    }

    #[test]
    fn can_view_company_company_caller_sees_only_own_company() {
        let own = Uuid::now_v7();
        let other = Uuid::now_v7();
        assert!(can_view_company(Some(own), Some(own), false));
        assert!(!can_view_company(Some(own), Some(other), false));
        assert!(!can_view_company(Some(own), None, false));
    }

    #[test]
    fn can_view_company_manage_caller_sees_everything() {
        let other = Uuid::now_v7();
        assert!(can_view_company(None, Some(other), true));
        assert!(can_view_company(Some(other), None, true));
        assert!(can_view_company(None, None, true));
    }

    #[test]
    fn scope_visible_keeps_only_the_callers_company() {
        let own = Uuid::now_v7();
        let other = Uuid::now_v7();
        let items = vec![Some(own), Some(other), None, Some(own)];
        let kept = scope_visible(items, |item| *item, Some(own), false);
        assert_eq!(kept, vec![Some(own), Some(own)]);
        assert!(scope_visible(vec![Some(own)], |item| *item, None, false).is_empty());
    }

    #[test]
    fn resolve_caller_company_ignores_the_company_header() {
        let claimed = Uuid::now_v7();
        let claimed_header = claimed.to_string();
        let req = {
            let req = TestRequest::default()
            .insert_header(("x-company-id", claimed_header.as_str()))
            .to_http_request();
            {
                use actix_web::HttpMessage;
                req.extensions_mut().insert(
                    unified_core::auth::Caller::verified(test_user_uuid("user-1"), Vec::new())
                        .with_roles(vec!["member".to_string()]),
                );
            }
            req
        };
        let ctx = test_context();
        assert_eq!(resolve_caller_company(&ctx, &req, &test_caller()), None);
    }

    #[test]
    fn resolve_caller_company_accepts_hook_payload_answer() {
        let company = Uuid::now_v7();
        let ctx = test_context();
        ctx.hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |context: &mut HookContext| {
                context.set("company_id", serde_json::Value::String(company.to_string()));
            }),
        );
        assert_eq!(
            resolve_caller_company(&ctx, &authed_request(), &test_caller()),
            Some(company)
        );
    }

    #[test]
    fn resolve_caller_company_accepts_hook_result_answer() {
        let company = Uuid::now_v7();
        let ctx = test_context();
        ctx.hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |context: &mut HookContext| {
                context.add_result(serde_json::json!({
                    "user_id": "user-1",
                    "company_id": company.to_string(),
                }));
            }),
        );
        assert_eq!(
            resolve_caller_company(&ctx, &authed_request(), &test_caller()),
            Some(company)
        );
    }

    #[test]
    fn resolve_caller_company_ignores_answer_for_another_user() {
        let company = Uuid::now_v7();
        let ctx = test_context();
        ctx.hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |context: &mut HookContext| {
                context.set("user_id", serde_json::Value::String("user-2".to_string()));
                context.set("company_id", serde_json::Value::String(company.to_string()));
                context.add_result(serde_json::json!({
                    "user_id": "user-2",
                    "company_id": company.to_string(),
                }));
            }),
        );
        assert_eq!(resolve_caller_company(&ctx, &authed_request(), &test_caller()), None);
    }

    #[test]
    fn require_caller_company_refuses_company_less_non_manage_caller() {
        let ctx = test_context();
        let req = tenant_request("", None);
        let tenant = tenant_ctx(&req);
        let error = require_caller_company(&ctx, &tenant, &req, &test_caller())
            .expect_err("no company must fail closed");
        assert!(error.message.contains("no company"));
    }

    #[test]
    fn require_caller_company_allows_manage_caller_without_company() {
        let ctx = allowing_context();
        let caller = Caller { actor: "admin-1".to_string(), role: "admin".to_string() };
        let req = {
            // Verified caller, plus a resolved tenant context
            // with no company: the gate resolves, the scope stays
            // platform (manage caller, no company).
            let req = TestRequest::default().to_http_request();
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
                unified_core::auth::Caller::verified(test_user_uuid("admin-1"), Vec::new())
                    .with_roles(vec!["admin".to_string()]),
            );
            req
        };
        let tenant = tenant_ctx(&req);
        assert_eq!(
            require_caller_company(&ctx, &tenant, &req, &caller).unwrap(),
            None
        );
    }

    #[test]
    fn require_caller_company_returns_hook_company_for_non_manage_caller() {
        let company = Uuid::now_v7();
        let ctx = test_context();
        ctx.hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |context: &mut HookContext| {
                context.set("company_id", serde_json::Value::String(company.to_string()));
            }),
        );
        let req = tenant_request("", None);
        let tenant = tenant_ctx(&req);
        assert_eq!(
            require_caller_company(&ctx, &tenant, &req, &test_caller()).unwrap(),
            Some(company)
        );
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

    fn tenant_ctx(req: &HttpRequest) -> unified_core::tenancy::TenantContext {
        unified_core::tenancy::TenantContext::from_http_request(req)
            .expect("tenant context must resolve")
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
        let company = Uuid::now_v7();
        let req = tenant_request(&company.to_string(), Some(&company.to_string()));
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_falls_back_to_verified_caller_company() {
        let company = Uuid::now_v7();
        let req = tenant_request("", Some(&company.to_string()));
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_tenant_and_verified_company_mismatch() {
        let req = tenant_request(
            &Uuid::now_v7().to_string(),
            Some(&Uuid::now_v7().to_string()),
        );
        let tenant = tenant_ctx(&req);
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_non_uuid_company_values() {
        // A tenant company that is not a UUID is not this module's
        // namespace: it resolves to no company here (the verified
        // caller company, a UUID, still wins).
        let company = Uuid::now_v7();
        let req = tenant_request("acme-corp", Some(&company.to_string()));
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
        let req = tenant_request("acme-corp", None);
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn caller_company_never_trusts_the_company_header() {
        // A forged X-Company-Id header grants nothing: with no
        // tenant company and no verified caller company, the scope
        // stays unresolved even when the header names a company.
        let claimed = Uuid::now_v7();
        let req = TestRequest::default()
            .insert_header(("x-company-id", claimed.to_string()))
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
        let tenant = tenant_ctx(&req);
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }
}
