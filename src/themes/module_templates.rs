// Per-module templates: Handlebars templates shipped by a module,
// resolved by the theme engine.
//
// A module registers a template from an in-code source with
// `ThemeEngine::register_module_template(module, name, source)`.
// Templates are addressed as `module:<module>/<name>` - for example
// `module:billing/invoice` - and rendered with
// `ThemeEngine::render_module_template(module, name, data, ...)`,
// which resolves registry variables (`{{site.name}}`, `{{#if
// is_admin}}`, ...) exactly like `render` does.
//
// Resolution order (theme wins, so a theme can restyle module
// output):
//
//   1. The active theme's override, when the theme ships
//      `templates/<module>/<name>.hbs` (discovered at load time by
//      `ThemeEngine::load_theme`; also works for the embedded
//      default theme when it ships such a file).
//   2. The module's registered source.
//   3. Otherwise the render fails with a 404 `UnifiedError`.
//
// Registration compiles the source immediately, so invalid
// Handlebars syntax is rejected at register time and can never
// produce a broken render later. A registered source whose key is
// currently overridden by the active theme stays stored but
// unrendered; switching to a theme without the override makes the
// module default renderable again without re-registering.
//
// Disk support is deliberately limited to theme overrides: there is
// no `register_module_template_dir`. Module defaults come from
// in-code sources only, so a module's templates are always explicit,
// versioned with the module's code, and never depend on a directory
// existing at runtime. A theme override is the only
// filesystem-backed module template.

use unified_core::error::{UnifiedError, UnifiedResult};
use handlebars::Handlebars;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Prefix of a module template's registry key.
pub const MODULE_TEMPLATE_PREFIX: &str = "module:";

/// Registry key for a module template: `module:<module>/<name>`, for
/// example `module:billing/invoice`. This is the name the template is
/// registered under in the live Handlebars registry.
pub fn module_template_key(module: &str, name: &str) -> String {
    format!("{MODULE_TEMPLATE_PREFIX}{module}/{name}")
}

/// Split a registry key back into `(module, name)`, or `None` when the
/// key is not a well-formed `module:<module>/<name>` key.
pub fn parse_module_template_key(key: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix(MODULE_TEMPLATE_PREFIX)?;
    let (module, name) = rest.split_once('/')?;
    if module.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((module.to_string(), name.to_string()))
}

/// Validate a module name or a module template name. Both follow the
/// same rule as plain template names: ASCII letters, digits, `-` and
/// `_` only, non-empty. The charset has no `.` or `/`, so `..`,
/// `../evil`, `a/b`, and `a.b` are all rejected and a name can never
/// escape its directory.
pub fn validate_module_template_name(name: &str) -> UnifiedResult<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid module/template name \"{name}\": only letters, digits, '-' and '_' are allowed"
        )));
    }
    Ok(())
}

/// Compile-check one module template source against a fresh registry.
/// Used at registration time so a syntax error is reported by
/// `register`, never by a later render.
pub fn compile_template_source(key: &str, source: &str) -> UnifiedResult<()> {
    let mut handlebars = Handlebars::new();
    handlebars.register_template_string(key, source).map_err(|error| {
        UnifiedError::bad_request(format!(
            "Module template \"{key}\" failed to compile: {error}"
        ))
        .with_trace(format!("{error:?}"))
    })?;
    Ok(())
}

/// Thread-safe store of module-provided template sources, keyed by
/// registry key (`module:<module>/<name>`). The store holds the
/// module defaults only; theme overrides live in the theme directory
/// and are resolved by the engine at load time.
#[derive(Default)]
pub struct ModuleTemplateRegistry {
    templates: RwLock<HashMap<String, String>>,
}

