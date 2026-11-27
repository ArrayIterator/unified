// engine - tests.
// Unit tests for the theme engine.
//

use super::*;
use crate::themes::assets::resolve_asset_url;
use crate::themes::module_templates::{
    MODULE_TEMPLATE_PREFIX, ModuleTemplateRegistry, module_template_key,
    validate_module_template_name,
};
use crate::themes::validity::{
    ThemeIssue, ThemeValidationReport, report_for_sources, validate_theme_directory,
};
use crate::themes::variables::{VariableContext, VariableRegistry, VariableResolver};
use handlebars::{
    Context, Handlebars, Helper, Output, RenderContext, RenderError, RenderErrorReason,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::routes::RouteRegistry;

use crate::themes::variables::{SiteInfo, TenantInfo, UserInfo};
use serde_json::json;

/// Build an engine whose only registered template is `probe` with
/// the given source, so render tests exercise the real Handlebars
/// registry without depending on the embedded default theme.
fn probe_engine(source: &str) -> ThemeEngine {
    let engine = ThemeEngine::new("themes");
    let rendered = vec![("probe".to_string(), source.to_string())];
    let fresh = engine
        .build_registry(&rendered, "test probe theme")
        .expect("probe template must compile");
    engine.swap_registry(fresh, &rendered);
    engine
}

#[test]
fn render_resolves_builtin_site_variable() {
    let engine = probe_engine("Welcome to {{site.name}} ({{site.url}})");
    let html = engine.render("probe", &json!({})).unwrap();
    assert_eq!(html, "Welcome to Unified ()");
}

#[test]
fn render_with_context_populates_user_tenant_and_settings() {
    let engine = probe_engine(
        "{{user.name}}|{{user.role}}|{{tenant.name}}|{{settings.theme}}",
    );
    let ctx = VariableContext::new()
        .with_site(SiteInfo::new("Acme", "https://acme.example"))
        .with_user(UserInfo::new("u-1", "Ada", "ada@example.com", "admin"))
        .with_tenant(TenantInfo::new("t-1", "Acme Ltd"))
        .with_setting("theme", json!("dark"));
    let html = engine.render_with_context("probe", &json!({}), &ctx).unwrap();
    assert_eq!(html, "Ada|admin|Acme Ltd|dark");
}

#[test]
fn anonymous_render_reports_unauthenticated_user() {
    let engine = probe_engine(
        "{{#if user.authenticated}}signed in{{else}}guest{{/if}}",
    );
    let html = engine.render("probe", &json!({})).unwrap();
    assert_eq!(html, "guest");
}

#[test]
fn per_call_data_wins_over_registered_variables() {
    let engine = probe_engine("{{site.name}}");
    let html = engine
        .render("probe", &json!({ "site": { "name": "Override" } }))
        .unwrap();
    assert_eq!(html, "Override");
}

#[test]
fn custom_registered_variable_renders() {
    let engine = probe_engine("{{greeting}}, {{user.name}}!");
    engine
        .register_variable(
            "greeting",
            "A greeting.",
            std::sync::Arc::new(|_: &VariableContext| Ok(json!("Hello"))),
        )
        .unwrap();
    let ctx = VariableContext::new()
        .with_user(UserInfo::new("u-2", "Grace", "grace@example.com", "member"));
    let html = engine.render_with_context("probe", &json!({}), &ctx).unwrap();
    assert_eq!(html, "Hello, Grace!");
}

#[test]
fn failing_custom_variable_does_not_break_render() {
    let engine = probe_engine("ok:{{good}} bad:{{#if bad}}present{{else}}absent{{/if}}");
    engine
        .register_variable(
            "good",
            "Works.",
            std::sync::Arc::new(|_: &VariableContext| Ok(json!("yes"))),
        )
        .unwrap();
    engine
        .register_variable(
            "bad",
            "Fails.",
            std::sync::Arc::new(|_: &VariableContext| {
                Err(UnifiedError::internal("boom"))
            }),
        )
        .unwrap();
    let html = engine.render("probe", &json!({})).unwrap();
    assert_eq!(html, "ok:yes bad:absent");
}

#[test]
fn logic_variables_branch_on_page_kind() {
    let engine = probe_engine(
        "{{#if is_admin}}admin{{else}}{{#if is_home}}home{{else}}{{#if is_dashboard}}dashboard{{else}}other{{/if}}{{/if}}{{/if}}",
    );
    let admin_ctx = VariableContext::new().with_page_kind("admin");
    assert_eq!(
        engine
            .render_with_context("probe", &json!({}), &admin_ctx)
            .unwrap(),
        "admin"
    );
    let home_ctx = VariableContext::new().with_page_kind("home");
    assert_eq!(
        engine
            .render_with_context("probe", &json!({}), &home_ctx)
            .unwrap(),
        "home"
    );
    let dashboard_ctx = VariableContext::new().with_page_kind("dashboard");
    assert_eq!(
        engine
            .render_with_context("probe", &json!({}), &dashboard_ctx)
            .unwrap(),
        "dashboard"
    );
    // Plain render: default context, kind "custom".
    assert_eq!(engine.render("probe", &json!({})).unwrap(), "other");
}

#[test]
fn logic_variables_branch_on_login_state() {
    let engine = probe_engine(
        "{{#if is_logged_in}}hi {{user.name}}{{else}}guest{{/if}}|{{#if has_user}}user{{else}}nouser{{/if}}",
    );
    assert_eq!(engine.render("probe", &json!({})).unwrap(), "guest|nouser");
    let ctx = VariableContext::new()
        .with_user(UserInfo::new("u-1", "Ada", "ada@example.com", "member"));
    assert_eq!(
        engine.render_with_context("probe", &json!({}), &ctx).unwrap(),
        "hi Ada|user"
    );
}

#[test]
fn has_data_reflects_per_call_data_object() {
    let engine = probe_engine("{{#if has_data}}data{{else}}empty{{/if}}");
    assert_eq!(engine.render("probe", &json!({})).unwrap(), "empty");
    assert_eq!(
        engine.render("probe", &json!({ "title": "Hello" })).unwrap(),
        "data"
    );
    // A per-call has_data value is always overridden by the
    // engine-computed one.
    assert_eq!(
        engine.render("probe", &json!({ "has_data": false })).unwrap(),
        "data"
    );
    assert_eq!(
        engine
            .render("probe", &json!({ "has_data": true }))
            .unwrap(),
        "data"
    );
}

#[test]
fn page_and_route_objects_render() {
    let engine = probe_engine("{{page.kind}}/{{page.type}}/{{route.name}}/{{page.path}}");
    let ctx = VariableContext::new().with_page(
        crate::themes::variables::PageInfo::new("admin", "/admin/users", "/admin/users")
            .with_route_name("admin_users"),
    );
    let html = engine.render_with_context("probe", &json!({}), &ctx).unwrap();
    assert_eq!(html, "admin/admin/admin_users//admin/users");
}

#[test]
fn editorial_disk_theme_loads_and_validates() {
    // The second shipped theme: a dark editorial variant of the
    // default theme. The test runs from the workspace root, so
    // the `themes` directory on disk is the real one.
    let engine = ThemeEngine::new("themes");
    let report = engine.validate_theme("editorial");
    assert!(
        report.valid,
        "editorial theme failed validation: {}",
        report.error_summary()
    );
    let meta = engine
        .load_theme("editorial")
        .expect("editorial theme must load");
    assert_eq!(meta.id, "editorial");
    assert_eq!(meta.name, "Editorial Dark");
    // Every template the default theme renders must exist in
    // the variant, or activating it would 404 dashboard pages.
    for name in [
        "layout",
        "dashboard",
        "home",
        "error",
        "admin-layout",
        "admin-dashboard",
        "user-layout",
        "user-dashboard",
        "login",
        "register",
        "forgot-password",
        "reset-password",
        "admin-site-builder",
        "admin-email-templates",
        "admin-settings",
        "admin-themes",
    ] {
        assert!(
            engine.template_names().contains(&name.to_string()),
            "editorial theme is missing template {name}"
        );
    }
    // The variant renders dark by default and ships its own
    // stylesheet under the theme's assets.
    let html = engine
        .render("layout", &json!({ "app_name": "Unified", "body": "x" }))
        .expect("editorial layout must render");
    assert!(html.contains("theme-dark"), "layout is not dark: {html}");
    assert!(html.contains("theme:editorial.css") || html.contains("editorial.css"));
}

/// A `theme.activated` event switches the engine's active theme
/// and the pages it renders: the default layout renders light,
/// Editorial Dark renders dark.
#[test]
fn theme_activated_event_switches_active_theme_and_rendering() {
    use unified_core::hooks::HookContext;
    let engine = ThemeEngine::new("themes");
    assert_eq!(engine.active_theme(), "default");
    let before = engine
        .render("layout", &json!({ "app_name": "Unified", "body": "x" }))
        .expect("default layout must render");
    assert!(
        !before.contains("theme-dark editorial"),
        "default layout must not render as Editorial Dark: {before}"
    );
    let mut ctx = HookContext::new(
        THEME_ACTIVATED_EVENT,
        json!({ "theme_id": "editorial", "active": true }),
    );
    engine.on_theme_activated(&mut ctx);
    assert_eq!(engine.active_theme(), "editorial");
    let after = engine
        .render("layout", &json!({ "app_name": "Unified", "body": "x" }))
        .expect("editorial layout must render");
    assert!(
        after.contains("theme-dark editorial"),
        "activated layout must render as Editorial Dark: {after}"
    );
}

/// A `theme.activated` event for an unknown or invalid theme
/// leaves the active theme (and its rendering) untouched.
#[test]
fn theme_activated_event_ignores_unknown_and_invalid_themes() {
    use unified_core::hooks::HookContext;
    let engine = ThemeEngine::new("themes");
    for theme_id in ["no-such-theme", "../escape", ""] {
        let mut ctx = HookContext::new(
            THEME_ACTIVATED_EVENT,
            json!({ "theme_id": theme_id, "active": true }),
        );
        engine.on_theme_activated(&mut ctx);
        assert_eq!(
            engine.active_theme(),
            "default",
            "theme_id {theme_id:?} must not switch the active theme"
        );
    }
    // An event without a theme_id is ignored the same way.
    let mut ctx =
        HookContext::new(THEME_ACTIVATED_EVENT, json!({ "active": true }));
    engine.on_theme_activated(&mut ctx);
    assert_eq!(engine.active_theme(), "default");
}
