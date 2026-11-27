// Dashboards - Capability resolution, overviews and page data.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, StoreOrder, User};
use unified_core::db::money::pg_numeric_to_minor;
use unified_core::tenancy::TenantContext;
use diesel::data_types::PgNumeric;
use unified_core::db::DbPool;
use crate::modules::roles::permission_allowed;
use crate::themes::variables::{PageInfo, UserInfo, VariableContext};
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use super::{ALL_CAPABILITIES, CAP_ADMIN_ACCESS, CAP_RESELLER_ACCESS, CAP_USER_ACCESS, CallerContext,
    CallerIdentity, DashboardsState, EVENT_CAPABILITIES_REQUESTED, EVENT_SECTION_REQUESTED,
    MAX_VIEW_AS_TTL_MINUTES, MIN_VIEW_AS_TTL_MINUTES, OrderRow, has_capability, nav_items,
    require_capability};

// Database access (blocking Diesel work runs on the blocking pool)

pub(crate) fn db_pool(ctx: &AppContext) -> UnifiedResult<DbPool> {
    ctx.db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new(
                "Database is not configured: this dashboard section needs a database connection",
                503,
            )
            .with_metadata("kind", json!("database_not_configured"))
        })
}

/// Run blocking Diesel work off the async worker threads.
pub(crate) async fn run_blocking<F, T>(pool: DbPool, work: F) -> UnifiedResult<T>
where
    F: FnOnce(&mut PgConnection) -> UnifiedResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        work(&mut connection)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Database task failed: {error}")))?
}

/// Resolve the caller's capabilities: the JWT scopes unioned with the
/// permissions of every role assigned to the user in the database.
/// Returns the user profile as well. When no database pool is
/// configured, the `dashboard.capabilities_requested` hook is emitted
/// and any contributed `capabilities` arrays are merged in; the
/// profile is then unavailable and only the token scopes apply.
pub(crate) async fn resolve_capabilities(
    ctx: &AppContext,
    identity: &CallerIdentity,
) -> UnifiedResult<(HashSet<String>, Option<User>)> {
    let mut capabilities: HashSet<String> = HashSet::new();
    for scope in &identity.token_scopes {
        let scope = scope.trim();
        if scope.is_empty() {
            continue;
        }
        if scope == "*" {
            capabilities.insert("*".to_string());
        } else if scope.contains(':') {
            capabilities.insert(scope.to_string());
        }
    }
    // Any authenticated caller is a user of their own dashboard.
    capabilities.insert(CAP_USER_ACCESS.to_string());
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            let hook = ctx.hooks.emit(
                EVENT_CAPABILITIES_REQUESTED,
                json!({ "user_id": identity.user_id.to_string() }),
            );
            for result in &hook.results {
                if let Some(list) = result.get("capabilities").and_then(|value| value.as_array()) {
                    for item in list {
                        if let Some(capability) = item.as_str() {
                            if capability == "*" || capability.contains(':') {
                                capabilities.insert(capability.to_string());
                            }
                        }
                    }
                }
            }
            return Ok((expand_wildcard(capabilities), None));
        }
    };
    let user_id = identity.user_id;
    let (profile, permissions) = run_blocking(pool, move |connection| {
        use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
        use unified_core::db::schema::users::dsl as users_dsl;
        let profile = users_dsl::users
            .find(user_id)
            .select(User::as_select())
            .first::<User>(connection)
            .optional()
            .map_err(UnifiedError::from)?;
        let Some(profile) = profile else {
            return Err(UnifiedError::unauthorized(
                "Authenticated user does not exist",
            )
            .with_metadata("kind", json!("unknown_user")));
        };
        if !profile.is_active {
            return Err(UnifiedError::unauthorized("User account is disabled")
                .with_metadata("kind", json!("user_disabled")));
        }
        let role_ids: Vec<Uuid> = user_roles_dsl::user_roles
            .filter(user_roles_dsl::user_id.eq(user_id))
            .select(user_roles_dsl::role_id)
            .load::<Uuid>(connection)
            .map_err(UnifiedError::from)?;
        // Role permissions are `jsonb`, but Diesel here has no
        // `serde_json` feature, so `Value` cannot be loaded from the
        // column directly (nothing else in the codebase does so
        // either). Read the JSON as text and parse it instead.
        let mut permissions: Vec<Value> = Vec::new();
        if !role_ids.is_empty() {
            #[derive(Debug, QueryableByName)]
            struct PermissionRow {
                #[diesel(sql_type = diesel::sql_types::Text)]
                permissions: String,
            }
            let rows = diesel::sql_query(
                "SELECT permissions::text AS permissions FROM roles WHERE id = ANY($1)",
            )
            .bind::<diesel::sql_types::Array<diesel::sql_types::Uuid>, _>(role_ids)
            .load::<PermissionRow>(connection)
            .map_err(UnifiedError::from)?;
            for row in rows {
                if let Ok(value) = serde_json::from_str::<Value>(&row.permissions) {
                    permissions.push(value);
                }
            }
        }
        Ok((Some(profile), permissions))
    })
    .await?;
    for permissions in &permissions {
        for capability in ALL_CAPABILITIES {
            if permission_allowed(permissions, capability) {
                capabilities.insert(capability.to_string());
            }
        }
        // `permission_allowed` grants `"*"` only for a literal
        // `"*": ["*"]` entry, so a wildcard role expands to every
        // capability through `expand_wildcard` below.
        if permission_allowed(permissions, "*") {
            capabilities.insert("*".to_string());
        }
    }
    Ok((expand_wildcard(capabilities), profile))
}

