// Shared helpers: callers, validation, loaders.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::sharing::{AuditorScopeEntry, PublicLink};
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

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

/// Reject the call when the caller is an auditor account: auditors
/// are read-only always, including on this module's own routes.
/// The only auditor-allowed read in this module is
/// `GET /api/v1/auditors/me/scope`, which does not call this guard.
/// Any account found for the caller blocks the call — usable,
/// expired, or revoked alike. An expired (but not revoked) auditor
/// must not slip through: the account still identifies its holder
/// as an auditor, and auditors may never perform mutations here.
pub(crate) fn reject_auditor_caller(state: &AuditorsState, actor: &str) -> UnifiedResult<()> {
    if state.auditor_for_user(actor).is_some() {
        return Err(UnifiedError::forbidden(
            "Auditor accounts are read-only and may not perform this action",
        ));
    }
    Ok(())
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
/// The one exception is `get_public_by_token`: a token-authenticated
/// public route whose credential is the link token itself (see the
/// exclusion note in the pilot-3 results).
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


/// True when a stored record belongs to the caller's company
/// scope. Auditor scope entries carry `company_id: Option<Uuid>`;
/// the caller scope is the tenant/verifier company as a string.
/// With no caller scope (an unresolved caller) only global records
/// match: entries with no company are visible, every company-owned
/// entry is denied (the shared `CallerScope::Unresolved` rule).
/// With a scope, entries carrying exactly that company match, plus
/// global entries, which are shared content visible to any resolved scope (C3 task 320, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => {
            record_company_id.is_none()
                || record_company_id.map(|id| id.to_string()).as_deref()
                    == Some(company.as_str())
        }
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
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) => match Uuid::parse_str(caller) {
            Ok(scope) if *record == scope => Ok(()),
            _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
        },
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// True when an auditor account is visible to the caller: at least
/// one of its scope entries names a company in the caller's scope.
/// Auditor accounts carry no company of their own — their scopes
/// are the company boundary. An account with no scope entries
/// sees nothing and is visible to nobody.
pub(crate) fn account_in_scope(
    state: &AuditorsState,
    auditor_id: Uuid,
    scope: &Option<String>,
) -> bool {
    state
        .scope_of(auditor_id)
        .entries
        .iter()
        .any(|entry| in_company_scope(&entry.company_id, scope))
}

/// Fail closed (403) when an auditor account is outside the
/// caller's company scope, as judged by its scope entries.
pub(crate) fn require_account_in_scope(
    state: &AuditorsState,
    auditor_id: Uuid,
    scope: &Option<String>,
) -> UnifiedResult<()> {
    if account_in_scope(state, auditor_id, scope) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The auditor account belongs to a different company",
        ))
    }
}

/// Verify one scope entry's company at auditor-creation time. A
/// scoped caller may only grant scopes naming exactly their own
/// company: a global entry (`company_id: None`) would grant audit
/// power over every company and exceeds a scoped caller's
/// authority, so it is refused. An unresolved caller (the platform
/// control plane) may grant any scope, global or company-bound.
pub(crate) fn require_scope_entry_company(
    scope: &Option<String>,
    entry_company_id: &Option<Uuid>,
) -> UnifiedResult<()> {
    match (scope, entry_company_id) {
        (Some(company), Some(entry)) if entry.to_string() == *company => Ok(()),
        (Some(_), _) => Err(UnifiedError::forbidden(
            "scopes[].company_id must name the caller's own company",
        )),
        (None, _) => Ok(()),
    }
}

/// True when a public link is visible to the caller. Links carry
/// no company of their own — the target record's company lives in
/// the owning module, not here — so the creator is the company
/// proxy: a scoped caller sees only links they created, and may
/// only revoke those. An unresolved caller (the platform control
/// plane) may manage every link.
pub(crate) fn link_in_scope(
    link: &PublicLink,
    scope: &Option<String>,
    actor: &str,
) -> bool {
    match scope {
        None => true,
        Some(_) => link.created_by == actor,
    }
}

/// Fail closed (403) when a public link is outside the caller's
/// company scope, as judged by its creator.
pub(crate) fn require_link_in_scope(
    link: &PublicLink,
    scope: &Option<String>,
    actor: &str,
) -> UnifiedResult<()> {
    if link_in_scope(link, scope, actor) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The public link belongs to a different company",
        ))
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

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
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

