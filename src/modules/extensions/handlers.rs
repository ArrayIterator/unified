//! extensions - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::{error_response, success_response};
use unified_core::modules::DependencyReport;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::service_monitor::{
    aggregate_external_requirements,
    latest_library_results,
    library_notice_message,
    run_library_checks,
    LibraryRecheckRequest,
};
use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use super::module::*;
use super::types::*;
use super::service::*;

// Caller identity and capability checks (same shape as the calculator
// and pricing modules: `X-User-Id` + `X-Role` headers, capability
// asked through the `role.check` hook, failing closed when no handler
// answers). Enabling and disabling modules is admin-only: it changes
// which modules serve requests, and a forced disable also disables
// every module that depends on the target.

/// Capability required to enable or disable a module. This reuses the
/// existing `modules:manage` capability (already listed in the roles
/// module's known capabilities and used by the dashboards module),
/// so no new capability string needs seeding: only the admin role
/// holds it, through its `"*": ["*"]` wildcard, and no seeded
/// non-admin role is granted the `modules` resource.
pub(crate) const PERM_EXTENSIONS_MANAGE: &str = "modules:manage";

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

pub(crate) async fn list_extensions(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let enabled_filter = pagination
        .filters
        .get("enabled")
        .and_then(|value| value.parse::<bool>().ok());
    let core_filter = pagination
        .filters
        .get("is_core")
        .and_then(|value| value.parse::<bool>().ok());
    let warnings_filter = pagination
        .filters
        .get("has_warnings")
        .and_then(|value| value.parse::<bool>().ok());
    let library_warnings_filter = pagination
        .filters
        .get("has_library_warnings")
        .and_then(|value| value.parse::<bool>().ok());

    let mut items: Vec<ExtensionInfo> = ctx
        .modules
        .list()
        .iter()
        .map(|module| build_extension_info(&ctx.modules, &enabled_state, module))
        .filter(|info| {
            if let Some(search) = &search {
                let haystack = format!("{} {} {}", info.id, info.name, info.description)
                    .to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            if let Some(enabled) = enabled_filter {
                if info.enabled != enabled {
                    return false;
                }
            }
            if let Some(is_core) = core_filter {
                if info.is_core != is_core {
                    return false;
                }
            }
            if let Some(has_warnings) = warnings_filter {
                if info.warnings.is_empty() == has_warnings {
                    return false;
                }
            }
            if let Some(has_library_warnings) = library_warnings_filter {
                if info.library_notices.is_empty() == has_library_warnings {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

pub(crate) async fn get_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match ctx.modules.get(&key) {
        Some(found) => success_response(
            build_extension_info(&ctx.modules, &enabled_state, &found),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Module \"{key}\" not found")),
            debug,
        ),
    }
}

pub(crate) async fn get_requirements(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match ctx.modules.get(&key) {
        Some(found) => success_response(
            build_requirement_check(&ctx.modules, &enabled_state, &found),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Module \"{key}\" not found")),
            debug,
        ),
    }
}

pub(crate) async fn list_dependencies(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let report: DependencyReport = ctx.modules.validate_dependencies();
    success_response(report, 200)
}

/// One external library requirement across all modules, with the
/// latest check result for it. `status` is `"ok"`, `"missing"`,
/// `"outdated"`, or `"unchecked"` (no check has run yet).
#[derive(Debug, Clone, Serialize)]
pub struct LibraryRequirementInfo {
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<String>,
    pub status: String,
    pub required_by: Vec<String>,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// List every external library requirement declared by any module,
/// with the latest local check result per library (paginated, with
/// `?search=` and `?status=` filters). This is the module-facing
/// view of the same check results the Service Monitor owns; the
/// results themselves live in the Service Monitor's cache and are
/// read, not copied, so the two endpoints can never disagree.
pub(crate) async fn list_library_requirements(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let status_filter = pagination.filters.get("status").cloned();
    let results = latest_library_results();
    let mut items: Vec<LibraryRequirementInfo> =
        aggregate_external_requirements(&ctx.modules)
            .into_iter()
            .map(|requirement| {
                let found = results.iter().find(|result| {
                    result.library.eq_ignore_ascii_case(&requirement.name)
                });
                let (status, found_version, message, checked_at) = match found {
                    Some(result) => (
                        result.status.clone(),
                        result.found_version.clone(),
                        library_notice_message(result),
                        Some(result.checked_at),
                    ),
                    None => ("unchecked".to_string(), None, None, None),
                };
                LibraryRequirementInfo {
                    library: requirement.name,
                    required_version: requirement.minimum_version,
                    found_version,
                    status,
                    required_by: requirement.required_by,
                    reasons: requirement.reasons,
                    message,
                    checked_at,
                }
            })
            .filter(|info| {
                if let Some(search) = &search {
                    let haystack = format!(
                        "{} {} {}",
                        info.library,
                        info.required_by.join(" "),
                        info.reasons.join(" ")
                    )
                    .to_lowercase();
                    if !haystack.contains(search.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &info.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
    match pagination.sort.as_deref().unwrap_or("library") {
        "status" => items.sort_by(|a, b| {
            a.status
                .cmp(&b.status)
                .then_with(|| a.library.to_lowercase().cmp(&b.library.to_lowercase()))
        }),
        _ => items.sort_by(|a, b| a.library.to_lowercase().cmp(&b.library.to_lowercase())),
    }
    if pagination.order == "desc" {
        items.reverse();
    }
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

/// Re-check action for the module list: runs the Service Monitor's
/// library checks (same function, same report shape as
/// `POST /api/v1/service-monitor/libraries/check`) so the module
/// list's notices refresh without a second implementation.
/// Admin-only: the caller is identified and checked for
/// `modules:manage` before any check runs, like `enable_extension`,
/// so an unauthenticated or unauthorized caller cannot trigger a
/// check run.
pub(crate) async fn recheck_library_requirements(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<LibraryRecheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EXTENSIONS_MANAGE) {
        return error_response(&error, debug);
    }
    let request = body.into_inner();
    let ctx_clone = Arc::clone(&ctx);
    let checked = web::block(move || run_library_checks(&ctx_clone, &request)).await;
    match checked {
        Ok(Ok(reports)) => success_response(reports, 200),
        Ok(Err(error)) => error_response(&error, debug),
        Err(error) => error_response(
            &UnifiedError::internal(format!("Library check failed: {error}")),
            debug,
        ),
    }
}

/// Enable one module (and its requirements, transitively).
/// Admin-only: the caller is identified and checked for
/// `modules:manage` before any state is read or changed, so a denied
/// caller learns nothing about the module and changes nothing.
pub(crate) async fn enable_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EXTENSIONS_MANAGE) {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    match enable_with_dependencies(&ctx, &enabled_state, &key) {
        Ok(response) => success_response(response, 200),
        Err(error) => error_response(&error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ForceQuery {
    #[serde(default)]
    force: Option<bool>,
}

/// Disable one module. Admin-only, like `enable_extension`: the
/// caller is identified and checked for `modules:manage` before any
/// state is read or changed. The check covers `?force=true` as well,
/// which transitively disables every dependent module.
pub(crate) async fn disable_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ForceQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EXTENSIONS_MANAGE) {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let force = query.force.unwrap_or(false);
    match disable_with_check(&ctx, &enabled_state, &key, force) {
        Ok(response) => success_response(response, 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn list_hooks(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());

    let mut by_event: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (event, owner) in ctx.hooks.registrations() {
        by_event.entry(event).or_default().push(owner);
    }
    let mut events: Vec<HookEventInfo> = by_event
        .into_iter()
        .map(|(event, owners)| {
            let handler_count = owners.len();
            let mut owners = owners;
            owners.sort();
            owners.dedup();
            HookEventInfo {
                event,
                owners,
                handler_count,
            }
        })
        .filter(|info| {
            if let Some(search) = &search {
                info.event.to_lowercase().contains(search.as_str())
            } else {
                true
            }
        })
        .collect();
    events.sort_by(|a, b| a.event.cmp(&b.event));
    let total = events.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(events.len());
    let items = if start < events.len() {
        events[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
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
    use unified_core::modules::{Module, ModuleRegistry};
    use crate::modules::service_monitor::latest_library_reports;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use dashmap::DashMap;

    struct DummyModule {
        id: &'static str,
        requires: &'static [&'static str],
    }

    impl Module for DummyModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            "Dummy"
        }

        fn description(&self) -> &'static str {
            "Test dummy module"
        }

        // Non-core, so the disable tests exercise the disable path
        // instead of the core-module refusal (`is_core` defaults to
        // `true` in the `Module` trait).
        fn is_core(&self) -> bool {
            false
        }

        fn requires(&self) -> &[&'static str] {
            self.requires
        }
    }

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context with a `dummy` module and a `child` module that
    /// requires it, so enable auto-activation and force-disable of
    /// dependents are both exercisable.
    fn context_with_modules() -> AppContext {
        let ctx = test_context();
        ctx.modules
            .register(Arc::new(DummyModule { id: "dummy", requires: &[] }));
        ctx.modules.register(Arc::new(DummyModule {
            id: "child",
            requires: &["dummy"],
        }));
        ctx
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = context_with_modules();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn fresh_state() -> (EnabledState, web::Data<EnabledState>) {
        let state: EnabledState = Arc::new(DashMap::new());
        (state.clone(), web::Data::new(state))
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&authed_request()).unwrap();
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
        // preset `"allowed": false` must stand and the call is refused
        // -- even for the admin role.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_EXTENSIONS_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_EXTENSIONS_MANAGE).is_err());
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = allowing_context();
        assert!(require_permission(&ctx, "admin", PERM_EXTENSIONS_MANAGE).is_ok());
    }

    #[test]
    fn enable_extension_rejects_anonymous_callers() {
        let (state, state_data) = fresh_state();
        let response = enable_extension(
            data(context_with_modules()),
            state_data,
            TestRequest::default().to_http_request(),
            web::Path::from("dummy".to_string()),
        );
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Nothing changed: the denial happens before any state read.
        assert!(!state.contains_key("dummy"));
    }

    #[test]
    fn enable_extension_rejects_callers_without_permission() {
        let (state, state_data) = fresh_state();
        state.insert("dummy".to_string(), false);
        let response = enable_extension(
            data(context_with_modules()),
            state_data,
            authed_request(),
            web::Path::from("dummy".to_string()),
        );
        // Headers present, but no `role.check` handler answers: the
        // guard fails closed and the module stays disabled.
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(*state.get("dummy").unwrap(), false);
    }

    #[test]
    fn enable_extension_accepts_an_authorized_admin() {
        let ctx = allowing_context();
        let registry = Arc::clone(&ctx.modules);
        let (state, state_data) = fresh_state();
        state.insert("dummy".to_string(), false);
        assert!(!is_enabled(&state, &registry, "dummy"));
        let response = enable_extension(
            data(ctx),
            state_data,
            authed_request(),
            web::Path::from("dummy".to_string()),
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert!(is_enabled(&state, &registry, "dummy"));
    }

    #[test]
    fn enable_extension_returns_not_found_for_unknown_id_when_authorized() {
        let (_state, state_data) = fresh_state();
        let response = enable_extension(
            data(allowing_context()),
            state_data,
            authed_request(),
            web::Path::from("ghost".to_string()),
        );
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn disable_extension_rejects_anonymous_callers() {
        let (_state, state_data) = fresh_state();
        let response = disable_extension(
            data(context_with_modules()),
            state_data,
            TestRequest::default().to_http_request(),
            web::Path::from("dummy".to_string()),
            web::Query(ForceQuery { force: Some(true) }),
        );
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn disable_extension_rejects_callers_without_permission() {
        let (state, state_data) = fresh_state();
        state.insert("dummy".to_string(), true);
        let response = disable_extension(
            data(context_with_modules()),
            state_data,
            authed_request(),
            web::Path::from("dummy".to_string()),
            web::Query(ForceQuery { force: Some(true) }),
        );
        // Even `?force=true` must not disable anything for a caller
        // without the permission.
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(*state.get("dummy").unwrap(), true);
    }

    #[test]
    fn disable_extension_accepts_an_authorized_admin() {
        let ctx = allowing_context();
        let registry = Arc::clone(&ctx.modules);
        let (state, state_data) = fresh_state();
        state.insert("dummy".to_string(), true);
        let response = disable_extension(
            data(ctx),
            state_data,
            authed_request(),
            web::Path::from("dummy".to_string()),
            web::Query(ForceQuery { force: None }),
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!is_enabled(&state, &registry, "dummy"));
    }

    #[test]
    fn disable_extension_force_disables_dependents_only_for_an_authorized_admin() {
        let ctx = allowing_context();
        let registry = Arc::clone(&ctx.modules);
        let (state, state_data) = fresh_state();
        state.insert("dummy".to_string(), false);
        state.insert("child".to_string(), false);
        let ctx_data = data(ctx);

        // Enabling the child auto-activates its requirement.
        let response = enable_extension(
            ctx_data.clone(),
            state_data.clone(),
            authed_request(),
            web::Path::from("child".to_string()),
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert!(is_enabled(&state, &registry, "dummy"));
        assert!(is_enabled(&state, &registry, "child"));

        // A caller without the permission cannot force-disable the
        // requirement -- or its dependents -- even with `?force=true`.
        let response = disable_extension(
            data(context_with_modules()),
            state_data.clone(),
            authed_request(),
            web::Path::from("dummy".to_string()),
            web::Query(ForceQuery { force: Some(true) }),
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(is_enabled(&state, &registry, "dummy"));
        assert!(is_enabled(&state, &registry, "child"));

        // The authorized admin force-disables both, dependents first.
        let response = disable_extension(
            ctx_data,
            state_data,
            authed_request(),
            web::Path::from("dummy".to_string()),
            web::Query(ForceQuery { force: Some(true) }),
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!is_enabled(&state, &registry, "dummy"));
        assert!(!is_enabled(&state, &registry, "child"));
    }

    /// How many library check reports the Service Monitor has
    /// cached. A check run stores one local report here, so a denied
    /// call must leave this count unchanged -- that is the observable
    /// proof `run_library_checks` never ran. The count (not
    /// emptiness) is compared because another test in this process
    /// may already have stored a report.
    fn cached_report_count() -> usize {
        latest_library_reports().len()
    }

    #[tokio::test]
    async fn recheck_library_requirements_rejects_anonymous_callers() {
        let before = cached_report_count();
        let response = recheck_library_requirements(
            data(context_with_modules()),
            TestRequest::default().to_http_request(),
            web::Json(LibraryRecheckRequest::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // No check ran: the cached reports are untouched.
        assert_eq!(cached_report_count(), before);
    }

    #[tokio::test]
    async fn recheck_library_requirements_rejects_callers_without_permission() {
        let before = cached_report_count();
        let response = recheck_library_requirements(
            data(context_with_modules()),
            authed_request(),
            web::Json(LibraryRecheckRequest::default()),
        )
        .await;
        // Headers present, but no `role.check` handler answers: the
        // guard fails closed and no library check runs.
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(cached_report_count(), before);
    }

    #[tokio::test]
    async fn recheck_library_requirements_accepts_an_authorized_admin() {
        let response = recheck_library_requirements(
            data(allowing_context()),
            authed_request(),
            web::Json(LibraryRecheckRequest::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        // The check ran and its local report is cached.
        assert!(!latest_library_reports().is_empty());
    }
}
