// engine - module_templates.
// Per-module template registration and rendering.
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

impl ThemeEngine {
    /// Register a module template from an in-code source, addressable
    /// as `module:<module>/<name>` (for example
    /// `module:billing/invoice`). The source is compiled immediately:
    /// invalid Handlebars syntax fails with HTTP 400, and an already
    /// registered template fails with HTTP 409 (use
    /// `replace_module_template` to overwrite one deliberately).
    /// Module and template names allow only letters, digits, `-` and
    /// `_`, so they can never traverse directories. When the active
    /// theme overrides this key with its own
    /// `templates/<module>/<name>.hbs`, the source is stored but the
    /// override keeps rendering until the theme changes.
    pub fn register_module_template(
        &self,
        module: &str,
        name: &str,
        source: &str,
    ) -> UnifiedResult<()> {
        self.module_templates.register(module, name, source)?;
        let key = module_template_key(module, name);
        if self.is_theme_module_override(&key) {
            return Ok(());
        }
        let mut handlebars = self
            .handlebars
            .write()
            .expect("handlebars lock poisoned");
        handlebars
            .register_template_string(&key, source)
            .map_err(|error| {
                UnifiedError::internal(format!(
                    "Module template \"{key}\" failed to compile: {error}"
                ))
                .with_trace(format!("{error:?}"))
            })?;
        self.known_templates
            .write()
            .expect("known templates lock poisoned")
            .insert(key);
        Ok(())
    }

    /// Overwrite a module template, registering it when it does not
    /// exist yet. Returns true when an existing template was replaced.
    /// The new source is compiled before anything is replaced, so a
    /// failed replace leaves the previous source untouched. A theme
    /// override for the same key keeps rendering, as with `register`.
    pub fn replace_module_template(
        &self,
        module: &str,
        name: &str,
        source: &str,
    ) -> UnifiedResult<bool> {
        let replaced = self.module_templates.replace(module, name, source)?;
        let key = module_template_key(module, name);
        if !self.is_theme_module_override(&key) {
            let mut handlebars = self
                .handlebars
                .write()
                .expect("handlebars lock poisoned");
            handlebars
                .register_template_string(&key, source)
                .map_err(|error| {
                    UnifiedError::internal(format!(
                        "Module template \"{key}\" failed to compile: {error}"
                    ))
                    .with_trace(format!("{error:?}"))
                })?;
            self.known_templates
                .write()
                .expect("known templates lock poisoned")
                .insert(key);
        }
        Ok(replaced)
    }

    /// Remove a registered module template. Returns true when a stored
    /// template was removed. A theme override for the same key is
    /// unaffected and keeps rendering until the theme changes.
    pub fn unregister_module_template(&self, module: &str, name: &str) -> bool {
        let removed = self.module_templates.unregister(module, name);
        if removed {
            let key = module_template_key(module, name);
            if !self.is_theme_module_override(&key) {
                self.handlebars
                    .write()
                    .expect("handlebars lock poisoned")
                    .unregister_template(&key);
                self.known_templates
                    .write()
                    .expect("known templates lock poisoned")
                    .remove(&key);
            }
        }
        removed
    }

    /// True when a module template is currently renderable: either the
    /// module's registered source or a theme override for the key.
    pub fn has_module_template(&self, module: &str, name: &str) -> bool {
        let key = module_template_key(module, name);
        self.known_templates
            .read()
            .expect("known templates lock poisoned")
            .contains(&key)
    }

    /// Registry keys (`module:<module>/<name>`) of the module
    /// templates currently renderable, sorted. This includes keys the
    /// active theme overrides.
    pub fn module_template_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .known_templates
            .read()
            .expect("known templates lock poisoned")
            .iter()
            .filter(|name| name.starts_with(MODULE_TEMPLATE_PREFIX))
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// The module's registered source for a template, or `None` when
    /// it is not registered. This is the module default even when the
    /// active theme overrides the key.
    pub fn module_template_source(&self, module: &str, name: &str) -> Option<String> {
        self.module_templates.get_source(module, name)
    }

    /// Render a module template (`module:<module>/<name>`) with the
    /// given JSON data. Resolution order: the active theme's
    /// `templates/<module>/<name>.hbs` override first, then the
    /// module's registered source; an unknown module template fails
    /// with a 404 `UnifiedError`. Registered variables are resolved
    /// against an empty `VariableContext`, exactly like `render`; use
    /// `render_module_template_with_context` to supply one. Plain
    /// `render` does not accept `module:` names.
    pub fn render_module_template(
        &self,
        module: &str,
        name: &str,
        data: &Value,
    ) -> UnifiedResult<String> {
        self.render_module_template_with_context(module, name, data, &VariableContext::new())
    }

    /// Render a module template with the given JSON data and a
    /// render-time variable context. See `render_module_template`.
    pub fn render_module_template_with_context(
        &self,
        module: &str,
        name: &str,
        data: &Value,
        ctx: &VariableContext,
    ) -> UnifiedResult<String> {
        validate_module_template_name(module)?;
        validate_module_template_name(name)?;
        let key = module_template_key(module, name);
        let known = self
            .known_templates
            .read()
            .expect("known templates lock poisoned")
            .contains(&key);
        if !known {
            return Err(UnifiedError::not_found(format!(
                "Module template \"{key}\" is not registered and no active theme overrides it"
            ))
            .with_metadata("template", Value::String(key)));
        }
        let merged = self.merged_template_context(data, ctx);
        let handlebars = self
            .handlebars
            .read()
            .expect("handlebars lock poisoned");
        handlebars.render(&key, &merged).map_err(|error| {
            UnifiedError::internal(format!("Failed to render module template \"{key}\": {error}"))
                .with_metadata("template", Value::String(key))
                .with_trace(format!("{error:?}"))
        })
    }

    /// True when the active theme provides its own template for this
    /// module template key.
    fn is_theme_module_override(&self, key: &str) -> bool {
        self.theme_module_templates
            .read()
            .expect("theme module templates lock poisoned")
            .contains(key)
    }
}