/// Client IP for the access log: the real remote address when the
/// proxy headers are trusted by the server, else the peer address.
/// Only its SHA-256 hash is ever stored (see `access_attempt`).
pub(crate) fn client_ip(req: &HttpRequest) -> Option<String> {
    req.connection_info()
        .realip_remote_addr()
        .map(str::to_string)
        .or_else(|| req.peer_addr().map(|addr| addr.ip().to_string()))
}

pub(crate) fn user_agent(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

pub(crate) fn load_auditor(state: &AuditorsState, raw_id: &str) -> UnifiedResult<AuditorAccount> {
    let id = parse_uuid(raw_id, "auditor")?;
    state
        .auditors
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Auditor not found: {id}")))
}

pub(crate) fn load_link(state: &AuditorsState, raw_id: &str) -> UnifiedResult<PublicLink> {
    let id = parse_uuid(raw_id, "public link")?;
    state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Public link not found: {id}")))
}

pub(crate) fn validate_scope_input(input: &AuditorScopeInput) -> UnifiedResult<AuditorScopeEntry> {
    let module = require_text(&input.module, "scopes[].module", MAX_MODULE_LEN)?.to_lowercase();
    if !module
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "scopes[].module must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    let resource_kind =
        require_text(&input.resource_kind, "scopes[].resource_kind", MAX_KIND_LEN)?.to_lowercase();
    if !resource_kind
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "scopes[].resource_kind must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    Ok(AuditorScopeEntry {
        module,
        resource_kind,
        resource_id: input.resource_id,
        company_id: input.company_id,
        allow_export: input.allow_export,
    })
}

