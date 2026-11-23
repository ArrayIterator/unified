// tasks — Validation helpers and small pure utilities.

use super::types::{TASK_ASSIGNED, TASK_UNASSIGNED};
use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, web};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same shape as the cards and
// documents modules: `X-User-Id` identifies the actor, `X-Role`
// carries the role slug the capability check runs against, `role.check`
// answers; no answer means not allowed — fail closed).
// ---------------------------------------------------------------------------

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
/// Task records carry `company_id: Option<Uuid>`, so the scope
/// helpers below compare UUIDs; a caller scope that is not a UUID
/// matches nothing (fail closed).
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

/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row.
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
        (None, caller) => Ok(caller.clone()),
    }
}

/// Uuid-aware form of `require_company_match`, for records whose
/// company is stored as `Option<Uuid>` (tasks) rather than a
/// string. The body company must equal the caller's scope when
/// both are present; when the body omits the company, the caller's
/// scope is adopted (parsed as a UUID — a non-UUID caller scope
/// fails closed, because it cannot be stored on a task).
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule).
pub(crate) fn require_company_match_uuid(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_uuid_for(&caller, body_company_id)
}

/// `require_company_match_uuid` against an already-resolved caller
/// scope. A `None` body value stays `None` (an explicit clear),
/// never re-stamped with the caller scope.
pub(crate) fn require_company_match_uuid_for(
    caller: &Option<String>,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match (body_company_id, caller) {
        (Some(body), Some(scope)) => match Uuid::parse_str(scope) {
            Ok(scope_id) if *body == scope_id => Ok(Some(*body)),
            _ => Err(UnifiedError::forbidden(
                "company_id does not match the caller's company scope",
            )),
        },
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(scope)) => match Uuid::parse_str(scope) {
            Ok(scope_id) => Ok(Some(scope_id)),
            Err(_) => Err(UnifiedError::forbidden(
                "The caller company scope is not a UUID, so it cannot be stored on a task",
            )),
        },
        (None, None) => Ok(None),
    }
}
pub(crate) fn in_company_scope(
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

/// Which assignment event (if any) an assignee change should emit:
/// `task.assigned` only when a new assignee is set and differs from
/// the old one, `task.unassigned` when a previously assigned task is
/// unassigned, and nothing when the assignee does not change
/// (same person re-assigned, or null assigned to an already
/// unassigned task).
pub(crate) fn assign_event_for(
    old_assignee: Option<Uuid>,
    new_assignee: Option<Uuid>,
) -> Option<&'static str> {
    match (old_assignee, new_assignee) {
        (old, Some(new)) if old != Some(new) => Some(TASK_ASSIGNED),
        (Some(_), None) => Some(TASK_UNASSIGNED),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

pub(crate) fn validate_title(title: &str) -> UnifiedResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("Title must not be empty"));
    }
    if title.chars().count() > 255 {
        return Err(UnifiedError::bad_request(
            "Title must be at most 255 characters long",
        ));
    }
    Ok(title.to_string())
}

pub(crate) fn validate_description(description: &str) -> UnifiedResult<()> {
    if description.chars().count() > 10_000 {
        return Err(UnifiedError::bad_request(
            "Description must be at most 10000 characters long",
        ));
    }
    Ok(())
}

pub(crate) fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > 20 {
        return Err(UnifiedError::bad_request("At most 20 tags are allowed"));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() {
            return Err(UnifiedError::bad_request("Tags must not be empty"));
        }
        if tag.chars().count() > 64 {
            return Err(UnifiedError::bad_request(
                "Each tag must be at most 64 characters long",
            ));
        }
        if !cleaned.contains(&tag) {
            cleaned.push(tag);
        }
    }
    Ok(cleaned)
}

pub(crate) fn validate_minutes(value: i32, field: &str) -> UnifiedResult<i32> {
    if value < 0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value)
}