/// A wildcard grant expands to every declared capability so the
/// scope endpoint and the navigation can list them explicitly.
pub(crate) fn expand_wildcard(mut capabilities: HashSet<String>) -> HashSet<String> {
    if capabilities.contains("*") {
        for capability in ALL_CAPABILITIES {
            capabilities.insert(capability.to_string());
        }
    }
    capabilities
}

/// The caller's effective scope: `admin` when they hold
/// `admin:access`, else `reseller` when they hold `reseller:access`,
/// else `user`. Scopes available to the caller, highest first.
pub(crate) fn effective_scope(capabilities: &HashSet<String>) -> (&'static str, Vec<&'static str>) {
    if has_capability(capabilities, CAP_ADMIN_ACCESS) {
        ("admin", vec!["admin", "reseller", "user"])
    } else if has_capability(capabilities, CAP_RESELLER_ACCESS) {
        ("reseller", vec!["reseller", "user"])
    } else {
        ("user", vec!["user"])
    }
}

/// Ask owning modules for one dashboard section through the hook bus.
/// Returns the first matching result, or an `available: false`
/// placeholder when no module answers. Never invents section data.
pub(crate) fn hook_section(ctx: &AppContext, section: &str, scope: &str, user_id: Uuid) -> Value {
    let hook = ctx.hooks.emit(
        EVENT_SECTION_REQUESTED,
        json!({
            "section": section,
            "scope": scope,
            "user_id": user_id.to_string(),
        }),
    );
    for result in &hook.results {
        if result.get("section").and_then(|value| value.as_str()) == Some(section) {
            return result.clone();
        }
    }
    json!({
        "section": section,
        "available": false,
        "data": Value::Null,
        "reason": "No module answered dashboard.section_requested for this section",
    })
}

/// Paginate an already-collected in-memory list with the standard
/// pagination parameters.
pub(crate) fn paginate_vec<T: Clone + Serialize>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