/// Queue the expiry job for one auditor through `scheduler.enqueue`
/// with kind `auditors.expire`. Returns `(scheduler_status,
/// scheduler_job_id)`, honestly reporting `declined` when the
/// scheduler does not know the kind yet (see the scheduler note in
/// the module header). The account is stored either way.
pub(crate) fn schedule_expiry(state: &AuditorsState, auditor: &AuditorAccount) -> (String, Option<String>) {
    let Some(bus) = state.bus() else {
        return ("declined".to_string(), None);
    };
    let answer = bus.emit(
        SCHEDULER_ENQUEUE,
        json!({
            "kind": "auditors.expire",
            "payload": {
                "auditor_id": auditor.id.to_string(),
                "run_at": auditor.expires_at.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => (
            "queued".to_string(),
            result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        ),
        _ => ("declined".to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(
        id: uuid::Uuid,
        user_id: &str,
        status: AuditorStatus,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> AuditorAccount {
        AuditorAccount {
            id,
            user_id: user_id.to_string(),
            label: "Test auditor".to_string(),
            email: "auditor@example.test".to_string(),
            is_auditor: true,
            auditor_of: "company-1".to_string(),
            status,
            expires_at,
            password_hash: "hash".to_string(),
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            created_at: expires_at,
            updated_at: expires_at,
        }
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_then_verified_caller() {
        use actix_web::HttpMessage;
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
        let req = build("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
        let req = build("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some("acme"));
        let req = build("acme", Some("other-company"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_shows_global_entries_to_scoped_callers() {
        let company = Uuid::now_v7();
        let scope = Some(company.to_string());
        assert!(in_company_scope(&Some(company), &scope));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &None));
    }

    #[test]
    fn require_scope_entry_company_refuses_global_scopes_from_scoped_callers() {
        let company = Uuid::now_v7();
        let scope = Some(company.to_string());
        assert!(require_scope_entry_company(&scope, &Some(company)).is_ok());
        assert!(require_scope_entry_company(&scope, &Some(Uuid::now_v7())).is_err());
        assert!(require_scope_entry_company(&scope, &None).is_err());
        assert!(require_scope_entry_company(&None, &None).is_ok());
        assert!(require_scope_entry_company(&None, &Some(Uuid::now_v7())).is_ok());
    }

    #[test]
    fn valid_email_accepts_and_rejects() {
        assert!(valid_email("auditor@example.test"));
        assert!(valid_email("a@b.co"));
        assert!(valid_email(&format!("{}@b.co", "a".repeat(249))));
        assert!(!valid_email(""));
        assert!(!valid_email("plain"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@@b.c"));
        assert!(!valid_email("@b.c"));
        assert!(!valid_email("a@.c"));
        assert!(!valid_email("a@b."));
        assert!(!valid_email("a b@c.d"));
        assert!(!valid_email(&format!("{}@b.co", "a".repeat(250))));
    }

    #[test]
    fn require_text_trims_and_validates() {
        assert_eq!(require_text("  label  ", "label", 200).unwrap(), "label");
        assert!(require_text("", "label", 200).is_err());
        assert!(require_text("   ", "label", 200).is_err());
        assert!(require_text("toolong", "label", 3).is_err());
    }

    #[test]
    fn parse_uuid_accepts_valid_and_rejects_invalid() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(parse_uuid(&id.to_string(), "auditor").unwrap(), id);
        assert!(parse_uuid("not-a-uuid", "auditor").is_err());
    }

    #[test]
    fn validate_scope_input_normalizes_and_validates() {
        let entry = validate_scope_input(&AuditorScopeInput {
            module: "Billing".to_string(),
            resource_kind: "Invoice".to_string(),
            resource_id: None,
            company_id: None,
            allow_export: true,
        })
        .expect("valid scope");
        assert_eq!(entry.module, "billing");
        assert_eq!(entry.resource_kind, "invoice");
        assert!(entry.allow_export);
        for (module, kind) in [
            ("has space", "project"),
            ("has.dot", "project"),
            ("project", "has space"),
            ("project", "has.dot"),
            ("", "project"),
            ("project", ""),
        ] {
            assert!(
                validate_scope_input(&AuditorScopeInput {
                    module: module.to_string(),
                    resource_kind: kind.to_string(),
                    resource_id: None,
                    company_id: None,
                    allow_export: false,
                })
                .is_err(),
                "module={module} kind={kind} must be rejected"
            );
        }
        assert!(validate_scope_input(&AuditorScopeInput {
            module: "a".repeat(65),
            resource_kind: "project".to_string(),
            resource_id: None,
            company_id: None,
            allow_export: false,
        })
        .is_err());
    }

    #[test]
    fn reject_auditor_caller_blocks_any_auditor_account() {
        let state = AuditorsState::default();
        let now = chrono::Utc::now();
        // A non-auditor is never blocked.
        assert!(reject_auditor_caller(&state, "plain-user").is_ok());
        // A usable auditor is blocked.
        let usable = account(
            uuid::Uuid::now_v7(),
            "auditor-1",
            AuditorStatus::Active,
            now + chrono::Duration::hours(1),
        );
        state.auditors.insert(usable.id, usable);
        assert!(reject_auditor_caller(&state, "auditor-1").is_err());
        // An expired auditor is blocked too: the account still
        // identifies its holder as an auditor, and auditors may
        // never perform mutations here.
        let expired = account(
            uuid::Uuid::now_v7(),
            "auditor-2",
            AuditorStatus::Active,
            now - chrono::Duration::hours(1),
        );
        state.auditors.insert(expired.id, expired);
        assert!(reject_auditor_caller(&state, "auditor-2").is_err());
        // A revoked auditor is blocked too.
        let revoked = account(
            uuid::Uuid::now_v7(),
            "auditor-3",
            AuditorStatus::Revoked,
            now + chrono::Duration::hours(1),
        );
        state.auditors.insert(revoked.id, revoked);
        assert!(reject_auditor_caller(&state, "auditor-3").is_err());
    }

    #[test]
    fn load_auditor_returns_inserted_account() {
        let state = AuditorsState::default();
        let now = chrono::Utc::now();
        let inserted = account(
            uuid::Uuid::now_v7(),
            "auditor-1",
            AuditorStatus::Active,
            now + chrono::Duration::hours(1),
        );
        state.auditors.insert(inserted.id, inserted.clone());
        let loaded = load_auditor(&state, &inserted.id.to_string()).expect("load inserted");
        assert_eq!(loaded.id, inserted.id);
        assert_eq!(loaded.user_id, "auditor-1");
        assert!(load_auditor(&state, &uuid::Uuid::now_v7().to_string()).is_err());
        assert!(load_auditor(&state, "not-a-uuid").is_err());
    }
}