pub(crate) fn client_ip(req: &HttpRequest) -> Option<String> {
    req.connection_info()
        .realip_remote_addr()
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

pub(crate) fn task_event_payload(task: &Task, ip: Option<&str>) -> Value {
    let mut payload = serde_json::json!({
        "id": task.id,
        "task_id": task.id,
        "title": task.title,
        "status": task.status.as_str(),
        "priority": task.priority,
        "assignee_id": task.assignee_id,
        "reporter_id": task.reporter_id,
        "company_id": task.company_id,
        "project_name": task.project_name,
        "entity_type": "task",
        "entity_id": task.id,
        "resource": format!("task:{}", task.id),
    });
    if let Some(ip) = ip {
        if let Value::Object(map) = &mut payload {
            map.insert("ip".to_string(), Value::String(ip.to_string()));
        }
    }
    if let Some(actor) = task.reporter_id {
        if let Value::Object(map) = &mut payload {
            map.insert("actor_id".to_string(), Value::String(actor.to_string()));
        }
    }
    payload
}

pub(crate) fn parse_date_filter(
    params: &PaginationParams,
    key: &str,
) -> UnifiedResult<Option<DateTime<Utc>>> {
    match params.filters.get(key) {
        Some(raw) => raw
            .parse::<DateTime<Utc>>()
            .map(Some)
            .map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Filter \"{key}\" must be an RFC 3339 date-time, got \"{raw}\""
                ))
            }),
        None => Ok(None),
    }
}

