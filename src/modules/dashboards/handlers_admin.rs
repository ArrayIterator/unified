// Dashboards - Admin dashboard pages and API handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::User;
use actix_web::{web, HttpRequest, HttpResponse};
use diesel::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::{CAP_ADMIN_ACCESS, CAP_EMAIL_TEMPLATES_MANAGE, CAP_LOGS_VIEW, CAP_MODULES_MANAGE,
    CAP_ORDERS_MANAGE, CAP_SCHOOL_FEES_VIEW, CAP_RESELLER_ACCESS, CAP_SECURITY_VIEW,
    CAP_SERVERS_MANAGE, CAP_SETTINGS_MANAGE, CAP_SITE_BUILDER_MANAGE, CAP_THEME_MANAGE,
    CAP_USERS_MANAGE, CAP_USER_ACCESS, DashboardsState, NavQuery, UserResponseView,
    admin_overview_data, db_pool, effective_scope, has_capability, hook_section_entry, nav_items,
    overview_tenant_scope, page_data, render_page_response, require_capability, run_blocking,
    section_flags};

pub(crate) async fn get_nav(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<NavQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_USER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let requested = query.scope.clone().unwrap_or_else(|| {
        let (scope, _) = effective_scope(&caller.capabilities);
        scope.to_string()
    });
    if !["admin", "user", "reseller"].contains(&requested.as_str()) {
        return error_response(
            &UnifiedError::bad_request(format!(
                "Unknown nav scope \"{requested}\": expected admin, user, or reseller"
            )),
            debug,
        );
    }
    // The requested scope must be one the caller may use; asking for
    // the admin nav without admin access is a 403, not an empty list.
    let required = match requested.as_str() {
        "admin" => CAP_ADMIN_ACCESS,
        "reseller" => CAP_RESELLER_ACCESS,
        _ => CAP_USER_ACCESS,
    };
    if required != CAP_USER_ACCESS && !has_capability(&caller.capabilities, required) {
        return error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{required}\" is required for the {requested} navigation"
            ))
            .with_metadata("capability", json!(required))
            .with_metadata("scope", json!(requested)),
            debug,
        );
    }
    let items = nav_items(
        &ctx,
        &state,
        &requested,
        &caller.capabilities,
        query.current.as_deref(),
    );
    success_response(json!({ "scope": requested, "items": items }), 200)
}