/// Section flags for dashboard templates: one boolean per known
/// section, named `section_<name>` with `-` spelled as `_`, so a
/// template can branch on the current section without an `eq`
/// helper (the engine only registers `route_url` and `asset_url`).
/// Exactly one flag is true: the current section. Unknown sections
/// produce all-false flags.
pub(crate) fn section_flags(section: &str) -> Value {
    let known = [
        "overview",
        "modules",
        "users",
        "orders",
        "school-fees",
        "servers",
        "security",
        "logs",
        "settings",
        "site-builder",
        "email-templates",
        "email-builder",
        "themes",
        "licenses",
        "downloads",
        "tickets",
        "services",
        "apikeys",
        "profile",
        "reseller",
    ];
    let mut flags = serde_json::Map::new();
    for name in known {
        let key = format!("section_{}", name.replace('-', "_"));
        flags.insert(key, Value::Bool(name == section));
    }
    // Reseller subsections get their own flags too.
    flags.insert(
        "section_reseller_clients".to_string(),
        Value::Bool(section == "reseller-clients"),
    );
    flags.insert(
        "section_reseller_payouts".to_string(),
        Value::Bool(section == "reseller-payouts"),
    );
    Value::Object(flags)
}

/// The hook-provided section entry for one dashboard section, taken
/// from an overview payload's `sections` array (`{section,
/// available, data, reason}`). Returns `Value::Null` when no module
/// answered for the section, so the template can render the honest
/// "not connected" state instead of invented rows.
pub(crate) fn hook_section_entry(overview: &Value, section: &str) -> Value {
    if let Some(sections) = overview.get("sections").and_then(|value| value.as_array()) {
        for entry in sections {
            if entry.get("section").and_then(|value| value.as_str()) == Some(section) {
                return entry.clone();
            }
        }
    }
    Value::Null
}

/// Shared page data every rendered dashboard page receives.
pub(crate) fn page_data(
    ctx: &AppContext,
    state: &DashboardsState,
    caller: &CallerContext,
    scope: &str,
    title: &str,
    current_path: &str,
) -> Value {
    let nav = nav_items(ctx, state, scope, &caller.capabilities, Some(current_path));
    let (effective_scope_name, _) = effective_scope(&caller.capabilities);
    let view_as = if caller.identity.impersonating {
        let session = state.view_as.get(&caller.identity.real_user_id);
        json!({
            "active": true,
            "admin_id": caller.identity.real_user_id.to_string(),
            "target_user_id": caller.identity.user_id.to_string(),
            "target_name": caller.profile.as_ref().map(|user| user.name.clone()),
            "expires_at": caller.identity.view_as_expires_at.map(|time| time.to_rfc3339()),
            "reason": session.map(|entry| entry.value().reason.clone()),
        })
    } else {
        json!({ "active": false })
    };
    json!({
        "app_name": "Unified",
        "title": title,
        "theme": "light",
        "scope": scope,
        "is_reseller": scope == "reseller",
        "effective_scope": effective_scope_name,
        "nav": nav,
        "view_as": view_as,
        "user": caller.profile.as_ref().map(|user| json!({
            "id": user.id.to_string(),
            "email": user.email.clone(),
            "name": user.name.clone(),
            "company_id": user.company_id.map(|id| id.to_string()),
        })),
        "user_id": caller.identity.user_id.to_string(),
        "admin_path": state.admin_prefix(),
        "user_path": state.user_prefix(),
    })
}

