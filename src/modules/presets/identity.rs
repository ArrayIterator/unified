//! Presets + Setup Wizard module. - Caller identity and capability checks.
//!
//! Caller identity comes from the request headers, exactly like the
//! logistics, curriculum, and compliance modules: `X-User-Id`
//! identifies the caller and `X-Role` names their role. Capability
//! checks go through the roles module's `role.check` hook and fail
//! closed (no answer means not allowed). The `actor` fields that
//! older clients still send in request bodies (`ActorInput`) are
//! untrusted payload data: they are never consulted for identity or
//! authorization, so a body claiming an admin role grants nothing.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

/// Caller identity resolved server-side from request headers.
/// `actor` is the `X-User-Id` header value; `role` is the `X-Role`
/// header value. Both are required: a request without them is
/// unauthenticated, never an anonymous caller with default rights.
pub(super) struct Caller {
    pub(super) actor: String,
    pub(super) role: String,
}

pub(super) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(super) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(super) fn require_permission(
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

/// Resolve the caller from the request headers and require
/// `permission` for them through the `role.check` hook. Returns the
/// caller on success so handlers can audit with the header actor
/// id. Any failure (missing headers, or the roles module denying
/// the permission) is an error response for the handler to return.
pub(super) fn require_caller_permission(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)?;
    Ok(caller)
}

/// Caller-supplied identity fields carried on write request bodies
/// for backward-compatible deserialization only.
///
/// These fields are UNTRUSTED input: they are never read for
/// identity or authorization. A body claiming
/// `{"viewer_role": "admin"}` grants nothing; only the `X-User-Id`
/// / `X-Role` headers (checked against the roles module) decide
/// what a caller may do. The fields stay in the shape so older
/// clients that still send them keep deserializing instead of
/// failing on unknown input.
#[derive(Debug, Deserialize)]
pub(super) struct ActorInput {
    #[serde(default)]
    pub(super) viewer_id: Option<String>,
    #[serde(default)]
    pub(super) viewer_role: Option<String>,
    #[serde(default)]
    pub(super) company_id: Option<String>,
}

pub(super) fn parse_company_id(value: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(value.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid company id \"{value}\" (expected a UUID)"))
    })
}

