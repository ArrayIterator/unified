// Dashboards - Module struct, route table and registration.

// Dashboards module — separate admin, user (client), and reseller dashboards.
//
// Three scopes, one design system:
//
//   - Admin dashboard under `/admin` (prefix fixed; see the config note
//     below): administrators and staff. Full management scope: system
//     overview, module management, users and roles, orders and invoices,
//     servers and clusters, security, logs, settings.
//   - User (client) dashboard under `/{app.dashboard_path}` (default
//     `/dashboard`, read from the application configuration in `init`;
//     exactly one user-dashboard prefix, never `/app`): the caller's own
//     licenses, downloads, orders, tickets, services, profile, and API
//     keys. Every handler filters by the effective user id.
//   - Reseller: the user dashboard plus a subset for the reseller's own
//     clients (their orders and services) and a commission and payout
//     view. Never the full admin scope.
//
// Enforcement is server-side, in two layers (defence in depth):
//
//   Layer 1 — middleware (`DashboardsAuthMiddleware`, registered through
//   `Module::register_middleware`, priority 600 so it runs after the
//   security-hardening middleware). It authenticates every request under
//   a dashboard prefix and rejects unauthenticated requests with 401 in
//   the standard error envelope before any handler runs. It also expires
//   view-as sessions. Non-dashboard paths pass through untouched.
//
//   Layer 2 — per-handler capability checks (`require_capability`).
//   Handlers never trust the middleware alone: they re-authenticate
//   from the request when no identity is present in the request
//   extensions, resolve the caller's capabilities, and return 403 in
//   the standard error envelope when the capability is missing. A user
//   guessing an admin URL gets 403; a reseller hitting an admin-only
//   route gets 403.
//
// Authentication
// ---------------
// Bearer tokens issued by the Auth Manager (`POST /api/v1/auth/token`):
// HS256 JWTs whose `sub` claim is the user's UUID. Verification here
// mirrors the Auth Manager exactly (base64url without padding,
// HMAC-SHA256 over `header.payload`, signing key
// `"{secret_key}:{salt_key}"` from the application configuration when
// both are set). The token may arrive as an `Authorization: Bearer`
// header or as the session cookie (`app.session_name`, default
// `vault`); page handlers set that cookie from a presented Bearer
// token so browser fetches from the rendered pages authenticate with
// `credentials: "same-origin"`.
//
// Known gaps (see the coordinator report for the integrator snippets):
//   - `src/auth/session.rs` tokens carry a numeric (`u64`) user id and
//     are never issued by any module, while users are UUID-keyed, so
//     they cannot identify a caller and are not accepted here.
//   - The Auth Manager keeps its JWT revocation list in its own
//     private state, so a revoked token cannot be detected here until
//     revocation is shared (hook or shared store).
//   - When both `app.secret_key` and `app.salt_key` are empty the Auth
//     Manager falls back to an in-memory secret this module cannot
//     read; dashboard authentication then fails closed with 401.
//
// Capabilities
// ------------
// Capabilities are `resource:action` strings evaluated with the Roles
// module's `permission_allowed` (see `crate::modules::roles`). The
// `admin` role carries `"*": ["*"]` and therefore holds every
// capability below. The `customer` role already carries
// `orders:read` and `licenses:read`. Capabilities come from two
// sources, unioned: the JWT `scopes` claim, and the caller's role
// permissions read from the database (`users` -> `user_roles` ->
// `roles.permissions`). When no database pool is configured, the
// `dashboard.capabilities_requested` hook is emitted and any handler
// may contribute a `capabilities` array for the user id.
//
// Declared by this module (no dotted `admin.access` spelling: the
// roles system splits on the first `:` into resource and action):
//   `admin:access`, `users:manage`, `modules:manage`,
//   `security:view`, `orders:manage`, `servers:manage`, `logs:view`,
//   `settings:manage`, `admin:view_as_user`, `user:access`,
//   `reseller:access`, `reseller:clients.view`,
//   `reseller:clients.manage`, `reseller:payouts.view`,
//   `licenses:read`, `orders:read`, `tickets:read`, `services:read`,
//   `apikeys:read`.
// `user:access` is the baseline for any authenticated caller: holding
// a valid token is what makes someone a user of their own dashboard.
//
// Reseller clients
// ----------------
// No reseller-to-client link exists in the schema yet. The working
// rule is an explicit link store owned by this module (in-memory,
// like the Roles and Audit stores): a reseller links a client id, and
// every reseller handler filters by the linked client ids. A migration
// snippet for a persistent `reseller_clients` table is given in the
// coordinator report.
//
// Overview data
// -------------
// Counts and rows that live in Diesel tables are read directly
// (`users`, `roles`, `licenses`, `store_orders`,
// `store_entitlements`, `store_downloads`). Sections owned by other
// modules (tickets, services, API keys, security, servers, logs,
// commissions) are assembled through the `dashboard.section_requested`
// hook: this module emits `{ "section", "scope", "user_id" }` and
// merges the first matching result `{ "section", "available", "data" }`.
// A section no module answers is returned as `available: false` with
// the reason, never as invented data. Owning modules can answer the
// hook to light a section up without any change here.
//
// Templates
// ---------
// Template names must be flat (`validate_template_name` in the theme
// engine rejects `/`, so `templates/default/admin/layout.hbs` would
// break registration of the whole embedded default theme). The pages
// are therefore `admin-layout` / `admin-dashboard` / `user-layout` /
// `user-dashboard`, with `admin-nav` / `user-nav` partials under
// `templates/default/partials/`. They reuse the shared design tokens
// and classes from `assets/css/dashboard.css` plus the layout-only
// `assets/css/admin.css` / `assets/css/client.css`.
//
// Customizable dashboard paths
// ----------------------------
// The dashboard prefixes come from the single shared route registry
// (`AppContext.routes`), attached by `init` together with the theme
// engine's copy of the same handle: `user_dashboard` and
// `admin_dashboard` resolve through the registry's config < settings
// override layers, and a `route.changed` hook handler keeps the
// stored prefixes current. Without a shared registry the prefixes
// fall back to `app.dashboard_path` (user dashboard) and the `admin`
// default.
// Actix scopes are registered once at startup, so a live path change
// needs a restart to serve under the new prefix; the Routes module's
// server-level redirect covers the old path until then.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::routes::{RouteRegistry, ROUTE_CHANGED_EVENT};
use actix_web::web;
use std::sync::Arc;