/// Render a content template inside its layout template and return
/// the HTML response. When the page request carried a Bearer token,
/// the response also sets the session cookie so browser fetches from
/// the page authenticate with `credentials: "same-origin"`.
pub(crate) fn render_page_response(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    content_template: &str,
    layout_template: &str,
    mut data: Value,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Build the template variable context for this page. The page
    // kind follows the content template: admin pages render as
    // "admin", user/reseller dashboard pages as "dashboard", anything
    // else as "custom". The request path and URI feed the `page` /
    // `route` variables. The caller profile already in `data`
    // populates `user`, so `is_logged_in` / `has_user` are true on
    // these authenticated pages; its role carries the effective scope
    // ("admin" / "reseller" / "user") because the page data does not
    // carry the database role, so templates must use `is_admin`
    // (page identity) for admin-page branching, not `user.role`.
    let page_kind = if content_template.starts_with("admin-") {
        "admin"
    } else if content_template.starts_with("user-") {
        "dashboard"
    } else {
        "custom"
    };
    let mut variable_ctx = VariableContext::new().with_page(PageInfo::new(
        page_kind,
        req.path(),
        req.uri().to_string(),
    ));
    if let Some(user) = data.get("user").and_then(|value| value.as_object()) {
        let role = data
            .get("effective_scope")
            .and_then(|value| value.as_str())
            .unwrap_or("user");
        variable_ctx = variable_ctx.with_user(UserInfo::new(
            user.get("id").and_then(|value| value.as_str()).unwrap_or_default(),
            user.get("name")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            user.get("email")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            role,
        ));
    }
    let body = match state
        .engine
        .render_with_context(content_template, &data, &variable_ctx)
    {
        Ok(html) => html,
        Err(error) => return error_response(&error, debug),
    };
    if let Some(object) = data.as_object_mut() {
        object.insert("body".to_string(), Value::String(body));
    }
    let html = match state
        .engine
        .render_with_context(layout_template, &data, &variable_ctx)
    {
        Ok(html) => html,
        Err(error) => return error_response(&error, debug),
    };
    let mut response = HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html);
    let (session_name, tls_configured) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.session_name.clone(),
            !config.app.ssl.listen.is_empty() || !config.app.ssl.cert.is_empty(),
        )
    };
    if let Some(header_value) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(value) = header_value.to_str() {
            if let Some(token) = value
                .trim()
                .strip_prefix("Bearer ")
                .or_else(|| value.trim().strip_prefix("bearer "))
            {
                // The cookie carries a bearer token: `Secure` is set
                // whenever TLS is configured, so the token can never
                // leak over plain HTTP. Plain-HTTP deployments keep a
                // working cookie by omitting it only when no TLS
                // listener or certificate is configured at all.
                let secure = if tls_configured { "; Secure" } else { "" };
                let cookie = format!(
                    "{session_name}={token}; Path=/; HttpOnly; SameSite=Lax{secure}"
                );
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    cookie.parse().unwrap_or_else(|_| {
                        actix_web::http::header::HeaderValue::from_static("")
                    }),
                );
            }
        }
    }
    response
}

// Overview data (database-backed counts and rows; hook-merged sections)

/// Resolve the tenant scope for an admin-overview request, failing
/// closed. `None` means no tenant context was attached: a
/// control-plane request (main host, explicit ControlPlane opt-in,
/// or exempt path). `Some` is the validated tenant context. A
/// request that resolved to a tenant host but whose tenant id is not
/// a valid tenant UUID is rejected - it must never fall through to
/// the `None` branch and be served platform-wide counts as if it
/// were a control-plane request.
pub(crate) fn overview_tenant_scope(req: &HttpRequest) -> UnifiedResult<Option<TenantContext>> {
    match unified_core::middleware::TenantContext::from_request(req) {
        None => Ok(None),
        Some(resolved) => Ok(Some(TenantContext::from_resolved(&resolved)?)),
    }
}

