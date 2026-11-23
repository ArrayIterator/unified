// Frontend - Public page handlers (home and auth pages).
//
// Rendering follows the dashboards `render_page_response` pattern:
// render the content template first, then wrap it in the `layout`
// template. These pages are public, so there is no capability check
// here; the auth pages rely on the JSON APIs for authentication and
// never accept credentials themselves.

use unified_core::context::AppContext;
use unified_core::json::error_response;
use crate::themes::variables::{PageInfo, VariableContext};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::module::FrontendState;

/// Render a content template inside the public `layout` template and
/// return the HTML response. `page_kind` drives the logic variables
/// (`is_home` for the home page, `custom` for the auth pages).
fn render_page(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &FrontendState,
    content_template: &str,
    page_kind: &str,
    mut data: Value,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let variable_ctx = VariableContext::new().with_page(PageInfo::new(
        page_kind,
        req.path(),
        req.uri().to_string(),
    ));
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
    match state
        .engine
        .render_with_context("layout", &data, &variable_ctx)
    {
        Ok(html) => HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body(html),
        Err(error) => error_response(&error, debug),
    }
}

/// Base data every public page receives: the app name, the resolved
/// auth and dashboard URLs, and the session cookie name the sign-in
/// script stores the issued token under.
fn base_data(ctx: &AppContext, state: &FrontendState, title: &str) -> Value {
    let session_name = {
        let config = ctx.config.read().expect("config lock poisoned");
        config.app.session_name.clone()
    };
    json!({
        "app_name": "Unified",
        "title": title,
        "theme": "light",
        "home_url": "/",
        "login_url": state.page_path("login"),
        "register_url": state.page_path("register"),
        "forgot_url": state.page_path("forgot_password"),
        "reset_url": state.page_path("reset_password"),
        "logout_url": state.page_path("logout"),
        "dashboard_url": state.engine.route_url("user_dashboard"),
        "admin_url": state.engine.route_url("admin_dashboard"),
        "session_name": session_name,
    })
}

/// `GET /`: the public home page. Counts only report what this
/// process knows without a database: the registered module total.
/// The template guards the database-backed stats, so they stay
/// hidden instead of rendering as zeroes.
pub(crate) async fn home_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FrontendState>>,
) -> HttpResponse {
    let mut data = base_data(&ctx, &state, "Home");
    let modules_total = ctx.modules.summary().len();
    if let Some(object) = data.as_object_mut() {
        object.insert(
            "tagline".to_string(),
            Value::String("One control plane for everything you run.".to_string()),
        );
        object.insert(
            "description".to_string(),
            Value::String(
                "Licensing, downloads, storefront, software and asset management, and day-to-day operations in one place.".to_string(),
            ),
        );
        object.insert(
            "counts".to_string(),
            json!({ "modules_total": modules_total }),
        );
    }
    render_page(&req, &ctx, &state, "home", "home", data)
}

/// `GET /login`: the sign-in page. The form posts to
/// `POST /api/v1/auth-manager/jwt/issue` from `assets/js/auth.js`;
/// on success the issued token is stored in the session cookie and
/// the caller is sent to the user dashboard.
pub(crate) async fn login_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FrontendState>>,
) -> HttpResponse {
    let data = base_data(&ctx, &state, "Sign in");
    render_page(&req, &ctx, &state, "login", "custom", data)
}

/// `GET /register`: the registration page. The form posts to
/// `POST /api/v1/users` from `assets/js/auth.js`. That endpoint
/// requires the `users:manage` permission, so a visitor without it
/// gets a plain explanation that an administrator creates accounts;
/// an administrator can use the same page to create one.
pub(crate) async fn register_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FrontendState>>,
) -> HttpResponse {
    let data = base_data(&ctx, &state, "Create account");
    render_page(&req, &ctx, &state, "register", "custom", data)
}

/// `GET /forgot-password`: the form posts to
/// `POST /api/v1/auth-manager/password/forgot` (see
/// `auth_manager::reset`), which emails a single-use reset link
/// valid for 60 minutes. The response never reveals whether the
/// account exists.
pub(crate) async fn forgot_password_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FrontendState>>,
) -> HttpResponse {
    let data = base_data(&ctx, &state, "Forgot password");
    render_page(&req, &ctx, &state, "forgot-password", "custom", data)
}

/// `GET /reset-password`: the form redeems the `?token=` carried
/// in the emailed reset link through
/// `POST /api/v1/auth-manager/password/reset` (see
/// `auth_manager::reset`). A token is single-use and expires after
/// 60 minutes.
pub(crate) async fn reset_password_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FrontendState>>,
) -> HttpResponse {
    let data = base_data(&ctx, &state, "Reset password");
    render_page(&req, &ctx, &state, "reset-password", "custom", data)
}

/// Re-exported for the module tests: the content template each public
/// page renders, so a test can assert the wiring without starting a
/// server.
pub(crate) fn content_template_for(route_name: &str) -> Option<&'static str> {
    match route_name {
        "login" => Some("login"),
        "register" => Some("register"),
        "forgot_password" => Some("forgot-password"),
        "reset_password" => Some("reset-password"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_auth_route_has_a_content_template() {
        for name in ["login", "register", "forgot_password", "reset_password"] {
            assert!(
                content_template_for(name).is_some(),
                "no content template for route {name}"
            );
        }
        assert_eq!(content_template_for("logout"), None);
        assert_eq!(content_template_for("home"), None);
    }

    #[test]
    fn auth_templates_are_registered_in_the_default_theme() {
        // The engine under test is the real embedded default theme:
        // a missing template is a wiring bug, not a render detail.
        let engine = crate::themes::engine::ThemeEngine::new("themes");
        for name in ["login", "register", "forgot-password", "reset-password"] {
            assert!(
                engine.template_names().contains(&name.to_string()),
                "template {name} is not registered"
            );
        }
    }
}