use crate::themes::engine::THEME_ACTIVATED_EVENT;

use super::{DashboardsAuthMiddleware, DashboardsState, admin_page, admin_page_root,
    get_admin_modules, get_admin_overview, get_admin_users, get_dashboard_settings, get_nav,
    get_reseller_clients, get_reseller_overview, get_scope, get_user_downloads, get_user_licenses,
    get_user_orders, get_user_overview, get_view_as, link_reseller_client, start_view_as,
    stop_view_as, unlink_reseller_client, update_dashboard_settings, user_page, user_page_root};

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct DashboardsModule {
    pub(crate) state: Arc<DashboardsState>,
}

impl DashboardsModule {
    pub fn new() -> Self {
        Self { state: Arc::new(DashboardsState::new()) }
    }

    /// Build the module around a `RouteRegistry` before `init` runs
    /// (for example the shared instance from `AppContext`). `init`
    /// always replaces it with the context registry (`ctx.routes`),
    /// so this is only a pre-init hint: the module, its prefixes, and
    /// the theme engine all resolve through the one shared registry
    /// after `init`, never through a private copy. The dashboards
    /// module resolves the `user_dashboard` and `admin_dashboard`
    /// paths from that registry in `init` instead of from the
    /// configuration alone, so config and admin-settings overrides
    /// for the dashboard paths apply here too. Without a shared
    /// registry the module falls back to `app.dashboard_path` (user)
    /// and `admin`.
    pub fn with_route_registry(registry: Arc<RouteRegistry>) -> Self {
        let module = Self::new();
        *module
            .state
            .route_registry
            .write()
            .expect("route registry lock poisoned") = Some(registry);
        module
    }
}