/// Admin overview: system facts from the live registry and database
/// counts. Sections owned by other modules arrive through the hook
/// bus and report `available: false` until their owners answer.
///
/// Tenant scoping (C3): the counts are scoped to the request's
/// tenant whenever one resolved, and are platform-wide ONLY on a
/// proven control-plane request (`tenant` is `None`). A tenant host
/// must never see platform-wide totals - `users`, `licenses`, and
/// `store_orders` are filtered by the tenant's company id, and
/// `roles` (a global vocabulary with no company id) counts the
/// distinct roles assigned to that tenant's users. A resolved tenant
/// with no parseable company id is rejected instead of being served
/// unfiltered counts. A request reaches this function with no tenant
/// context only on the main host (or an explicit ControlPlane
/// opt-in / exempt path): since the C4 fix, unresolved hosts are
/// answered 404 by the tenant-resolution middleware before any
/// handler runs, so "no tenant context" is the same proof the
/// codebase's `require_control_plane` guard relies on - never a
/// fallback to `public` for an unresolved tenant.
pub(crate) async fn admin_overview_data(
    ctx: &AppContext,
    caller: &CallerContext,
    tenant: Option<&TenantContext>,
) -> UnifiedResult<Value> {
    let pool = db_pool(ctx).ok();
    let (users_total, roles_total, licenses_total, orders_total) = match &pool {
        Some(pool) => {
            let pool = pool.clone();
            match tenant {
                Some(tenant) => {
                    let company_id = tenant
                        .company_id
                        .as_deref()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .ok_or_else(|| {
                            UnifiedError::forbidden(
                                "Resolved tenant has no company scope: refusing to serve platform-wide counts",
                            )
                            .with_metadata("kind", json!("missing_tenant_scope"))
                        })?;
                    run_blocking(pool, move |connection| {
                        use unified_core::db::schema::licenses::dsl as licenses_dsl;
                        use unified_core::db::schema::store_orders::dsl as orders_dsl;
                        use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
                        use unified_core::db::schema::users::dsl as users_dsl;
                        let users_total = users_dsl::users
                            .filter(users_dsl::company_id.eq(company_id))
                            .count()
                            .get_result::<i64>(connection)?;
                        let licenses_total = licenses_dsl::licenses
                            .filter(licenses_dsl::company_id.eq(company_id))
                            .count()
                            .get_result::<i64>(connection)?;
                        let orders_total = orders_dsl::store_orders
                            .filter(orders_dsl::company_id.eq(company_id))
                            .count()
                            .get_result::<i64>(connection)?;
                        // Roles carry no company id, so the platform
                        // total is meaningless (and a leak) for a
                        // tenant: count the distinct roles assigned
                        // to this tenant's users instead.
                        let user_ids: Vec<Uuid> = users_dsl::users
                            .filter(users_dsl::company_id.eq(company_id))
                            .select(users_dsl::id)
                            .load::<Uuid>(connection)?;
                        let roles_total = if user_ids.is_empty() {
                            0
                        } else {
                            user_roles_dsl::user_roles
                                .filter(user_roles_dsl::user_id.eq_any(&user_ids))
                                .select(user_roles_dsl::role_id)
                                .distinct()
                                .load::<Uuid>(connection)?
                                .len() as i64
                        };
                        Ok((users_total, roles_total, licenses_total, orders_total))
                    })
                    .await?
                }
                None => {
                    run_blocking(pool, |connection| {
                        use unified_core::db::schema::licenses::dsl as licenses_dsl;
                        use unified_core::db::schema::roles::dsl as roles_dsl;
                        use unified_core::db::schema::store_orders::dsl as orders_dsl;
                        use unified_core::db::schema::users::dsl as users_dsl;
                        let users_total = users_dsl::users.count().get_result::<i64>(connection)?;
                        let roles_total = roles_dsl::roles.count().get_result::<i64>(connection)?;
                        let licenses_total = licenses_dsl::licenses
                            .count()
                            .get_result::<i64>(connection)?;
                        let orders_total = orders_dsl::store_orders
                            .count()
                            .get_result::<i64>(connection)?;
                        Ok((users_total, roles_total, licenses_total, orders_total))
                    })
                    .await?
                }
            }
        }
        None => (0, 0, 0, 0),
    };
    let modules = ctx.modules.summary();
    let dependency_report = ctx.modules.validate_dependencies();
    let scope = "admin";
    Ok(json!({
        "tenant_scope": if tenant.is_some() { "tenant" } else { "control_plane" },
        "system": {
            "uptime_seconds": ctx.uptime_seconds(),
            "modules_total": modules.len(),
            "modules": modules,
            "dependencies_valid": dependency_report.valid,
            "dependency_issues": dependency_report.issues,
            "db_available": pool.is_some(),
            "paused": ctx.is_paused(),
            "maintenance": ctx.is_maintenance(),
        },
        "counts": {
            "users": users_total,
            "roles": roles_total,
            "licenses": licenses_total,
            "orders": orders_total,
        },
        "sections": [
            hook_section(ctx, "servers", scope, caller.identity.user_id),
            hook_section(ctx, "security", scope, caller.identity.user_id),
            hook_section(ctx, "logs", scope, caller.identity.user_id),
        ],
    }))
}

