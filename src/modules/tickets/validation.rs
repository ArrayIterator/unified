// Tickets module - validation helpers, SLA defaults, and status rules.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// Helpers

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

pub(crate) fn valid_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}

/// Default SLA targets in minutes (first response, resolution) when
/// no department policy answers.
pub(crate) fn default_sla_minutes(priority: &str) -> (u32, u32) {
    match priority {
        "urgent" => (60, 240),
        "high" => (240, 720),
        "low" => (1440, 4320),
        _ => (480, 1440),
    }
}

/// Canonical status values are `open`, `in_progress`, `waiting`,
/// `resolved`, `closed`. Human spellings are accepted as aliases so
/// API clients can use `in-progress` and `waiting-for-customer`.
pub(crate) fn normalize_status(input: &str) -> String {
    match input {
        "in-progress" => "in_progress".to_string(),
        "waiting-for-customer" | "waiting_for_customer" => "waiting".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn valid_status(status: &str) -> bool {
    matches!(
        status,
        "open" | "in_progress" | "waiting" | "resolved" | "closed"
    )
}

/// Allowed status transitions. `closed -> open` is a reopen.
/// Restating the current status is always allowed: a PATCH that
/// includes the ticket's unchanged status is a no-op, not a
/// transition, so it must not fail validation.
pub(crate) fn allowed_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        ("open", "in_progress")
            | ("open", "closed")
            | ("in_progress", "waiting")
            | ("in_progress", "resolved")
            | ("in_progress", "closed")
            | ("waiting", "in_progress")
            | ("waiting", "resolved")
            | ("waiting", "closed")
            | ("resolved", "closed")
            | ("resolved", "in_progress")
            | ("closed", "open")
    )
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// Caller authentication and capabilities

/// Capability strings checked through the `role.check` hook. Reads
/// use `tickets:read`; staff-wide views use `tickets:view_all`;
/// staff writes use `tickets:manage`.
pub(crate) const PERM_TICKETS_READ: &str = "tickets:read";
pub(crate) const PERM_TICKETS_VIEW_ALL: &str = "tickets:view_all";
pub(crate) const PERM_TICKETS_MANAGE: &str = "tickets:manage";

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
/// `authenticate` or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Ticket records carry `company_id: Option<String>`
/// (empty/missing means unscoped), so the scope helpers below
/// compare strings.
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
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
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
/// `CallerScope::Unresolved` rule). With a scope, only records carrying
/// exactly that company match, plus unscoped (global/legacy) records,
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


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook, via the module state. Fails closed: no
/// answer means not allowed.
pub(crate) fn require_permission(
    state: &TicketsState,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if state.check_capability(role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authenticate a request and require one capability, returning the
/// error response to send when either step fails. Handlers return it
/// directly so no unauthenticated request reaches handler logic.
pub(crate) fn authenticate(
    req: &HttpRequest,
    state: &TicketsState,
    permission: &str,
) -> Result<Caller, HttpResponse> {
    match caller(req).and_then(|caller| {
        require_permission(state, &caller.role, permission)?;
        Ok(caller)
    }) {
        Ok(caller) => Ok(caller),
        Err(error) => Err(json::error_response(&error, false)),
    }
}

/// Authenticate a request without requiring any capability; the
/// caller is then checked against the ticket or staff rules by the
/// handler. Returns the error response to send when the headers are
/// missing.
pub(crate) fn authenticate_caller(req: &HttpRequest) -> Result<Caller, HttpResponse> {
    caller(req).map_err(|error| json::error_response(&error, false))
}

/// The viewer's scope, derived from the authenticated caller only.
/// Query parameters never contribute identity: a caller who may view
/// everything sees all tickets, otherwise only their own tickets and
/// the departments they agent for.
pub(crate) fn caller_scope(
    state: &TicketsState,
    caller: &Caller,
) -> (String, bool, Vec<String>) {
    let view_all = state.check_capability(&caller.role, PERM_TICKETS_VIEW_ALL);
    let departments = if view_all {
        Vec::new()
    } else {
        state
            .department_ids_for_user(&caller.actor)
            .unwrap_or_default()
    };
    (caller.actor.clone(), view_all, departments)
}

/// True when the caller is staff for ticket purposes: they may view
/// everything, or they hold the staff write capability.
pub(crate) fn require_staff(state: &TicketsState, caller: &Caller) -> UnifiedResult<()> {
    if state.check_capability(&caller.role, PERM_TICKETS_VIEW_ALL)
        || state.check_capability(&caller.role, PERM_TICKETS_MANAGE)
    {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Only staff may access internal ticket data",
        ))
    }
}

/// True when the caller may write to a ticket: the ticket owner may
/// reply, comment, attach, and link on their own ticket; anyone else
/// needs the staff write capability.
pub(crate) fn require_ticket_access(
    state: &TicketsState,
    caller: &Caller,
    ticket: &Ticket,
) -> UnifiedResult<()> {
    if ticket.user_id == caller.actor
        || state.check_capability(&caller.role, PERM_TICKETS_MANAGE)
    {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Only the ticket owner or staff may change this ticket",
        ))
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
        let req = verified_request("alice", "agent");
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
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

    #[test]
    fn valid_priority_accepts_known_priorities_only() {
        for priority in ["low", "normal", "high", "urgent"] {
            assert!(valid_priority(priority), "{priority} should be valid");
        }
        assert!(!valid_priority("critical"));
        assert!(!valid_priority(""));
        assert!(!valid_priority("LOW"));
    }

    #[test]
    fn default_sla_minutes_depend_on_priority() {
        assert_eq!(default_sla_minutes("urgent"), (60, 240));
        assert_eq!(default_sla_minutes("high"), (240, 720));
        assert_eq!(default_sla_minutes("low"), (1440, 4320));
        assert_eq!(default_sla_minutes("normal"), (480, 1440));
        assert_eq!(default_sla_minutes("unknown"), (480, 1440));
    }

    #[test]
    fn normalize_status_maps_human_spellings() {
        assert_eq!(normalize_status("in-progress"), "in_progress");
        assert_eq!(normalize_status("waiting-for-customer"), "waiting");
        assert_eq!(normalize_status("waiting_for_customer"), "waiting");
        assert_eq!(normalize_status("open"), "open");
        assert_eq!(normalize_status("resolved"), "resolved");
    }

    #[test]
    fn valid_status_accepts_canonical_statuses_only() {
        for status in ["open", "in_progress", "waiting", "resolved", "closed"] {
            assert!(valid_status(status), "{status} should be valid");
        }
        assert!(!valid_status("pending"));
        assert!(!valid_status("in-progress"));
    }

    #[test]
    fn allowed_transition_covers_the_happy_paths() {
        assert!(allowed_transition("open", "in_progress"));
        assert!(allowed_transition("in_progress", "resolved"));
        assert!(allowed_transition("waiting", "in_progress"));
        assert!(allowed_transition("resolved", "closed"));
    }

    #[test]
    fn allowed_transition_rejects_skips_and_reopens_except_closed() {
        assert!(!allowed_transition("open", "resolved"));
        assert!(!allowed_transition("open", "waiting"));
        assert!(!allowed_transition("resolved", "open"));
        assert!(allowed_transition("closed", "open"));
    }

    #[test]
    fn allowed_transition_allows_restating_the_current_status() {
        // A PATCH that includes the ticket's unchanged status is a
        // no-op, not a transition, so it must not fail validation.
        for status in ["open", "in_progress", "waiting", "resolved", "closed"] {
            assert!(allowed_transition(status, status), "{status} -> {status} must be allowed");
        }
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
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
    fn require_permission_fails_closed_without_a_role_check_listener() {
        let state = TicketsState::new();
        assert!(!state.check_capability("admin", PERM_TICKETS_READ));
        assert!(require_permission(&state, "admin", PERM_TICKETS_READ).is_err());
        assert!(require_permission(&state, "admin", PERM_TICKETS_MANAGE).is_err());
    }

    #[test]
    fn caller_scope_derives_viewer_from_authenticated_caller() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-9", "admin");
        let call = caller(&req).unwrap();

        // A role check that allows everything makes the caller staff.
        let state = TicketsState::new();
        let handler: HookHandler = Arc::new(|ctx: &mut HookContext| {
            ctx.set("allowed", json!(true));
        });
        state.bus().register("role.check", "tickets-test", handler);
        let (viewer_id, view_all, departments) = caller_scope(&state, &call);
        assert_eq!(viewer_id, "user-9");
        assert!(view_all);
        assert!(departments.is_empty());

        // Without a role-check answer the caller is scoped to their
        // own tickets and the departments they agent for.
        let scoped = TicketsState::new();
        let membership: HookHandler = Arc::new(|ctx: &mut HookContext| {
            ctx.add_result(json!({ "department_ids": ["dept-1", "dept-2"] }));
        });
        scoped
            .bus()
            .register("department.membership_check", "tickets-test", membership);
        let (viewer_id, view_all, departments) = caller_scope(&scoped, &call);
        assert_eq!(viewer_id, "user-9");
        assert!(!view_all);
        assert_eq!(departments, vec!["dept-1".to_string(), "dept-2".to_string()]);
    }
}