impl Default for DashboardsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DashboardsModule {
    fn id(&self) -> &'static str {
        "dashboards"
    }

    fn name(&self) -> &'static str {
        "Dashboards"
    }

    fn description(&self) -> &'static str {
        "Separate admin, user, and reseller dashboards with server-side capability enforcement, role-aware navigation, and audited admin view-as-user."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "users", "audit"]
    }

    /// Attach the shared route registry from the context (and the
    /// same instance to the theme engine), then resolve the dashboard
    /// prefixes from it and stash the handles the middleware needs.
    /// The registry's effective paths for `user_dashboard` and
    /// `admin_dashboard` win over the configuration
    /// (`app.dashboard_path` for the user dashboard, default
    /// `dashboard`), because the registry already layers the config
    /// value under admin-settings overrides. `init` runs before the
    /// HTTP server starts, so the prefixes are fixed before
    /// `configure_routes` registers the page scopes.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Unify on the single shared registry from the context: the
        // same instance the Routes module serves, the server redirect
        // handler resolves through, and `AppContext::new` seeded from
        // the `routes:` config section. Attaching it here (instead of
        // keeping whatever `with_route_registry` may have set) keeps
        // the prefixes, the settings endpoint, and the theme engine
        // on one registry, so a `routes:` override reaches all of them.
        *self.state.route_registry.write().expect("route registry lock poisoned") =
            Some(Arc::clone(&ctx.routes));
        // Attach the same shared registry to the theme engine so its
        // `route_url` helper resolves config and settings overrides
        // instead of the built-in defaults.
        self.state.engine.set_route_registry(Arc::clone(&ctx.routes));
        let registry = self
            .state
            .route_registry
            .read()
            .expect("route registry lock poisoned")
            .clone();
        if let Some(registry) = registry {
            if let Some(path) = registry.resolve("user_dashboard") {
                if let Some(segment) = DashboardsState::first_segment(&path) {
                    *self.state.user_prefix.write().expect("user prefix lock poisoned") =
                        segment;
                }
            }
            if let Some(path) = registry.resolve("admin_dashboard") {
                if let Some(segment) = DashboardsState::first_segment(&path) {
                    *self.state.admin_prefix.write().expect("admin prefix lock poisoned") =
                        segment;
                }
            }
        } else {
            let config = ctx.config.read().expect("config lock poisoned");
            let user_path = config.app.dashboard_path.trim();
            if !user_path.is_empty()
                && user_path
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                *self.state.user_prefix.write().expect("user prefix lock poisoned") =
                    user_path.to_string();
            }
        }
        let _ = self.state.hooks.set(ctx.hooks.clone());
        let _ = self.state.config.set(ctx.config.clone());
        let _ = self.state.db.set(ctx.db.clone());
        Ok(())
    }

    /// Follow `route.changed` for the two dashboard routes so the
    /// navigation, the scope endpoint, and the settings endpoint keep
    /// reporting the current paths after an admin override. Actix
    /// scopes are registered once at startup, so serving under a
    /// newly overridden prefix still needs a restart; the Routes
    /// module's server-level redirect covers the old path until then.
    ///
    /// Also follow `theme.activated`: activating a theme must switch
    /// the theme this module's pages render with. Without this
    /// subscription the theme module would persist the active id and
    /// emit the event, but this engine would keep rendering the
    /// embedded default theme forever. The engine validates the id
    /// itself, so an invalid activation is ignored and the current
    /// theme keeps rendering.
    fn register_hooks(&self, bus: &HookBus) {
        let engine = Arc::clone(&self.state.engine);
        bus.register(
            THEME_ACTIVATED_EVENT,
            "dashboards",
            Arc::new(move |ctx: &mut HookContext| engine.on_theme_activated(ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ROUTE_CHANGED_EVENT,
            "dashboards",
            Arc::new(move |ctx: &mut unified_core::hooks::HookContext| {
                let route = ctx
                    .get("route")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let new_path = ctx
                    .get("new_path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let Some(segment) = DashboardsState::first_segment(new_path) else {
                    return;
                };
                match route {
                    "user_dashboard" => {
                        *state.user_prefix.write().expect("user prefix lock poisoned") =
                            segment;
                    }
                    "admin_dashboard" => {
                        *state.admin_prefix.write().expect("admin prefix lock poisoned") =
                            segment;
                    }
                    _ => {}
                }
            }),
        );
    }

    fn register_middleware(&self, registry: &mut unified_core::middleware::MiddlewareManager) {
        registry.register(DashboardsAuthMiddleware::new(self.state.clone()));
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = self.state.clone();
        let admin_scope = format!("/{}", state.admin_prefix());
        let user_scope = format!("/{}", state.user_prefix());
        cfg.service(
            web::scope("/api/v1/dashboards")
                .app_data(web::Data::new(state.clone()))
                .route("/scope", web::get().to(get_scope))
                .route("/nav", web::get().to(get_nav))
                .route("/admin/overview", web::get().to(get_admin_overview))
                .route("/admin/modules", web::get().to(get_admin_modules))
                .route("/admin/users", web::get().to(get_admin_users))
                .route("/admin/settings", web::get().to(get_dashboard_settings))
                .route("/admin/settings", web::put().to(update_dashboard_settings))
                .route("/admin/view-as", web::get().to(get_view_as))
                .route("/admin/view-as", web::post().to(start_view_as))
                .route("/admin/view-as", web::delete().to(stop_view_as))
                .route("/user/overview", web::get().to(get_user_overview))
                .route("/user/licenses", web::get().to(get_user_licenses))
                .route("/user/orders", web::get().to(get_user_orders))
                .route("/user/downloads", web::get().to(get_user_downloads))
                .route("/reseller/overview", web::get().to(get_reseller_overview))
                .route("/reseller/clients", web::get().to(get_reseller_clients))
                .route("/reseller/clients", web::post().to(link_reseller_client))
                .route("/reseller/clients/{client_id}", web::delete().to(unlink_reseller_client)),
        );
        cfg.service(
            web::scope(&admin_scope)
                .app_data(web::Data::new(state.clone()))
                .route("", web::get().to(admin_page_root))
                .route("/{tail:.*}", web::get().to(admin_page)),
        );
        cfg.service(
            web::scope(&user_scope)
                .app_data(web::Data::new(state.clone()))
                .route("", web::get().to(user_page_root))
                .route("/{path:.*}", web::get().to(user_page)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/dashboards/scope", owner: "dashboards", description: "Caller's effective scope, capabilities, and impersonation state", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/nav", owner: "dashboards", description: "Role-aware navigation items for one scope", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/overview", owner: "dashboards", description: "Admin system overview", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/modules", owner: "dashboards", description: "Admin module list with dependency validation", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/users", owner: "dashboards", description: "Admin paginated user list with roles", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/settings", owner: "dashboards", description: "Dashboard runtime settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/dashboards/admin/settings", owner: "dashboards", description: "Update dashboard runtime settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/view-as", owner: "dashboards", description: "Current admin view-as session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/dashboards/admin/view-as", owner: "dashboards", description: "Start an admin view-as-user session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/dashboards/admin/view-as", owner: "dashboards", description: "Stop the active view-as session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/overview", owner: "dashboards", description: "Caller's own overview", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/licenses", owner: "dashboards", description: "Caller's own licenses, paginated", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/orders", owner: "dashboards", description: "Caller's own orders, paginated", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/downloads", owner: "dashboards", description: "Caller's own downloads, paginated", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/reseller/overview", owner: "dashboards", description: "Reseller overview for linked clients", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/dashboards/reseller/clients", owner: "dashboards", description: "Reseller linked clients, paginated", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/dashboards/reseller/clients", owner: "dashboards", description: "Link a client to the reseller", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/dashboards/reseller/clients/{client_id}", owner: "dashboards", description: "Unlink a client from the reseller", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/admin", owner: "dashboards", description: "Admin dashboard page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/admin/{section}", owner: "dashboards", description: "Admin dashboard section page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/{dashboard_path}", owner: "dashboards", description: "User dashboard page (prefix from app.dashboard_path)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/{dashboard_path}/{path}", owner: "dashboards", description: "User dashboard section page", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DashboardsModule::new()));
}


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::dashboards::{CAP_ORDERS_MANAGE, CAP_SCHOOL_FEES_VIEW, nav_items};
    use std::collections::HashSet;
    use std::sync::Arc;

    fn context() -> Arc<AppContext> {
        Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        ))
    }

    fn state() -> DashboardsState {
        DashboardsState::new()
    }

    fn caps(items: &[&str]) -> HashSet<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn nav_items_admin_orders_visible_with_capability() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "admin", &caps(&[CAP_ORDERS_MANAGE]), None);
        assert!(items.iter().any(|item| item.id == "admin.orders"));
    }

    #[test]
    fn nav_items_admin_orders_hidden_without_capability() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "admin", &HashSet::new(), None);
        assert!(!items.iter().any(|item| item.id == "admin.orders"));
    }

    #[test]
    fn nav_items_admin_school_fees_visible_with_capability() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "admin", &caps(&[CAP_SCHOOL_FEES_VIEW]), None);
        let entry = items
            .iter()
            .find(|item| item.id == "admin.school-fees")
            .expect("admin school-fees nav entry missing");
        assert_eq!(entry.href, "/admin/school-fees");
    }

    #[test]
    fn nav_items_admin_school_fees_hidden_without_capability() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "admin", &HashSet::new(), None);
        assert!(!items.iter().any(|item| item.id == "admin.school-fees"));
    }

    #[test]
    fn nav_items_user_school_fees_visible_with_capability() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "user", &caps(&[CAP_SCHOOL_FEES_VIEW]), None);
        let entry = items
            .iter()
            .find(|item| item.id == "user.school-fees")
            .expect("user school-fees nav entry missing");
        assert_eq!(entry.href, "/dashboard/school-fees");
    }

    #[test]
    fn nav_items_user_school_fees_hidden_without_capability() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "user", &HashSet::new(), None);
        assert!(!items.iter().any(|item| item.id == "user.school-fees"));
    }

    #[test]
    fn nav_items_wildcard_shows_all_admin_entries() {
        let ctx = context();
        let items = nav_items(&ctx, &state(), "admin", &caps(&["*"]), None);
        assert_eq!(items.len(), 9);
        assert!(items.iter().any(|item| item.id == "admin.school-fees"));
    }

    #[test]
    fn nav_items_marks_current_entry_active() {
        let ctx = context();
        let items = nav_items(
            &ctx,
            &state(),
            "admin",
            &caps(&[CAP_ORDERS_MANAGE]),
            Some("/admin/orders"),
        );
        let current = items.iter().find(|item| item.id == "admin.orders").unwrap();
        assert!(current.active);
    }

    /// Activating a theme through the `theme.activated` hook switches
    /// the theme the dashboard pages render with: the default layout
    /// renders light, Editorial Dark renders dark. An activation for
    /// an unknown theme is ignored and the current theme keeps
    /// rendering.
    #[test]
    fn theme_activated_event_switches_dashboard_engine_theme() {
        use serde_json::json;
        let module = DashboardsModule::new();
        let bus = HookBus::new();
        module.register_hooks(&bus);
        assert_eq!(module.state.engine.active_theme(), "default");
        bus.emit(
            THEME_ACTIVATED_EVENT,
            json!({ "theme_id": "editorial", "active": true }),
        );
        assert_eq!(module.state.engine.active_theme(), "editorial");
        let html = module
            .state
            .engine
            .render("layout", &json!({ "app_name": "Unified", "body": "x" }))
            .expect("editorial layout must render");
        assert!(
            html.contains("theme-dark editorial"),
            "dashboard layout must render as Editorial Dark: {html}"
        );
        bus.emit(
            THEME_ACTIVATED_EVENT,
            json!({ "theme_id": "no-such-theme", "active": true }),
        );
        assert_eq!(module.state.engine.active_theme(), "editorial");
    }
}
