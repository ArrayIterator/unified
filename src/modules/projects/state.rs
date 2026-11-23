// Projects module — shared state and common helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct ProjectsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) projects: DashMap<String, Project>,
    pub(crate) members: DashMap<String, ProjectMember>,
    pub(crate) boards: DashMap<String, ProjectBoard>,
    pub(crate) columns: DashMap<String, ProjectBoardColumn>,
    pub(crate) tasks: DashMap<String, ProjectTask>,
    pub(crate) dependencies: DashMap<String, ProjectTaskDependency>,
    pub(crate) comments: DashMap<String, ProjectTaskComment>,
    pub(crate) watchers: DashMap<String, ProjectTaskWatcher>,
    pub(crate) attachments: DashMap<String, ProjectTaskAttachment>,
    pub(crate) milestones: DashMap<String, ProjectMilestone>,
    pub(crate) sprints: DashMap<String, ProjectSprint>,
    pub(crate) phases: DashMap<String, ProjectPhase>,
    pub(crate) time_entries: DashMap<String, ProjectTimeEntry>,
    pub(crate) automation_rules: DashMap<String, ProjectAutomationRule>,
    pub(crate) templates: DashMap<String, ProjectTemplate>,
    pub(crate) cycles: DashMap<String, ProjectCycle>,
    pub(crate) program_increments: DashMap<String, ProjectProgramIncrement>,
    pub(crate) business_cases: DashMap<String, ProjectBusinessCase>,
    pub(crate) activity: DashMap<String, ProjectActivity>,
}

impl ProjectsState {
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            projects: DashMap::new(),
            members: DashMap::new(),
            boards: DashMap::new(),
            columns: DashMap::new(),
            tasks: DashMap::new(),
            dependencies: DashMap::new(),
            comments: DashMap::new(),
            watchers: DashMap::new(),
            attachments: DashMap::new(),
            milestones: DashMap::new(),
            sprints: DashMap::new(),
            phases: DashMap::new(),
            time_entries: DashMap::new(),
            automation_rules: DashMap::new(),
            templates: DashMap::new(),
            cycles: DashMap::new(),
            program_increments: DashMap::new(),
            business_cases: DashMap::new(),
            activity: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Send a project notification through the Notifications module.
    /// The existing `notification.requested` event is the only channel
    /// the notifications module listens to for ad-hoc messages.
    pub(crate) fn notify(&self, user_id: &str, title: String, body: String) {
        if user_id.trim().is_empty() {
            return;
        }
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id.trim(),
                "title": title,
                "body": body,
                "kind": "project",
            }),
        );
    }

    /// Ask the Roles module whether `role` holds `permission`
    /// (`role.check`). Fails closed: no answer means not allowed.
    pub(crate) fn check_capability(&self, role: &str, permission: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role": role, "permission": permission, "allowed": false }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub(crate) fn is_member(&self, project_id: &str, user_id: &str) -> bool {
        self.members
            .iter()
            .any(|entry| entry.value().project_id == project_id && entry.value().user_id == user_id)
    }

    pub(crate) fn get_project(&self, id: &str) -> UnifiedResult<Project> {
        self.projects
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project not found"))
    }

    pub(crate) fn get_task(&self, id: &str) -> UnifiedResult<ProjectTask> {
        self.tasks
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))
    }

    pub(crate) fn log_activity(
        &self,
        project_id: &str,
        actor_id: &str,
        action: &str,
        target_type: &str,
        target_id: Option<String>,
        details: Value,
    ) {
        let record = ProjectActivity {
            id: new_id(),
            project_id: project_id.to_string(),
            actor_id: actor_id.to_string(),
            action: action.to_string(),
            target_type: target_type.to_string(),
            target_id,
            details,
            created_at: now(),
        };
        self.activity.insert(record.id.clone(), record);
    }

    /// Notify a task's assignee plus every watcher, each user once.
    pub(crate) fn notify_task_watchers(&self, task: &ProjectTask, title: String, body: String) {
        let mut recipients: HashSet<String> = HashSet::new();
        if let Some(assignee_id) = &task.assignee_id {
            if !assignee_id.trim().is_empty() {
                recipients.insert(assignee_id.trim().to_string());
            }
        }
        for entry in self.watchers.iter() {
            if entry.value().task_id == task.id && !entry.value().user_id.trim().is_empty() {
                recipients.insert(entry.value().user_id.trim().to_string());
            }
        }
        for user_id in recipients {
            self.notify(&user_id, title.clone(), body.clone());
        }
    }
}

impl Default for ProjectsState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// Slice one page out of a filtered, sorted list, mirroring the
/// tickets module: `core::pagination` carries the parameter and
/// result types only, so the slicing helper lives here.
pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

/// Viewer identity from the request headers (`X-User-Id` /
/// `X-Role`), exactly like the risk, content, forms, and
/// appointments modules. Query parameters are never read for
/// identity: they are logged, cached, and bookmarked, so a claimed
/// role in the query string must not decide what the caller may do.
/// Returns (viewer id, viewer role); either side may be absent, and
/// every downstream check fails closed on an absent side: no role
/// means no capability, and no viewer means no ownership.
pub(crate) fn viewer_of(req: &HttpRequest) -> (Option<String>, Option<String>) {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        (
            Some(verified.user_id.clone()),
            verified.roles.first().cloned(),
        )
    } else {
        (None, None)
    }
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Fail when no role was supplied or the role lacks the permission.
pub(crate) fn require_capability(
    state: &ProjectsState,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    match role {
        Some(role) if state.check_capability(role, permission) => Ok(()),
        _ => Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))),
    }
}