// Tenant edge guard (C3 pilot 3).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `require_caller_permission` or
/// any capability check.
pub(super) fn require_tenant(
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
/// middleware-resolved tenant context are sources. The
/// `ActorInput.company_id` body field is untrusted payload data
/// and is never a scope source.
pub(super) fn caller_company(
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

/// Verify a request body's `company_id` (`Option<Uuid>`) against the
/// caller's company scope and return the company id to store. Fails
/// closed (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
/// a write requires a resolved company scope. A caller scope that is not a UUID is refused the
/// same way (fail closed: application / wizard company ids are UUIDs).
pub(super) fn require_company_match_uuid(
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
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// Resolve the company a preset preview answers for: a `company_id`
/// query value other than the caller's scope is refused (403), as is
/// a named company from an unresolved caller; with no (or an
/// unparseable) query value the caller's own scope applies (an
/// unresolved caller previews with no company, as before).
pub(super) fn resolve_preview_company(
    scope: &Option<String>,
    query_company: Option<&String>,
) -> UnifiedResult<Option<Uuid>> {
    match query_company.and_then(|value| Uuid::parse_str(value).ok()) {
        Some(parsed) => match scope {
            Some(company) if parsed.to_string() == *company => Ok(Some(parsed)),
            _ => Err(UnifiedError::forbidden(
                "company_id does not match the caller's company scope",
            )),
        },
        None => Ok(scope
            .as_deref()
            .and_then(|company| Uuid::parse_str(company).ok())),
    }
}

/// True when a stored record with a REQUIRED company id belongs to
/// the caller's company scope. Applications and wizards always
/// carry a company (there are no global rows), so an unresolved
/// caller sees none of them and a scoped caller sees only their
/// own company's rows.
pub(super) fn in_company_scope_uuid(
    record_company_id: &Uuid,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => false,
        Some(company) => record_company_id.to_string() == *company,
    }
}

/// Fail closed (403) when a stored record with a required company
/// id is outside the caller's company scope.
pub(super) fn require_record_in_company_scope_uuid(
    record_company_id: &Uuid,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope_uuid(record_company_id, caller_company) {
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
pub(super) fn require_record_writable_in_company_scope_uuid(
    record_company_id: &Uuid,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match caller_company {
        Some(caller) => match Uuid::parse_str(caller) {
            Ok(scope) if scope == *record_company_id => Ok(()),
            _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
        },
        None => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
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

    /// A context whose `role.check` handler answers from a fixed
    /// allow-list of permissions, like the statuspage test helper.
    fn allowing_context(allowed: &[&str]) -> AppContext {
        let ctx = test_context();
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut unified_core::hooks::HookContext| {
                let permission = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                context.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        ctx
    }

    fn authed_request(role: &str) -> HttpRequest {
        verified_request("user-1", role)
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_then_verified_caller() {
        use actix_web::HttpMessage;
        let company = Uuid::now_v7().to_string();
        let build = |tenant_company: &str, caller_company: Option<&str>| {
            let req = actix_web::test::TestRequest::default().to_http_request();
            let verified = match caller_company {
                Some(company) => unified_core::auth::Caller::verified(
                    uuid::Uuid::now_v7(),
                    Vec::new(),
                )
                .with_company(company.to_string()),
                None => {
                    unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                }
            };
            req.extensions_mut().insert(verified);
            req.extensions_mut()
                .insert(unified_core::middleware::TenantContext {
                    tenant_id: uuid::Uuid::now_v7().to_string(),
                    company_id: tenant_company.to_string(),
                    site_id: String::new(),
                    kind: unified_core::middleware::TenantKind::Subdomain,
                    host: "tenant.example.test".to_string(),
                    scope: None,
                    product_id: None,
                    package_id: None,
                    product_subdomain_id: None,
                });
            req
        };
        let req = build(&company, Some("other-company"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        let req = build("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
        let req = build(&Uuid::now_v7().to_string(), Some(&Uuid::now_v7().to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_uuid_is_own_company_only() {
        let company = Uuid::now_v7();
        assert!(in_company_scope_uuid(&company, &Some(company.to_string())));
        assert!(!in_company_scope_uuid(&company, &Some(Uuid::now_v7().to_string())));
        // Applications and wizards always carry a company: there
        // are no global rows for an unresolved caller to see.
        assert!(!in_company_scope_uuid(&company, &None));
    }

    #[test]
    fn require_company_match_uuid_refuses_writes_from_unresolved_callers() {
        use actix_web::HttpMessage;
        let company = Uuid::now_v7();
        let build = |tenant_company: &str| {
            let req = actix_web::test::TestRequest::default().to_http_request();
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
            );
            req.extensions_mut()
                .insert(unified_core::middleware::TenantContext {
                    tenant_id: uuid::Uuid::now_v7().to_string(),
                    company_id: tenant_company.to_string(),
                    site_id: String::new(),
                    kind: unified_core::middleware::TenantKind::Subdomain,
                    host: "tenant.example.test".to_string(),
                    scope: None,
                    product_id: None,
                    package_id: None,
                    product_subdomain_id: None,
                });
            req
        };
        let req = build("");
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company_match_uuid(&tenant, &req, &Some(company)).is_err());
        assert!(require_company_match_uuid(&tenant, &req, &None).is_err());
        let req = build(&company.to_string());
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
    fn caller_requires_a_verified_caller() {
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let no_actor = TestRequest::default().to_http_request();
        assert!(caller(&no_actor).is_err());
        let blank = TestRequest::default().to_http_request();
        assert!(caller(&blank).is_err());
    }

    #[test]
    fn caller_reads_the_verified_caller() {
        let request = verified_request("user-1", "operator");
        let caller = caller(&request).expect("caller");
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "operator");
    }

    #[test]
    fn check_capability_fails_closed_without_a_role_check_answer() {
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", "presets:manage"));
        assert!(!check_capability(&ctx, "admin", "presets:view"));
        assert!(require_permission(&ctx, "admin", "presets:view").is_err());
    }

    #[test]
    fn check_capability_honours_the_role_check_answer() {
        let ctx = allowing_context(&["presets:view", "presets:apply"]);
        assert!(check_capability(&ctx, "operator", "presets:view"));
        assert!(check_capability(&ctx, "operator", "presets:apply"));
        assert!(!check_capability(&ctx, "operator", "presets:manage"));
        assert!(require_permission(&ctx, "operator", "presets:apply").is_ok());
        assert!(require_permission(&ctx, "operator", "presets:manage").is_err());
    }

    #[test]
    fn require_caller_permission_needs_a_verified_caller_and_permission() {
        let ctx = allowing_context(&["presets:manage"]);
        // No verified caller at all: unauthenticated, even though the
        // (absent) body could have claimed any role.
        assert!(require_caller_permission(
            &ctx,
            &TestRequest::default().to_http_request(),
            "presets:manage"
        )
        .is_err());
        let request = authed_request("operator");
        let denied = require_caller_permission(&ctx, &request, "presets:view");
        assert!(denied.is_err());
        let allowed = require_caller_permission(&ctx, &request, "presets:manage")
            .expect("operator is allowed presets:manage by this test hook");
        assert_eq!(allowed.actor, test_user_uuid("user-1").to_string());
        assert_eq!(allowed.role, "operator");
    }
}
