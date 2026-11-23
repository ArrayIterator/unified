// Frontend - Module struct, route table and registration.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, events};
use unified_core::routes::RouteRegistry;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::{Arc, RwLock};

use crate::modules::auth_manager::state::AuthManagerState;
use crate::modules::security::auth::{EXPECTED_JWT_ISSUER, cookie_value, verify_jwt};

use crate::themes::engine::{THEME_ACTIVATED_EVENT, ThemeEngine};

use super::handlers::{
    forgot_password_page, home_page, login_page, register_page, reset_password_page,
};

/// Shared state for the frontend module: its own theme engine (the
/// same embedded default theme the dashboards engine renders) and
/// the shared named-route registry attached at `init`.
pub struct FrontendState {
    pub engine: Arc<ThemeEngine>,
    pub routes: RwLock<Option<Arc<RouteRegistry>>>,
}

impl FrontendState {
    pub fn new() -> Self {
        Self {
            engine: Arc::new(ThemeEngine::new("themes")),
            routes: RwLock::new(None),
        }
    }

    /// Effective path of a named page route, resolved through the
    /// shared registry when attached, otherwise the built-in
    /// default. Used both by `configure_routes` and by the page
    /// handlers that link between auth pages.
    pub fn page_path(&self, name: &str) -> String {
        if let Some(registry) = self.routes.read().expect("route registry lock poisoned").as_ref()
        {
            return registry.route_url(name);
        }
        RouteRegistry::new().route_url(name)
    }
}

impl Default for FrontendState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FrontendModule {
    pub(crate) state: Arc<FrontendState>,
}

impl FrontendModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(FrontendState::new()),
        }
    }

    /// Build the module around a `RouteRegistry` before `init` runs.
    /// `init` always replaces it with the context registry
    /// (`ctx.routes`), so this is only a pre-init hint.
    pub fn with_route_registry(registry: Arc<RouteRegistry>) -> Self {
        let module = Self::new();
        *module
            .state
            .routes
            .write()
            .expect("route registry lock poisoned") = Some(registry);
        module
    }
}

impl Default for FrontendModule {
    fn default() -> Self {
        Self::new()
    }
}

/// `GET /logout`: revoke the session token (when one is
/// presented), expire the session cookie and send the caller back
/// to the sign-in page. Revocation is best-effort and never blocks
/// the logout itself: the cookie is cleared and the redirect is
/// sent even when there is no token, the token is invalid, or no
/// database is attached. When a database is attached the
/// revocation is persisted to `auth_revoked_tokens`, so the global
/// auth middleware denies the token on every route from now on.
/// The Auth Manager's in-memory revocation map is not reachable
/// from here, but both enforcement points (the middleware and the
/// token-verify path) consult the table directly, so the row is
/// enough; the map is only a cache of it.
async fn logout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FrontendState>>,
    req: HttpRequest,
) -> HttpResponse {
    let (secret_key, salt_key, session_name) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.secret_key.clone(),
            config.app.salt_key.clone(),
            config.app.session_name.clone(),
        )
    };
    if let Some(token) = cookie_value(&req, &session_name) {
        if let Ok(claims) = verify_jwt(&token, &secret_key, &salt_key, EXPECTED_JWT_ISSUER) {
            if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
                let expires_at =
                    DateTime::from_timestamp(claims.exp, 0).unwrap_or_else(Utc::now);
                if AuthManagerState::persist_revocation(
                    &pool,
                    &claims.jti,
                    &claims.sub,
                    Utc::now(),
                    expires_at,
                )
                .is_ok()
                {
                    ctx.hooks.emit(
                        "auth.token.revoked",
                        json!({ "user_id": claims.sub, "jti": claims.jti }),
                    );
                    ctx.hooks.emit(
                        events::AUTH_LOGOUT,
                        json!({ "user_id": claims.sub, "provider": "jwt" }),
                    );
                }
            }
        }
    }
    let login_path = state.page_path("login");
    HttpResponse::Found()
        .insert_header((header::LOCATION, login_path))
        .insert_header((
            header::SET_COOKIE,
            format!("{session_name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
        ))
        .finish()
}