/// Resolve the actor a write body may record (`author_id`,
/// `uploaded_by`, `user_id` on the created record). The recorded
/// actor is always the header caller (`viewer_of`): a body value
/// naming someone else is forgery and is rejected with 403,
/// EXCEPT when the caller holds `projects:manage` — the
/// admin/on-behalf flow, verified through the same
/// `require_capability` pattern as every other check. A missing
/// header identity fails closed the same way: with no caller to
/// match, only a manage holder may name an actor. An empty value
/// is a 400, matching the per-handler validation it replaces.
pub(crate) fn resolve_body_actor(
    state: &ProjectsState,
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    requested: &str,
    label: &str,
) -> UnifiedResult<String> {
    let requested = requested.trim().to_string();
    if requested.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{label} must not be empty"
        )));
    }
    if Some(requested.as_str()) == viewer_id {
        return Ok(requested);
    }
    if let Some(role) = viewer_role {
        if state.check_capability(role, "projects:manage") {
            return Ok(requested);
        }
    }
    Err(UnifiedError::forbidden(format!(
        "{label} must match the authenticated caller unless the caller holds projects:manage"
    )))
}

/// A viewer may see a project when they own it, are a member of it,
/// or hold the `projects:view` capability. Anything else is forbidden.
pub(crate) fn require_project_access(
    state: &ProjectsState,
    project: &Project,
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    scope: &Option<String>,
) -> UnifiedResult<()> {
    // Tenant partitioning comes first: a caller outside the project's
    // company sees nothing of it, even when they own it or hold the
    // view capability (C3 pilot 2; the shared CallerScope::Unresolved
    // rule — an unresolved caller reads global rows only).
    require_record_in_company_scope(&project.company_id, scope)?;
    if let Some(viewer_id) = viewer_id {
        if project.owner_id == viewer_id || state.is_member(&project.id, viewer_id) {
            return Ok(());
        }
    }
    if let Some(role) = viewer_role {
        if state.check_capability(role, "projects:view") {
            return Ok(());
        }
    }
    Err(UnifiedError::forbidden(
        "Viewer has no access to this project",
    ))
}


// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 2).
// ---------------------------------------------------------------------------

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
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is task 317's job elsewhere).
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
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3 task 320, rule A).
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
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
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

/// Project records carry `company_id: Option<String>`;
/// sub-records (tasks, boards, milestones, sprints, phases,
/// time entries, automation rules, templates, cycles, program
/// increments, business cases, activity) carry only a
/// `project_id` and inherit their scope from the parent project
/// (see `require_project_access`).


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
    use actix_web::test::TestRequest;

    fn test_project() -> Project {
        Project {
            id: "p1".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            methodology: "scrum".to_string(),
            status: "active".to_string(),
            visibility: "private".to_string(),
            owner_id: "owner-1".to_string(),
            company_id: None,
            client_id: None,
            start_date: None,
            due_date: None,
            color: None,
            icon: None,
            labels: Vec::new(),
            custom_field_definitions: serde_json::json!({}),
            archived: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn viewer_identity_comes_from_the_verified_caller_not_query_params() {
        let req = verified_request("user-1", "admin");
        assert_eq!(
            viewer_of(&req),
            (
                Some(test_user_uuid("user-1").to_string()),
                Some("admin".to_string())
            )
        );
        // A role claimed only in the query string is ignored:
        // query params are logged, cached, and bookmarked.
        let req = TestRequest::default()
            .uri("/api/v1/projects?viewer_id=spoofed&viewer_role=admin")
            .to_http_request();
        assert_eq!(viewer_of(&req), (None, None));
    }

    #[test]
    fn forged_admin_query_param_grants_no_capability_or_access() {
        let req = TestRequest::default()
            .uri("/api/v1/projects?viewer_id=spoofed&viewer_role=admin")
            .to_http_request();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let state = ProjectsState::new();
        assert!(require_capability(&state, viewer_role.as_deref(), "projects:view").is_err());
        assert!(require_project_access(
            &state,
            &test_project(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        &None,
        )
        .is_err());
    }

    /// A state whose `role.check` hook allows exactly the given
    /// permissions (all others denied), like the entitlements
    /// test context.
    fn allowing_state(allowed: &[&str]) -> ProjectsState {
        use unified_core::hooks::HookContext;
        let state = ProjectsState::new();
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        state.bus().register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        state
    }

    #[test]
    fn body_actor_matching_the_caller_is_recorded() {
        let state = ProjectsState::new();
        let resolved = resolve_body_actor(
            &state,
            Some("user-1"),
            Some("member"),
            "user-1",
            "author_id",
        )
        .expect("own id is allowed");
        assert_eq!(resolved, "user-1");
    }

    #[test]
    fn body_actor_naming_someone_else_is_forbidden_without_manage() {
        let state = allowing_state(&["projects:edit"]);
        let error = resolve_body_actor(
            &state,
            Some("user-1"),
            Some("member"),
            "someone-else",
            "author_id",
        )
        .unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn body_actor_naming_someone_else_is_allowed_with_manage() {
        let state = allowing_state(&["projects:edit", "projects:manage"]);
        let resolved = resolve_body_actor(
            &state,
            Some("user-1"),
            Some("admin"),
            "someone-else",
            "author_id",
        )
        .expect("manage holder may act on behalf");
        assert_eq!(resolved, "someone-else");
    }

    #[test]
    fn body_actor_fails_closed_without_a_verified_caller() {
        let state = allowing_state(&["projects:edit"]);
        let error =
            resolve_body_actor(&state, None, Some("member"), "someone-else", "author_id")
                .unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn body_actor_rejects_an_empty_value() {
        let state = ProjectsState::new();
        let error = resolve_body_actor(&state, Some("user-1"), Some("member"), "   ", "author_id")
            .unwrap_err();
        assert_eq!(error.code, 400);
    }
}