/// User overview: only the caller's own data. Licenses, orders,
/// entitlements, and downloads are read from the database and always
/// filtered by the effective user id; tickets, services, and API keys
/// arrive through the hook bus.
pub(crate) async fn user_overview_data(ctx: &AppContext, caller: &CallerContext) -> UnifiedResult<Value> {
    let pool = db_pool(ctx).ok();
    let user_id = caller.identity.user_id;
    let (licenses_total, active_licenses, orders_total, entitlements_total, downloads_total, recent_licenses, recent_orders) =
        match &pool {
            Some(pool) => {
                let pool = pool.clone();
                run_blocking(pool, move |connection| {
                    use unified_core::db::schema::licenses::dsl as licenses_dsl;
                    use unified_core::db::schema::store_downloads::dsl as downloads_dsl;
                    use unified_core::db::schema::store_entitlements::dsl as entitlements_dsl;
                    use unified_core::db::schema::store_orders::dsl as orders_dsl;
                    let licenses_total = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .count()
                        .get_result::<i64>(connection)?;
                    let active_licenses = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .filter(licenses_dsl::status.eq("active"))
                        .count()
                        .get_result::<i64>(connection)?;
                    let orders_total = orders_dsl::store_orders
                        .filter(orders_dsl::user_id.eq(user_id.to_string()))
                        .count()
                        .get_result::<i64>(connection)?;
                    let entitlements_total = entitlements_dsl::store_entitlements
                        .filter(entitlements_dsl::user_id.eq(user_id.to_string()))
                        .count()
                        .get_result::<i64>(connection)?;
                    let license_ids: Vec<Uuid> = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .select(licenses_dsl::id)
                        .load::<Uuid>(connection)?;
                    let downloads_total = if license_ids.is_empty() {
                        0
                    } else {
                        downloads_dsl::store_downloads
                            .filter(downloads_dsl::license_id.eq_any(&license_ids))
                            .count()
                            .get_result::<i64>(connection)?
                    };
                    let recent_licenses = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .order(licenses_dsl::created_at.desc())
                        .limit(5)
                        .select(License::as_select())
                        .load::<License>(connection)?;
                    let recent_orders = orders_dsl::store_orders
                        .filter(orders_dsl::user_id.eq(user_id.to_string()))
                        .order(orders_dsl::created_at.desc())
                        .limit(5)
                        .select(StoreOrder::as_select())
                        .load::<StoreOrder>(connection)
                        .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>())?;
                    Ok((
                        licenses_total,
                        active_licenses,
                        orders_total,
                        entitlements_total,
                        downloads_total,
                        recent_licenses,
                        recent_orders,
                    ))
                })
                .await?
            }
            None => (0, 0, 0, 0, 0, Vec::new(), Vec::new()),
        };
    let scope = "user";
    Ok(json!({
        "db_available": pool.is_some(),
        "counts": {
            "licenses": licenses_total,
            "active_licenses": active_licenses,
            "orders": orders_total,
            "entitlements": entitlements_total,
            "downloads": downloads_total,
        },
        "recent_licenses": recent_licenses,
        "recent_orders": recent_orders,
        "sections": [
            hook_section(ctx, "tickets", scope, caller.identity.user_id),
            hook_section(ctx, "services", scope, caller.identity.user_id),
            hook_section(ctx, "apikeys", scope, caller.identity.user_id),
        ],
    }))
}