impl Module for FrontendModule {
    fn id(&self) -> &'static str {
        "frontend"
    }

    fn name(&self) -> &'static str {
        "Frontend"
    }

    fn description(&self) -> &'static str {
        "Public pages: home, sign-in, registration, password recovery, and sign-out, rendered through the theme engine."
    }

    /// Attach the shared route registry from the context (and the
    /// same instance to the theme engine), so the `route_url` helper
    /// in templates and the page paths registered here resolve config
    /// and settings overrides instead of the built-in defaults.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .routes
            .write()
            .expect("route registry lock poisoned") = Some(Arc::clone(&ctx.routes));
        self.state.engine.set_route_registry(Arc::clone(&ctx.routes));
        Ok(())
    }

    /// Follow `theme.activated` so activating a theme switches the
    /// theme the public pages render with. Without this subscription
    /// the theme module would persist the active id and emit the
    /// event, but this engine would keep rendering the embedded
    /// default theme forever. The engine validates the id itself, so
    /// an invalid activation is ignored and the current theme keeps
    /// rendering.
    fn register_hooks(&self, bus: &HookBus) {
        let engine = Arc::clone(&self.state.engine);
        bus.register(
            THEME_ACTIVATED_EVENT,
            "frontend",
            Arc::new(move |ctx: &mut HookContext| engine.on_theme_activated(ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = self.state.clone();
        // Paths are read once at startup from the shared registry;
        // a live override needs a restart to serve under the new
        // prefix, and the server-level redirect covers the old path
        // until then (same rule as the dashboard prefixes).
        let login = state.page_path("login");
        let register = state.page_path("register");
        let forgot = state.page_path("forgot_password");
        let reset = state.page_path("reset_password");
        let logout_path = state.page_path("logout");
        cfg.app_data(web::Data::new(state.clone()));
        cfg.route("/", web::get().to(home_page));
        cfg.route(&login, web::get().to(login_page));
        cfg.route(&register, web::get().to(register_page));
        cfg.route(&forgot, web::get().to(forgot_password_page));
        cfg.route(&reset, web::get().to(reset_password_page));
        cfg.route(&logout_path, web::get().to(logout));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        // Paths are descriptive here: the live paths come from the
        // shared route registry and may be overridden.
        vec![
            RouteInfo { method: "GET", path: "/", owner: "frontend", description: "Public home page", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/login", owner: "frontend", description: "Sign-in page", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/register", owner: "frontend", description: "Registration page", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/forgot-password", owner: "frontend", description: "Forgot-password page", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/reset-password", owner: "frontend", description: "Reset-password page", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/logout", owner: "frontend", description: "Sign-out endpoint (redirects back to the sign-in page)", auth: unified_core::auth::RouteAuth::Public},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FrontendModule::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_paths_use_built_in_defaults_without_a_registry() {
        let state = FrontendState::new();
        assert_eq!(state.page_path("login"), "/login");
        assert_eq!(state.page_path("register"), "/register");
        assert_eq!(state.page_path("forgot_password"), "/forgot-password");
        assert_eq!(state.page_path("reset_password"), "/reset-password");
        assert_eq!(state.page_path("logout"), "/logout");
    }

    #[test]
    fn page_paths_follow_registry_overrides() {
        let registry = Arc::new(RouteRegistry::new());
        registry
            .set_override(
                "login",
                "/sign-in",
                unified_core::routes::OverrideSource::Settings,
            )
            .expect("override must apply");
        let module = FrontendModule::with_route_registry(Arc::clone(&registry));
        assert_eq!(module.state.page_path("login"), "/sign-in");
        // Untouched routes keep their defaults.
        assert_eq!(module.state.page_path("register"), "/register");
    }

    #[test]
    fn page_paths_follow_config_overrides() {
        use std::collections::HashMap;
        let registry = Arc::new(RouteRegistry::new());
        let overrides = HashMap::from([("register".to_string(), "/join".to_string())]);
        let problems = registry.apply_config_map(&overrides);
        assert!(problems.is_empty(), "problems: {problems:?}");
        let module = FrontendModule::with_route_registry(registry);
        assert_eq!(module.state.page_path("register"), "/join");
    }

    /// Activating a theme through the `theme.activated` hook switches
    /// the theme the public pages render with. An activation for an
    /// unknown theme is ignored and the current theme keeps
    /// rendering.
    #[test]
    fn theme_activated_event_switches_frontend_engine_theme() {
        use serde_json::json;
        use unified_core::hooks::HookBus;
        let module = FrontendModule::new();
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
            "public layout must render as Editorial Dark: {html}"
        );
        bus.emit(
            THEME_ACTIVATED_EVENT,
            json!({ "theme_id": "no-such-theme", "active": true }),
        );
        assert_eq!(module.state.engine.active_theme(), "editorial");
    }
}