pub(crate) async fn get_admin_overview(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_ADMIN_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let tenant = match overview_tenant_scope(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    match admin_overview_data(&ctx, &caller, tenant.as_ref()).await {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn get_admin_modules(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let caller = match require_capability(&req, &ctx, &state, CAP_MODULES_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    let report = ctx.modules.validate_dependencies();
    success_response(
        json!({
            "modules": ctx.modules.summary(),
            "dependencies_valid": report.valid,
            "dependency_issues": report.issues,
        }),
        200,
    )
}

pub(crate) async fn get_admin_users(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_USERS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["name", "email", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let search = pagination.search.clone().map(|value| value.to_lowercase());
    // Tenant scoping (C3): the client-supplied `company_id` filter
    // must never widen the caller's scope. On a tenant host the
    // filter is forced to the resolved tenant's company — asking for
    // another company is a 403 — so a tenant admin can only ever
    // list their own users. On a proven control-plane request (no
    // tenant context) the filter stays caller-supplied, as before.
    let tenant = match overview_tenant_scope(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let mut company_filter = pagination
        .filters
        .get("company_id")
        .and_then(|value| Uuid::parse_str(value).ok());
    if let Some(tenant) = &tenant {
        let tenant_company = match tenant
            .company_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            Some(company_id) => company_id,
            None => {
                return error_response(
                    &UnifiedError::forbidden(
                        "Resolved tenant has no company scope: refusing to list users unfiltered",
                    )
                    .with_metadata("kind", json!("missing_tenant_scope")),
                    debug,
                );
            }
        };
        if let Some(requested) = company_filter {
            if requested != tenant_company {
                return error_response(
                    &UnifiedError::forbidden(
                        "Requested company does not match the resolved tenant: users are never listed across tenants",
                    )
                    .with_metadata("kind", json!("tenant_scope_mismatch")),
                    debug,
                );
            }
        }
        company_filter = Some(tenant_company);
    }
    let result = run_blocking(pool, move |connection| {
        use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
        use unified_core::db::schema::users::dsl as users_dsl;
        let mut users: Vec<User> = users_dsl::users
            .select(User::as_select())
            .load::<User>(connection)
            .map_err(UnifiedError::from)?;
        users.retain(|user| {
            if let Some(needle) = &search {
                let haystack = format!("{} {}", user.name, user.email).to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            if let Some(company_id) = company_filter {
                if user.company_id != Some(company_id) {
                    return false;
                }
            }
            true
        });
        users.sort_by(|a, b| match sort.as_str() {
            "name" => a.name.cmp(&b.name),
            "email" => a.email.cmp(&b.email),
            _ => a.created_at.cmp(&b.created_at),
        });
        if descending {
            users.reverse();
        }
        let total = users.len() as u64;
        let start = ((pagination.page - 1) * pagination.per_page) as usize;
        let end = (start + pagination.per_page as usize).min(users.len());
        let page_users: Vec<User> = if start < users.len() {
            users[start..end].to_vec()
        } else {
            Vec::new()
        };
        let ids: Vec<Uuid> = page_users.iter().map(|user| user.id).collect();
        let assignments: Vec<(Uuid, Uuid)> = if ids.is_empty() {
            Vec::new()
        } else {
            user_roles_dsl::user_roles
                .filter(user_roles_dsl::user_id.eq_any(&ids))
                .select((user_roles_dsl::user_id, user_roles_dsl::role_id))
                .load::<(Uuid, Uuid)>(connection)
                .map_err(UnifiedError::from)?
        };
        let mut by_user: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for (user_id, role_id) in assignments {
            by_user.entry(user_id).or_default().push(role_id);
        }
        let items: Vec<Value> = page_users
            .iter()
            .map(|user| {
                let mut view = serde_json::to_value(UserResponseView::from(user)).unwrap_or(json!({}));
                if let Some(object) = view.as_object_mut() {
                    object.insert(
                        "roles".to_string(),
                        json!(by_user.get(&user.id).cloned().unwrap_or_default()),
                    );
                }
                view
            })
            .collect();
        Ok((items, total))
    })
    .await;
    match result {
        Ok((items, total)) => success_response(PageResult::new(items, total, &pagination).to_json(), 200),
        Err(error) => error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Page handlers (HTML; auth failures stay in the standard JSON envelope)
// ---------------------------------------------------------------------------

pub(crate) async fn admin_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let tail = path.into_inner();
    admin_page_inner(req, ctx, state, &tail).await
}

pub(crate) async fn admin_page_inner(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    tail: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let first = tail.split('/').next().unwrap_or("");
    let section = if first.is_empty() { "overview" } else { first };
    let allowed = ["overview", "modules", "users", "orders", "school-fees", "servers", "security", "logs", "settings", "site-builder", "email-templates", "themes"];
    if !allowed.contains(&section) {
        return error_response(
            &UnifiedError::not_found(format!("Admin section \"{section}\" does not exist")),
            debug,
        );
    }
    let capability = match section {
        "modules" => CAP_MODULES_MANAGE,
        "users" => CAP_USERS_MANAGE,
        "orders" => CAP_ORDERS_MANAGE,
        "school-fees" => CAP_SCHOOL_FEES_VIEW,
        "servers" => CAP_SERVERS_MANAGE,
        "security" => CAP_SECURITY_VIEW,
        "logs" => CAP_LOGS_VIEW,
        "settings" => CAP_SETTINGS_MANAGE,
        // Site builder, email templates, and themes keep their own
        // modules' permission names (colon form, as everywhere in
        // this module); the admin role holds "*" and passes them all.
        "site-builder" => CAP_SITE_BUILDER_MANAGE,
        "email-templates" => CAP_EMAIL_TEMPLATES_MANAGE,
        "themes" => CAP_THEME_MANAGE,
        _ => CAP_ADMIN_ACCESS,
    };
    let caller = match require_capability(&req, &ctx, &state, capability).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let current_path = format!("/{}/{}", state.admin_prefix(), tail.trim_matches('/'));
    let current_path = current_path.trim_end_matches('/').to_string();
    let current_path = if current_path.is_empty() {
        format!("/{}", state.admin_prefix())
    } else {
        current_path
    };
    let mut data = page_data(&ctx, &state, &caller, "admin", &format!("Admin — {section}"), &current_path);
    let tenant = match overview_tenant_scope(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let overview = match admin_overview_data(&ctx, &caller, tenant.as_ref()).await {
        Ok(data) => data,
        Err(error) => return error_response(&error, debug),
    };
    if let Some(object) = data.as_object_mut() {
        object.insert("section".to_string(), Value::String(section.to_string()));
        object.insert("overview".to_string(), overview.clone());
        // Section flags (`section_users`, `section_settings`, ...) let
        // the template show the current section's panel instead of
        // repeating the overview on every section page.
        if let Value::Object(flags) = section_flags(section) {
            for (key, value) in flags {
                object.insert(key, value);
            }
        }
        // Hook-provided data for this section, when its owning module
        // answers; `null` renders the honest "not connected" state.
        object.insert(
            "section_data".to_string(),
            hook_section_entry(&overview, section),
        );
    }
    // The builder, email templates, settings, and themes sections
    // have their own content templates; the remaining sections share
    // the overview template with section panels.
    let content_template = match section {
        "site-builder" => "admin-site-builder",
        "email-templates" => "admin-email-templates",
        "settings" => "admin-settings",
        "themes" => "admin-themes",
        _ => "admin-dashboard",
    };
    render_page_response(&req, &ctx, &state, content_template, "admin-layout", data)
}

pub(crate) async fn admin_page_root(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    admin_page_inner(req, ctx, state, "").await
}