impl ModuleTemplateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a module template source. The source is compiled
    /// first: invalid Handlebars syntax fails with HTTP 400. A
    /// template that is already registered fails with HTTP 409; use
    /// `replace` to overwrite one deliberately.
    pub fn register(&self, module: &str, name: &str, source: &str) -> UnifiedResult<()> {
        validate_module_template_name(module)?;
        validate_module_template_name(name)?;
        let key = module_template_key(module, name);
        compile_template_source(&key, source)?;
        let mut templates = self
            .templates
            .write()
            .expect("module template registry lock poisoned");
        if templates.contains_key(&key) {
            return Err(UnifiedError::conflict(format!(
                "Module template \"{key}\" is already registered; use replace to overwrite it"
            )));
        }
        templates.insert(key, source.to_string());
        Ok(())
    }

    /// Overwrite a module template, registering it when it does not
    /// exist yet. Returns true when an existing template was replaced.
    /// The new source is compiled first, so a failed replace leaves
    /// the previous source untouched.
    pub fn replace(&self, module: &str, name: &str, source: &str) -> UnifiedResult<bool> {
        validate_module_template_name(module)?;
        validate_module_template_name(name)?;
        let key = module_template_key(module, name);
        compile_template_source(&key, source)?;
        let mut templates = self
            .templates
            .write()
            .expect("module template registry lock poisoned");
        let replaced = templates.contains_key(&key);
        templates.insert(key, source.to_string());
        Ok(replaced)
    }

    /// Remove a module template. Returns true when a template was
    /// removed. A theme override for the same key is unaffected: it
    /// keeps rendering until the theme changes.
    pub fn unregister(&self, module: &str, name: &str) -> bool {
        let key = module_template_key(module, name);
        self.templates
            .write()
            .expect("module template registry lock poisoned")
            .remove(&key)
            .is_some()
    }

    /// True when a module template with this module and name is
    /// registered (regardless of whether the active theme overrides
    /// it).
    pub fn contains(&self, module: &str, name: &str) -> bool {
        let key = module_template_key(module, name);
        self.templates
            .read()
            .expect("module template registry lock poisoned")
            .contains_key(&key)
    }

    /// The registered source for a module template, or `None` when it
    /// is not registered.
    pub fn get_source(&self, module: &str, name: &str) -> Option<String> {
        let key = module_template_key(module, name);
        self.templates
            .read()
            .expect("module template registry lock poisoned")
            .get(&key)
            .cloned()
    }

    /// Number of registered module templates.
    pub fn len(&self) -> usize {
        self.templates
            .read()
            .expect("module template registry lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Registered template keys (`module:<module>/<name>`), sorted.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .templates
            .read()
            .expect("module template registry lock poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Snapshot of `(key, source)` pairs, sorted by key. Used by the
    /// validity report (defensive re-compilation) and by the engine
    /// when it rebuilds the live registry on theme activation.
    pub fn sources(&self) -> Vec<(String, String)> {
        let mut sources: Vec<(String, String)> = self
            .templates
            .read()
            .expect("module template registry lock poisoned")
            .iter()
            .map(|(key, source)| (key.clone(), source.clone()))
            .collect();
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        sources
    }

    /// Register the stored module defaults into a fresh Handlebars
    /// registry, skipping every key in `skip` (theme overrides the
    /// fresh registry already contains - theme wins). Returns the keys
    /// actually registered. Sources were compiled at registration
    /// time, so a failure here is unexpected: the offending template
    /// is logged and skipped instead of breaking theme activation.
    pub fn register_into(
        &self,
        handlebars: &mut Handlebars<'static>,
        skip: &HashSet<String>,
    ) -> Vec<String> {
        let mut registered = Vec::new();
        for (key, source) in self.sources() {
            if skip.contains(&key) {
                continue;
            }
            match handlebars.register_template_string(&key, &source) {
                Ok(()) => registered.push(key),
                Err(error) => {
                    tracing::warn!(
                        "Module template \"{key}\" failed to compile during theme activation and was skipped: {error}"
                    );
                }
            }
        }
        registered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes::engine::ThemeEngine;
    use crate::themes::variables::VariableContext;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("unified-module-templates-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// Write a valid disk theme under `base/<id>/` (all required
    /// templates plus an optional `templates/<module>/<name>.hbs`
    /// override) and return the themes directory.
    fn write_theme_with_override(
        base: &Path,
        id: &str,
        module_override: Option<(&str, &str, &str)>,
    ) -> PathBuf {
        let theme = base.join(id);
        std::fs::create_dir_all(theme.join("templates")).unwrap();
        std::fs::write(
            theme.join("theme.yaml"),
            "name: Shop\nversion: 1.0.0\ndescription: Shop theme\ntemplates:\n  - layout\n  - dashboard\n  - home\n  - error\n",
        )
        .unwrap();
        for name in ["layout", "dashboard", "home", "error"] {
            std::fs::write(theme.join(format!("templates/{name}.hbs")), "<h1>{{title}}</h1>")
                .unwrap();
        }
        std::fs::write(theme.join("screenshot.png"), "fake-png").unwrap();
        if let Some((module, name, source)) = module_override {
            let path = theme.join(format!("templates/{module}/{name}.hbs"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        base.to_path_buf()
    }

    #[test]
    fn key_format_and_parse_round_trip() {
        assert_eq!(
            module_template_key("billing", "invoice"),
            "module:billing/invoice"
        );
        assert_eq!(
            parse_module_template_key("module:billing/invoice"),
            Some(("billing".to_string(), "invoice".to_string()))
        );
        assert_eq!(parse_module_template_key("billing/invoice"), None);
        assert_eq!(parse_module_template_key("module:billing"), None);
        assert_eq!(parse_module_template_key("module:billing/a/b"), None);
        assert_eq!(parse_module_template_key("module:/invoice"), None);
    }

    #[test]
    fn register_and_read_round_trip() {
        let registry = ModuleTemplateRegistry::new();
        assert!(registry.is_empty());
        registry
            .register("billing", "invoice", "<p>{{title}}</p>")
            .unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.contains("billing", "invoice"));
        assert!(!registry.contains("billing", "receipt"));
        assert_eq!(
            registry.get_source("billing", "invoice").as_deref(),
            Some("<p>{{title}}</p>")
        );
        assert_eq!(registry.names(), vec!["module:billing/invoice"]);
    }

    #[test]
    fn duplicate_register_is_rejected() {
        let registry = ModuleTemplateRegistry::new();
        registry.register("billing", "invoice", "one").unwrap();
        let error = registry
            .register("billing", "invoice", "two")
            .unwrap_err();
        assert_eq!(error.code, 409);
        assert_eq!(
            registry.get_source("billing", "invoice").as_deref(),
            Some("one")
        );
    }

    #[test]
    fn replace_overwrites_and_reports_previous_presence() {
        let registry = ModuleTemplateRegistry::new();
        assert!(!registry.replace("billing", "invoice", "one").unwrap());
        assert!(registry.replace("billing", "invoice", "two").unwrap());
        assert_eq!(
            registry.get_source("billing", "invoice").as_deref(),
            Some("two")
        );
    }

    #[test]
    fn invalid_names_are_rejected() {
        let registry = ModuleTemplateRegistry::new();
        for (module, name) in [
            ("../evil", "invoice"),
            ("billing", "../evil"),
            ("a/b", "invoice"),
            ("billing", "a/b"),
            ("billing", "a.b"),
            ("", "invoice"),
            ("billing", ""),
            ("bill ing", "invoice"),
        ] {
            assert!(
                registry.register(module, name, "x").is_err(),
                "({module:?}, {name:?}) should be rejected"
            );
        }
        assert!(registry.register("billing-2", "invoice_v2", "x").is_ok());
    }

    #[test]
    fn invalid_syntax_rejected_at_registration() {
        let registry = ModuleTemplateRegistry::new();
        let error = registry
            .register("billing", "invoice", "{{#if}}")
            .unwrap_err();
        assert_eq!(error.code, 400);
        assert!(!registry.contains("billing", "invoice"));
        // A failed replace leaves the previous source untouched.
        registry.register("billing", "ok", "fine").unwrap();
        assert!(registry.replace("billing", "ok", "{{#each}}").is_err());
        assert_eq!(registry.get_source("billing", "ok").as_deref(), Some("fine"));
    }

    #[test]
    fn unregister_removes_template() {
        let registry = ModuleTemplateRegistry::new();
        registry.register("billing", "invoice", "x").unwrap();
        assert!(registry.unregister("billing", "invoice"));
        assert!(!registry.contains("billing", "invoice"));
        assert!(!registry.unregister("billing", "invoice"));
    }

    #[test]
    fn register_into_skips_theme_overrides() {
        let registry = ModuleTemplateRegistry::new();
        registry.register("billing", "invoice", "default").unwrap();
        registry.register("billing", "receipt", "default").unwrap();
        let mut handlebars = Handlebars::new();
        handlebars
            .register_template_string("module:billing/invoice", "override")
            .unwrap();
        let skip: HashSet<String> = ["module:billing/invoice".to_string()].into_iter().collect();
        let registered = registry.register_into(&mut handlebars, &skip);
        assert_eq!(registered, vec!["module:billing/receipt"]);
        assert_eq!(
            handlebars
                .render("module:billing/invoice", &json!({}))
                .unwrap(),
            "override"
        );
        assert_eq!(
            handlebars
                .render("module:billing/receipt", &json!({}))
                .unwrap(),
            "default"
        );
    }

    #[test]
    fn engine_registers_and_renders_module_template() {
        let base = test_dir("engine-render");
        let engine = ThemeEngine::new(base.clone());
        engine
            .register_module_template("billing", "invoice", "{{site.name}}: {{title}}")
            .unwrap();
        let html = engine
            .render_module_template("billing", "invoice", &json!({ "title": "INV-1" }))
            .unwrap();
        assert_eq!(html, "Unified: INV-1");
        assert!(engine.has_module_template("billing", "invoice"));
        assert_eq!(
            engine.module_template_names(),
            vec!["module:billing/invoice"]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_module_template_renders_logic_variables() {
        let base = test_dir("engine-logic");
        let engine = ThemeEngine::new(base.clone());
        engine
            .register_module_template(
                "billing",
                "invoice",
                "{{#if is_admin}}admin{{else}}public{{/if}}|{{#if is_logged_in}}in{{else}}out{{/if}}",
            )
            .unwrap();
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({}))
                .unwrap(),
            "public|out"
        );
        let ctx = VariableContext::new().with_page_kind("admin");
        assert_eq!(
            engine
                .render_module_template_with_context("billing", "invoice", &json!({}), &ctx)
                .unwrap(),
            "admin|out"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_theme_override_wins_over_module_default() {
        let base = test_dir("engine-override");
        let themes_dir = write_theme_with_override(
            &base,
            "shop",
            Some(("billing", "invoice", "OVERRIDE {{title}}")),
        );
        let engine = ThemeEngine::new(themes_dir);
        // Register the default BEFORE activation: activation must keep
        // the theme override, not the module default.
        engine
            .register_module_template("billing", "invoice", "DEFAULT {{title}}")
            .unwrap();
        engine
            .register_module_template("billing", "receipt", "RECEIPT {{title}}")
            .unwrap();
        engine.set_active_theme("shop").unwrap();
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({ "title": "INV-1" }))
                .unwrap(),
            "OVERRIDE INV-1"
        );
        // A module template without an override still renders its
        // default after the theme switch.
        assert_eq!(
            engine
                .render_module_template("billing", "receipt", &json!({ "title": "R-1" }))
                .unwrap(),
            "RECEIPT R-1"
        );
        // Registering after activation does not displace the override.
        engine
            .replace_module_template("billing", "invoice", "NEWER {{title}}")
            .unwrap();
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({ "title": "INV-2" }))
                .unwrap(),
            "OVERRIDE INV-2"
        );
        // Unregistering the stored default leaves the override in place.
        assert!(engine.unregister_module_template("billing", "invoice"));
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({ "title": "INV-3" }))
                .unwrap(),
            "OVERRIDE INV-3"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_module_template_survives_switch_back_to_default() {
        let base = test_dir("engine-switch-back");
        let themes_dir = write_theme_with_override(
            &base,
            "shop",
            Some(("billing", "invoice", "OVERRIDE")),
        );
        let engine = ThemeEngine::new(themes_dir);
        engine
            .register_module_template("billing", "invoice", "DEFAULT")
            .unwrap();
        engine.set_active_theme("shop").unwrap();
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({}))
                .unwrap(),
            "OVERRIDE"
        );
        engine.set_active_theme("default").unwrap();
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({}))
                .unwrap(),
            "DEFAULT"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_unknown_module_template_is_not_found() {
        let base = test_dir("engine-unknown");
        let engine = ThemeEngine::new(base.clone());
        let error = engine
            .render_module_template("billing", "invoice", &json!({}))
            .unwrap_err();
        assert_eq!(error.code, 404);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_rejects_invalid_module_template_at_registration() {
        let base = test_dir("engine-bad-register");
        let engine = ThemeEngine::new(base.clone());
        assert!(engine
            .register_module_template("billing", "invoice", "{{#if}}")
            .is_err());
        assert!(engine
            .register_module_template("../evil", "invoice", "x")
            .is_err());
        assert!(engine
            .register_module_template("billing", "a/b", "x")
            .is_err());
        assert!(!engine.has_module_template("billing", "invoice"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_unregister_removes_renderable_module_template() {
        let base = test_dir("engine-unregister");
        let engine = ThemeEngine::new(base.clone());
        engine
            .register_module_template("billing", "invoice", "hi")
            .unwrap();
        assert!(engine.unregister_module_template("billing", "invoice"));
        assert!(!engine.has_module_template("billing", "invoice"));
        assert_eq!(
            engine
                .render_module_template("billing", "invoice", &json!({}))
                .unwrap_err()
                .code,
            404
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
