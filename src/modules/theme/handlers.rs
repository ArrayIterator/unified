// Theme HTTP handlers and hook listeners.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json as api_json;
use crate::themes::engine::THEME_ACTIVATED_EVENT;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

use super::*;

/// Capability required to activate a theme, checked through the
/// `role.check` hook (fails closed: no answer means not allowed).
/// The read endpoints stay public; only activation mutates state.
pub(crate) const PERM_THEME_MANAGE: &str = "theme:manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
pub(crate) struct Caller {
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // The actor header is required (anonymous callers are refused)
    // but the capability check runs on the role only.
    let verified = unified_core::caller::require_caller(req)?;
    let role = verified.role().to_string();
    Ok(Caller { role })
}

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
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

/// Caller identity plus one capability check in one call: anonymous
/// callers are refused (401), and so are roles without the
/// permission (403).
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    if check_capability(ctx, &found.role, permission) {
        Ok(found)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{permission}\"",
            found.role
        )))
    }
}

/// Run theme discovery off the async executor thread: discovery does
/// synchronous `read_dir` / `read_to_string` / `canonicalize` filesystem
/// I/O, which must never block an executor worker. Discovery is run
/// fresh on every call (no cache), so listings always reflect the
/// themes directory and behaviour is identical to before. On the
/// (practically unreachable) blocking-task failure, fall back to the
/// embedded default theme alone rather than failing the request.
async fn discover_themes_blocking(state: &ThemeState) -> Vec<ThemeInfo> {
    let owned = state.clone();
    let active_id = owned.active_id();
    match tokio::task::spawn_blocking(move || discover_themes(&owned)).await {
        Ok(themes) => themes,
        Err(_) => vec![embedded_default_info(&active_id)],
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_themes(state: web::Data<ThemeState>) -> HttpResponse {
    let themes = discover_themes_blocking(&state).await;
    api_json::success_response(json!({ "themes": themes }), 200)
}

pub(crate) async fn get_theme(state: web::Data<ThemeState>, id: web::Path<String>) -> HttpResponse {
    let id = id.into_inner();
    if let Err(error) = validate_theme_id(&id) {
        return api_json::error_response(&error, false);
    }
    let themes = discover_themes_blocking(&state).await;
    match themes.into_iter().find(|theme| theme.id == id) {
        Some(theme) => api_json::success_response(theme, 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("theme \"{id}\" not found")),
            false,
        ),
    }
}

pub(crate) async fn activate_theme(
    state: web::Data<ThemeState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    // Authorization comes first: an unauthenticated or unauthorized
    // caller learns nothing about whether the theme id is valid.
    if let Err(error) = authorize(&ctx, &req, PERM_THEME_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    if let Err(error) = validate_theme_id(&id) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if id != "default" {
        // Theme validation reads theme.yaml from disk; run it off the
        // async executor thread like discovery.
        let dir = state.themes_dir.join(&id);
        let validation = match tokio::task::spawn_blocking(move || validate_theme(&dir)).await
        {
            Ok(result) => result,
            Err(join_error) => {
                return api_json::error_response(
                    &UnifiedError::internal(format!(
                        "Theme validation task failed: {join_error}"
                    )),
                    ctx.is_debug(),
                );
            }
        };
        match validation {
            Ok(_) => {}
            Err(problems) => {
                return api_json::error_response(
                    &UnifiedError::unprocessable(format!(
                        "theme \"{id}\" failed validation"
                    ))
                    .with_metadata("problems", json!(problems)),
                    ctx.is_debug(),
                );
            }
        }
    }
    // Capture the previous theme before mutating: if persisting the new
    // active theme fails, roll back in memory so memory and disk never
    // diverge (mirrors `on_theme_activate_requested` below).
    let previous = state.active_id();
    state.set_active(&id);
    if let Err(error) = state.persist_active().await {
        state.set_active(&previous);
        return api_json::error_response(&error, ctx.is_debug());
    }
    ctx.hooks.emit(
        THEME_ACTIVATED_EVENT,
        json!({ "theme_id": id, "active": true }),
    );
    let themes = discover_themes_blocking(&state).await;
    match themes.into_iter().find(|theme| theme.id == id) {
        Some(theme) => api_json::success_response(theme, 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("theme \"{id}\" not found")),
            ctx.is_debug(),
        ),
    }
}

pub(crate) async fn get_active_theme(state: web::Data<ThemeState>) -> HttpResponse {
    let active_id = state.active_id();
    let themes = discover_themes_blocking(&state).await;
    match themes.into_iter().find(|theme| theme.id == active_id) {
        Some(theme) => api_json::success_response(theme, 200),
        None => api_json::success_response(
            json!({
                "id": active_id,
                "missing": true,
                "fallback": embedded_default_info("default"),
            }),
            200,
        ),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers (ERP interconnection: Site Builder and SaaS ask the
// Theme module through these events instead of calling it directly).
// ---------------------------------------------------------------------------

/// Answer `theme.exists` (from the Site Builder) with `{"exists": bool}`
/// from the module's theme discovery. A discovered theme counts as
/// existing even when invalid; `valid` reports the validation outcome.
pub(crate) fn on_theme_exists(state: &ThemeState, ctx: &mut HookContext) {
    let theme_id = ctx
        .get("theme_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let found = discover_themes(state)
        .into_iter()
        .find(|theme| theme.id == theme_id);
    match found {
        Some(theme) => {
            ctx.add_result(json!({
                "exists": true,
                "valid": theme.valid,
                "theme_id": theme_id,
            }));
        }
        None => {
            ctx.add_result(json!({
                "exists": false,
                "valid": false,
                "theme_id": theme_id,
            }));
        }
    }
}

/// Answer `theme.render_block` (from the Site Builder). This module has
/// no template rendering engine of its own, so no rendering applies and
/// the fragment is returned unchanged — both as the `html` result the
/// task contract asks for and via `context.set("html", ...)`, which is
/// what the Site Builder's `render_block` actually reads.
pub(crate) fn on_theme_render_block(ctx: &mut HookContext) {
    let html = ctx
        .get("html")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("block")
                .and_then(|block| block.get("html"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    match html {
        Some(html) => {
            ctx.set("html", Value::String(html.clone()));
            ctx.add_result(json!({ "html": html }));
        }
        None => {
            ctx.add_result(json!({
                "error": "theme.render_block requires 'html' (or 'block.html')",
            }));
        }
    }
}

/// Answer `theme.activate_requested` (from the SaaS module) by running
/// the module's own activation logic — id validation, theme validation,
/// `set_active`, and a synchronous persist of the state file — and
/// reporting `{"activated": bool}`.
pub(crate) fn on_theme_activate_requested(state: &ThemeState, ctx: &mut HookContext) {
    let theme_id = ctx
        .get("theme_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let outcome: UnifiedResult<()> = (|| {
        let id = validate_theme_id(&theme_id)?;
        if id != "default" {
            let dir = state.themes_dir.join(&id);
            validate_theme(&dir).map_err(|problems| {
                UnifiedError::unprocessable(format!("theme \"{id}\" failed validation"))
                    .with_metadata("problems", json!(problems))
            })?;
        }
        let previous = state.active_id();
        state.set_active(&id);
        if let Err(error) = state.persist_active_sync() {
            state.set_active(&previous);
            return Err(error);
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            ctx.add_result(json!({ "activated": true, "theme_id": theme_id }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "activated": false,
                "theme_id": theme_id,
                "error": error.message,
            }));
        }
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use std::path::PathBuf;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", Value::Bool(true));
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

    /// A themes directory containing one valid theme `alpha`, plus the
    /// base directory that owns it (for the state file and cleanup).
    /// Each test uses its own directory so parallel tests never share
    /// filesystem state.
    fn setup_themes(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "unified-theme-handler-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let themes_dir = base.join("themes");
        let alpha = themes_dir.join("alpha");
        std::fs::create_dir_all(&alpha).expect("create alpha theme dir");
        std::fs::write(
            alpha.join("theme.yaml"),
            "name: Alpha\nversion: 1.2.3\ndescription: A test theme.\n",
        )
        .expect("write theme.yaml");
        // The engine requires layout, dashboard, home and error.
        let templates_dir = alpha.join("templates");
        std::fs::create_dir_all(&templates_dir).expect("create templates dir");
        for name in ["layout", "dashboard", "home", "error"] {
            std::fs::write(templates_dir.join(format!("{name}.hbs")), "<html></html>")
                .expect("write template");
        }
        (base, themes_dir)
    }

    fn state_for(base: &PathBuf, themes_dir: &PathBuf) -> web::Data<ThemeState> {
        web::Data::new(ThemeState::new(
            themes_dir.clone(),
            base.join("state").join("theme.json"),
        ))
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert_eq!(caller(&req).unwrap_err().code, 401);
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

    #[tokio::test]
    async fn activate_theme_rejects_anonymous_callers() {
        let (base, themes_dir) = setup_themes("anonymous");
        let response = activate_theme(
            state_for(&base, &themes_dir),
            data(allowing_context()),
            TestRequest::default().to_http_request(),
            web::Path::from("alpha".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn activate_theme_rejects_callers_without_theme_manage() {
        // Headers present, but no `role.check` handler answers: the
        // guard fails closed.
        let (base, themes_dir) = setup_themes("unauthorized");
        let response = activate_theme(
            state_for(&base, &themes_dir),
            data(test_context()),
            authed_request(),
            web::Path::from("alpha".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn activate_theme_accepts_authorized_caller() {
        let (base, themes_dir) = setup_themes("authorized");
        let state = state_for(&base, &themes_dir);
        let response = activate_theme(
            state.clone(),
            data(allowing_context()),
            authed_request(),
            web::Path::from("alpha".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.active_id(), "alpha");
        let persisted =
            std::fs::read_to_string(base.join("state").join("theme.json"))
                .expect("state file is written");
        assert!(persisted.contains("\"alpha\""), "{persisted}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn activate_theme_rejects_unknown_theme() {
        // A well-formed id with no theme directory fails validation
        // (422) and the active theme is left untouched.
        let (base, themes_dir) = setup_themes("unknown");
        let state = state_for(&base, &themes_dir);
        let response = activate_theme(
            state.clone(),
            data(allowing_context()),
            authed_request(),
            web::Path::from("no-such-theme".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(state.active_id(), "default");
        assert!(!base.join("state").join("theme.json").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn activate_theme_rejects_malformed_theme_id() {
        // An id that cannot name a theme at all is refused (400)
        // before any validation or state change.
        let (base, themes_dir) = setup_themes("malformed");
        let state = state_for(&base, &themes_dir);
        let response = activate_theme(
            state.clone(),
            data(allowing_context()),
            authed_request(),
            web::Path::from("bad id!".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.active_id(), "default");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn activate_theme_rolls_back_when_persist_fails() {
        let (base, themes_dir) = setup_themes("rollback");
        // The state file's parent is a regular file, so creating the
        // parent directory (and therefore persisting) always fails.
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("write blocker file");
        let state = web::Data::new(ThemeState::new(
            themes_dir,
            blocker.join("theme.json"),
        ));
        assert_eq!(state.active_id(), "default");
        let response = activate_theme(
            state.clone(),
            data(allowing_context()),
            authed_request(),
            web::Path::from("alpha".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Memory must still hold the previous theme: the failed persist
        // must not leave memory and disk diverged.
        assert_eq!(state.active_id(), "default");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn list_themes_discovers_themes_off_the_executor() {
        let (base, themes_dir) = setup_themes("list");
        let response = list_themes(state_for(&base, &themes_dir)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn get_active_theme_returns_the_active_theme() {
        let (base, themes_dir) = setup_themes("active");
        let state = state_for(&base, &themes_dir);
        state.set_active("alpha");
        let response = get_active_theme(state).await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(&base);
    }
}
