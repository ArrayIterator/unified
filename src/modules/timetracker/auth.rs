// timetracker — Caller identity, capability checks, and agent authentication.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use crate::modules::apikeys::{AuthenticatedKey, authenticate_key, key_has_scope, note_used};
use actix_web::HttpRequest;
use serde_json::{Value, json};
use std::time::Instant;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers: caller, permissions, agent auth, company scope
// ---------------------------------------------------------------------------

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

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Note: timetracker company-scoped records (teams, staff,
/// invites, settings) carry `company_id: Uuid` (required), so the
/// scope helpers below compare UUIDs. Time entries, timers,
/// activity samples and screenshots carry no company id: they are
/// user-scoped, and the tenant gate is their only partition (a
/// C11 follow-up if per-company entry partitioning is wanted).
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
        (None, verified) => Ok(verified.clone()),
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


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
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

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) no record
/// matches: timetracker records always carry a company (there
/// are no global rows). With a scope, only records carrying
/// exactly that company match. A caller scope that is not a UUID
/// matches nothing (fail closed): timetracker company ids are
/// UUIDs.
pub(crate) fn in_company_scope(record_company_id: Uuid, caller_company: &Option<String>) -> bool {
    match caller_company {
        None => false,
        Some(company) => Uuid::parse_str(company).map_or(false, |scope| scope == record_company_id),
    }
}

/// `in_company_scope` for records whose company is optional.
pub(crate) fn in_company_scope_opt(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => match Uuid::parse_str(company) {
            Ok(scope) => record_company_id == &Some(scope),
            Err(_) => false,
        },
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: Uuid,
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


/// The company the caller claims to act for, from the
/// `X-Company-Id` header. The value is only a claim: every use of
/// it goes through `require_company_member`, which verifies the
/// company exists and the caller is a member of it.
pub(crate) fn claimed_company_id(req: &HttpRequest) -> UnifiedResult<Uuid> {
    let raw = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            UnifiedError::bad_request("Missing X-Company-Id header: company-scoped calls must name their company")
        })?;
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid company id: {raw}")))
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// (Same fail-closed shape as the ai module's capability check.)
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
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