/// Reseller overview: the reseller's linked clients and those
/// clients' orders (scoped to the link store, never to all users).
/// Commissions and payouts are owned by the billing side and arrive
/// through the hook bus.
pub(crate) async fn reseller_overview_data(
    ctx: &AppContext,
    state: &DashboardsState,
    caller: &CallerContext,
) -> UnifiedResult<Value> {
    let reseller_id = caller.identity.user_id;
    let client_ids = linked_client_ids(state, reseller_id);
    let pool = db_pool(ctx).ok();
    let (client_orders_total, client_orders_value) = match &pool {
        Some(pool) if !client_ids.is_empty() => {
            let pool = pool.clone();
            let client_id_strings: Vec<String> =
                client_ids.iter().map(|id| id.to_string()).collect();
            run_blocking(pool, move |connection| {
                use unified_core::db::schema::store_orders::dsl as orders_dsl;
                let total = orders_dsl::store_orders
                    .filter(orders_dsl::user_id.eq_any(&client_id_strings))
                    .count()
                    .get_result::<i64>(connection)?;
                let value: Option<PgNumeric> = orders_dsl::store_orders
                    .filter(orders_dsl::user_id.eq_any(&client_id_strings))
                    .select(diesel::dsl::sum(orders_dsl::total))
                    .first::<Option<PgNumeric>>(connection)?;
                // The sum mixes currencies, so it is converted with the
                // conventional 2-decimal scale (see money::minor_scale).
                let value = value
                    .and_then(|numeric| pg_numeric_to_minor(&numeric, "USD"))
                    .map(|minor| minor as f64 / 100.0)
                    .unwrap_or(0.0);
                Ok((total, value))
            })
            .await?
        }
        _ => (0, 0.0),
    };
    Ok(json!({
        "db_available": pool.is_some(),
        "clients_total": client_ids.len(),
        "client_ids": client_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "counts": {
            "client_orders": client_orders_total,
            "client_orders_value": client_orders_value,
        },
        "sections": [
            hook_section(ctx, "commissions", "reseller", caller.identity.user_id),
        ],
    }))
}

/// Client ids linked to one reseller, sorted for stable output.
pub(crate) fn linked_client_ids(state: &DashboardsState, reseller_id: Uuid) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = state
        .reseller_links
        .iter()
        .filter(|entry| entry.key().0 == reseller_id)
        .map(|entry| entry.key().1)
        .collect();
    ids.sort();
    ids
}

// API handlers