pub(crate) fn parse_uuid_filter(
    params: &PaginationParams,
    key: &str,
) -> UnifiedResult<Option<Uuid>> {
    match params.filters.get(key) {
        Some(raw) => raw.parse::<Uuid>().map(Some).map_err(|_| {
            UnifiedError::bad_request(format!("Filter \"{key}\" must be a UUID, got \"{raw}\""))
        }),
        None => Ok(None),
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

    use super::super::types::{PERM_TASKS_MANAGE, PERM_TASKS_READ};
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
        let req = verified_request("alice", "admin");
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_matches_uuid_company() {
        let company = uuid::Uuid::now_v7();
        assert!(in_company_scope(&Some(company), &Some(company.to_string())));
        assert!(!in_company_scope(
            &Some(uuid::Uuid::now_v7()),
            &Some(company.to_string())
        ));
        assert!(!in_company_scope(&None, &Some(company.to_string())));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(company), &None));
        assert!(!in_company_scope(
            &Some(company),
            &Some("not-a-uuid".to_string())
        ));
    }

    #[test]
    fn require_company_match_for_refuses_body_company_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).unwrap().is_none());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_company_match_uuid_for_refuses_body_company_from_unresolved_callers() {
        let company = uuid::Uuid::now_v7();
        assert!(require_company_match_uuid_for(&None, &Some(company)).is_err());
        assert!(require_company_match_uuid_for(&None, &None)
            .unwrap()
            .is_none());
        assert_eq!(
            require_company_match_uuid_for(&Some(company.to_string()), &Some(company))
                .unwrap(),
            Some(company)
        );
    }
    use std::sync::Arc;

    #[test]
    fn validate_title_trims_and_rejects_empty_and_long() {
        assert_eq!(validate_title("  Do the thing  ").unwrap(), "Do the thing");
        assert!(validate_title("").is_err());
        assert!(validate_title("   ").is_err());
        assert_eq!(validate_title(&"x".repeat(255)).unwrap().len(), 255);
        assert!(validate_title(&"x".repeat(256)).is_err());
    }

    #[test]
    fn validate_description_limit() {
        assert!(validate_description("").is_ok());
        assert!(validate_description(&"x".repeat(10_000)).is_ok());
        assert!(validate_description(&"x".repeat(10_001)).is_err());
    }

    #[test]
    fn validate_tags_normalises_dedupes_and_rejects() {
        assert_eq!(
            validate_tags(&[
                " Rust ".to_string(),
                "RUST".to_string(),
                "web".to_string()
            ])
            .unwrap(),
            vec!["rust".to_string(), "web".to_string()]
        );
        assert!(validate_tags(&["".to_string()]).is_err());
        assert!(validate_tags(&["   ".to_string()]).is_err());
        assert!(validate_tags(&["x".repeat(65)]).is_err());
        assert!(validate_tags(&vec!["t".to_string(); 21]).is_err());
    }

    #[test]
    fn validate_minutes_rejects_negative() {
        assert_eq!(validate_minutes(0, "estimate").unwrap(), 0);
        assert_eq!(validate_minutes(90, "estimate").unwrap(), 90);
        assert!(validate_minutes(-1, "estimate").is_err());
    }

    #[test]
    fn parse_uuid_filter_round_trips() {
        let mut params = PaginationParams::default();
        assert_eq!(parse_uuid_filter(&params, "assignee_id").unwrap(), None);
        let id = Uuid::new_v4();
        params.filters.insert("assignee_id".to_string(), id.to_string());
        assert_eq!(
            parse_uuid_filter(&params, "assignee_id").unwrap(),
            Some(id)
        );
        params.filters.insert("assignee_id".to_string(), "not-a-uuid".to_string());
        assert!(parse_uuid_filter(&params, "assignee_id").is_err());
    }

    #[test]
    fn parse_date_filter_round_trips() {
        let mut params = PaginationParams::default();
        assert_eq!(parse_date_filter(&params, "from").unwrap(), None);
        params
            .filters
            .insert("from".to_string(), "2026-01-01T00:00:00Z".to_string());
        let parsed = parse_date_filter(&params, "from").unwrap().unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-01-01T00:00:00+00:00");
        params.filters.insert("from".to_string(), "yesterday".to_string());
        assert!(parse_date_filter(&params, "from").is_err());
    }

    #[test]
    fn assign_event_for_only_emits_on_real_assignment_change() {
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        // Fresh assignment and reassignment to someone else.
        assert_eq!(assign_event_for(None, Some(alice)), Some(TASK_ASSIGNED));
        assert_eq!(
            assign_event_for(Some(alice), Some(bob)),
            Some(TASK_ASSIGNED)
        );
        // Re-assigning the same person is not an assignment.
        assert_eq!(assign_event_for(Some(alice), Some(alice)), None);
        // Unassigning a previously assigned task is its own event.
        assert_eq!(
            assign_event_for(Some(alice), None),
            Some(TASK_UNASSIGNED)
        );
        // Unassigning an already unassigned task changes nothing.
        assert_eq!(assign_event_for(None, None), None);
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        // No headers at all: unauthorized.
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        // Only one header: still unauthorized.
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        // Blank headers do not count.
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        // Both headers present: identity resolved.
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    /// Context whose `role.check` hook allows only role `admin` to
    /// hold the tasks permissions; every other role is denied, and
    /// an unanswered check fails closed.
    fn gated_context() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "tasks-test",
            Arc::new(|context| {
                let role = context
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let permission = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let allowed = role == "admin"
                    && (permission == PERM_TASKS_READ
                        || permission == PERM_TASKS_MANAGE);
                context.set("allowed", Value::Bool(allowed));
            }),
        );
        AppContext::new(
            AppConfig::default(),
            bus,
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn require_permission_fails_closed_without_a_grant() {
        let ctx = gated_context();
        assert!(require_permission(&ctx, "admin", PERM_TASKS_READ).is_ok());
        assert!(require_permission(&ctx, "admin", PERM_TASKS_MANAGE).is_ok());
        assert!(require_permission(&ctx, "member", PERM_TASKS_READ).is_err());
        assert!(require_permission(&ctx, "member", PERM_TASKS_MANAGE).is_err());
        assert!(require_permission(&ctx, "admin", "tasks:approve").is_err());
        assert!(require_permission(&ctx, "ghost", PERM_TASKS_READ).is_err());
    }

    #[test]
    fn require_permission_fails_closed_with_no_answerer() {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        assert!(require_permission(&ctx, "admin", PERM_TASKS_READ).is_err());
    }

}