/// The presented API key: `Authorization: Bearer <key>` first,
/// then `X-Api-Key` (the mcp module's shape).
fn presented_key(req: &HttpRequest) -> Option<String> {
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(text) = header.to_str() {
            if let Some(key) = text.strip_prefix("Bearer ") {
                let key = key.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    req.headers()
        .get("x-api-key")
        .and_then(|header| header.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Authenticate an agent call: a valid, unrevoked, unexpired API
/// key scoped to `timetracker:<action>`, whose owner id becomes
/// the acting user. Agent calls are always scoped to the key's
/// owner: a key without an owner cannot act for anyone.
pub(crate) fn authenticate_agent(
    state: &TimeTrackerState,
    req: &HttpRequest,
    action: &str,
    rate_limit: u32,
) -> UnifiedResult<(AuthenticatedKey, String)> {
    let presented = presented_key(req).ok_or_else(|| {
        UnifiedError::unauthorized("Missing API key: send Authorization: Bearer <key> or X-Api-Key")
    })?;
    let authenticated = authenticate_key(&presented)?;
    if !key_has_scope(&authenticated.scopes, "timetracker", action) {
        return Err(UnifiedError::forbidden(format!(
            "API key is not scoped to timetracker:{action}; grant scope \"timetracker:{action}\" or \"timetracker:*\""
        )));
    }
    let owner_id = authenticated.owner_id.ok_or_else(|| {
        UnifiedError::forbidden("API key has no owner: agent calls must be owned by a user")
    })?;
    check_agent_rate(state, &authenticated.id.to_string(), rate_limit.min(authenticated.requests_per_minute))?;
    note_used(authenticated.id);
    Ok((authenticated, owner_id.to_string()))
}

/// Per-key per-minute rate limit for agent calls. Returns 429 with
/// `retry_after_secs` in metadata when the window is exhausted.
fn check_agent_rate(
    state: &TimeTrackerState,
    key_id: &str,
    limit: u32,
) -> UnifiedResult<()> {
    if limit == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut bucket = state
        .agent_buckets
        .entry(key_id.to_string())
        .or_insert((now, 0));
    if now.duration_since(bucket.0).as_secs() >= 60 {
        *bucket = (now, 1);
        return Ok(());
    }
    if bucket.1 >= limit {
        let retry_after = 60 - now.duration_since(bucket.0).as_secs();
        return Err(UnifiedError::new(
            format!("Agent rate limit exceeded for this API key ({limit} calls per minute)"),
            429,
        )
        .with_metadata("retry_after_secs", json!(retry_after)));
    }
    bucket.1 += 1;
    Ok(())
}

/// True when the company module confirms the company exists,
/// through the `company.exists` hook (the same hook the SaaS
/// module uses). Fail-closed: no answer means it does not exist.
fn company_exists(ctx: &AppContext, company_id: Uuid) -> bool {
    let answer = ctx.hooks.emit(
        "company.exists",
        json!({ "company_id": company_id.to_string() }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("exists"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Verify the caller is a member of the company they claim, and
/// that the company exists. The membership question goes out as
/// `timetracker.company.membership.check` with
/// `{"company_id", "user_id"}`, expecting `{"member": true}` in
/// the first hook result; this module answers it itself from its
/// own staff map and team memberships (see
/// `on_company_membership_check` in module.rs) — the company
/// module has no membership hook. Fail-closed on both halves: an
/// unknown company or an unanswered membership check is a 403,
/// never a pass.
pub(crate) fn require_company_member(
    ctx: &AppContext,
    user_id: &str,
    company_id: Uuid,
) -> UnifiedResult<()> {
    if !company_exists(ctx, company_id) {
        return Err(UnifiedError::forbidden(format!(
            "Company {company_id} does not exist"
        )));
    }
    let answer = ctx.hooks.emit(
        "timetracker.company.membership.check",
        json!({ "company_id": company_id.to_string(), "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("member").and_then(Value::as_bool) == Some(true) => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "Caller is not a member of this company",
        )),
    }
}

/// True when `manager_id` may view `user_id`'s tracked data: the
/// caller themself, or a manager whose team contains the user.
/// Team membership is asked through `timetracker.team.check` with
/// `{"manager_id", "user_id"}`, expecting `{"manages": true}`.
/// This module answers that check itself from its own teams,
/// team memberships, and staff manager relations (see
/// `on_team_check` in module.rs); there is no separate team
/// module. Fail-closed: no answer means not allowed.
fn team_check(ctx: &AppContext, manager_id: &str, user_id: &str) -> bool {
    let answer = ctx.hooks.emit(
        "timetracker.team.check",
        json!({ "manager_id": manager_id, "user_id": user_id }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("manages"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Authorise reading another user's time, activity, or usage: the
/// user themself, or a caller holding `timetracker:view_team`
/// whose team contains the user.
pub(crate) fn can_view_user(ctx: &AppContext, caller: &Caller, user_id: &str) -> UnifiedResult<()> {
    if caller.actor == user_id {
        return Ok(());
    }
    require_permission(ctx, &caller.role, PERM_VIEW_TEAM)?;
    if team_check(ctx, &caller.actor, user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "User is not on the caller's team",
        ))
    }
}

/// Authorise reading another user's screenshots: stricter than
/// `can_view_user` — it needs the dedicated screenshot capability
/// on top of the team check.
pub(crate) fn can_view_screenshots(
    ctx: &AppContext,
    caller: &Caller,
    user_id: &str,
) -> UnifiedResult<()> {
    if caller.actor == user_id {
        return Ok(());
    }
    require_permission(ctx, &caller.role, PERM_VIEW_TEAM_SCREENSHOTS)?;
    if team_check(ctx, &caller.actor, user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "User is not on the caller's team",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tenant context the tenant-resolution middleware would
    /// attach for a request on a tenant host
    /// (`unified_core::middleware::TenantContext`, the exact struct
    /// `tenancy::TenantContext::from_http_request` reads from the
    /// request extensions). The company is empty, so the resolved
    /// scope is unscoped.
    fn tenant_request() -> actix_web::HttpRequest {
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
            },
        );
        req
    }

        /// A request with a tenant context carrying `tenant_company`
    /// (empty = no tenant company), a verified caller carrying
    /// `verified_company` (empty = no company resolved), and a
    /// forged `X-Company-Id` header when `forged_header` is
    /// non-empty. The header must never affect the scope.
    fn scoped_request(
        tenant_company: &str,
        verified_company: &str,
        forged_header: &str,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let mut req = actix_web::test::TestRequest::default();
        if !forged_header.is_empty() {
            req = req.insert_header(("x-company-id", forged_header));
        }
        let req = req.to_http_request();
        if !verified_company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(verified_company.to_string()),
            );
        }
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
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = scoped_request("company-a", "company-a", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_uses_verified_caller_when_tenant_has_none() {
        let req = scoped_request("", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-b".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = scoped_request("company-a", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert!(super::caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = scoped_request("", "", "company-forged");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(super::caller_company(&tenant, &req).unwrap(), None);
    }

#[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_matches_uuid_company() {
        let company = uuid::Uuid::now_v7();
        assert!(in_company_scope(company, &Some(company.to_string())));
        assert!(!in_company_scope(
            uuid::Uuid::now_v7(),
            &Some(company.to_string())
        ));
        assert!(!in_company_scope(company, &None));
        assert!(!in_company_scope(company, &Some("not-a-uuid".to_string())));
        assert!(in_company_scope_opt(
            &Some(company),
            &Some(company.to_string())
        ));
        assert!(!in_company_scope_opt(&None, &Some(company.to_string())));
        assert!(in_company_scope_opt(&None, &None));
        assert!(!in_company_scope_opt(&Some(company), &None));
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