pub(crate) async fn get_scope(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let caller = match require_capability(&req, &ctx, &state, CAP_USER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let (scope, scopes_available) = effective_scope(&caller.capabilities);
    let mut capabilities: Vec<String> = caller.capabilities.iter().cloned().collect();
    capabilities.sort();
    success_response(
        json!({
            "scope": scope,
            "scopes_available": scopes_available,
            "capabilities": capabilities,
            "user_id": caller.identity.user_id.to_string(),
            "real_user_id": caller.identity.real_user_id.to_string(),
            "impersonating": caller.identity.impersonating,
            "impersonated_by": caller.identity.impersonated_by.map(|id| id.to_string()),
            "view_as_expires_at": caller.identity.view_as_expires_at.map(|time| time.to_rfc3339()),
            "admin_path": state.admin_prefix(),
            "user_path": state.user_prefix(),
        }),
        200,
    )
}

pub(crate) fn dashboard_settings_json(ctx: &AppContext, state: &DashboardsState) -> Value {
    let session_name = {
        let config = ctx.config.read().expect("config lock poisoned");
        config.app.session_name.clone()
    };
    json!({
        "view_as_ttl_minutes": state.view_as_ttl_minutes(),
        "view_as_ttl_range": { "min": MIN_VIEW_AS_TTL_MINUTES, "max": MAX_VIEW_AS_TTL_MINUTES },
        "admin_path": state.admin_prefix(),
        "user_path": state.user_prefix(),
        "user_path_config_key": "app.dashboard_path",
        "shared_route_registry": state.route_registry.read().expect("route registry lock poisoned").is_some(),
        "auth": {
            "mechanism": "auth-manager-jwt",
            "token_header": "Authorization: Bearer <token>",
            "session_cookie": session_name,
        },
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::dashboards::CAP_SCHOOL_FEES_VIEW;
    use std::collections::HashSet;

    #[test]
    fn expand_wildcard_star_grants_school_fees_view() {
        let expanded = expand_wildcard(HashSet::from(["*".to_string()]));
        assert!(expanded.contains(&CAP_SCHOOL_FEES_VIEW.to_string()));
        assert!(expanded.contains(&"*".to_string()));
        assert_eq!(expanded.len(), ALL_CAPABILITIES.len() + 1);
    }

    #[test]
    fn expand_wildcard_keeps_plain_capabilities() {
        let expanded = expand_wildcard(HashSet::from(["orders.view".to_string()]));
        assert_eq!(expanded, HashSet::from(["orders.view".to_string()]));
    }

    #[test]
    fn effective_scope_prefers_admin_with_admin_access() {
        let caps = HashSet::from([CAP_ADMIN_ACCESS.to_string()]);
        let (scope, scopes) = effective_scope(&caps);
        assert_eq!(scope, "admin");
        assert_eq!(scopes, vec!["admin", "reseller", "user"]);
    }

    #[test]
    fn effective_scope_prefers_reseller_with_reseller_access() {
        let caps = HashSet::from([CAP_RESELLER_ACCESS.to_string()]);
        let (scope, scopes) = effective_scope(&caps);
        assert_eq!(scope, "reseller");
        assert_eq!(scopes, vec!["reseller", "user"]);
    }

    #[test]
    fn section_flags_mark_exactly_one_section() {
        let flags = section_flags("licenses");
        assert_eq!(flags.get("section_licenses"), Some(&Value::Bool(true)));
        assert_eq!(flags.get("section_overview"), Some(&Value::Bool(false)));
        assert_eq!(flags.get("section_downloads"), Some(&Value::Bool(false)));
        assert_eq!(
            flags.get("section_reseller_clients"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn section_flags_cover_reseller_subsections() {
        let flags = section_flags("reseller-clients");
        assert_eq!(
            flags.get("section_reseller_clients"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            flags.get("section_reseller_payouts"),
            Some(&Value::Bool(false))
        );
        assert_eq!(flags.get("section_reseller"), Some(&Value::Bool(false)));
    }

    #[test]
    fn section_flags_are_all_false_for_unknown_sections() {
        let flags = section_flags("no-such-section");
        assert!(flags
            .as_object()
            .expect("flags are an object")
            .values()
            .all(|value| value == &Value::Bool(false)));
    }

    #[test]
    fn hook_section_entry_finds_the_matching_section() {
        let overview = json!({
            "sections": [
                { "section": "tickets", "available": true, "data": { "open": 2 } },
                { "section": "servers", "available": false, "reason": "No module answered" },
            ]
        });
        let entry = hook_section_entry(&overview, "servers");
        assert_eq!(entry.get("available"), Some(&Value::Bool(false)));
        assert_eq!(hook_section_entry(&overview, "missing"), Value::Null);
        assert_eq!(hook_section_entry(&json!({}), "tickets"), Value::Null);
    }

    #[test]
    fn effective_scope_falls_back_to_user() {
        let (scope, scopes) = effective_scope(&HashSet::new());
        assert_eq!(scope, "user");
        assert_eq!(scopes, vec!["user".to_string()]);
    }
}
