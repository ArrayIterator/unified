// engine — render.
// Template rendering.
//
// Split from `src/themes/engine.rs`; moved verbatim, no behaviour changes.

use super::manifest::validate_template_name;
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
    /// Render a registered template with the given JSON data.
    ///
    /// Registered variables are resolved against an empty
    /// `VariableContext` (default site, anonymous user, no tenant,
    /// default page kind `"custom"`) and merged into the template
    /// context first; the per-call data is then overlaid on top, so a
    /// call site can override a registered variable by passing the
    /// same key. The logic variables therefore read their defaults
    /// here: `is_home` / `is_admin` / `is_dashboard` are false,
    /// `is_logged_in` is false, and `has_data` is true exactly when
    /// the per-call data object is non-empty. Returns a 404
    /// `UnifiedError` when the template is not registered and a 500
    /// `UnifiedError` when rendering fails.
    pub fn render(&self, template: &str, data: &Value) -> UnifiedResult<String> {
        self.render_with_context(template, data, &VariableContext::new())
    }

    /// Render a registered template with the given JSON data and a
    /// render-time variable context.
    ///
    /// Every registered variable is resolved against `ctx` and merged
    /// into the template context first; the per-call `data` is then
    /// overlaid on top. Precedence on a name collision: per-call data
    /// wins, so a call site can override a registered variable (for
    /// example passing its own `site` object). A resolver that fails
    /// is logged and its variable omitted; it never fails the render.
    /// When `data` is not a JSON object the registered variables are
    /// not merged and `data` is rendered as-is.
    pub fn render_with_context(
        &self,
        template: &str,
        data: &Value,
        ctx: &VariableContext,
    ) -> UnifiedResult<String> {
        validate_template_name(template)?;
        let known = self
            .known_templates
            .read()
            .expect("known templates lock poisoned")
            .contains(template);
        if !known {
            return Err(UnifiedError::not_found(format!(
                "Template \"{template}\" is not registered in the active theme"
            ))
            .with_metadata("template", Value::String(template.to_string())));
        }
        let merged = self.merged_template_context(data, ctx);
        let handlebars = self
            .handlebars
            .read()
            .expect("handlebars lock poisoned");
        handlebars.render(template, &merged).map_err(|error| {
            UnifiedError::internal(format!("Failed to render template \"{template}\": {error}"))
                .with_metadata("template", Value::String(template.to_string()))
                .with_trace(format!("{error:?}"))
        })
    }

    /// Merge resolved registry variables with per-call template data.
    /// Registry variables form the base; per-call keys overwrite them.
    /// Finally `has_data` is injected (overriding any per-call value
    /// under that name): it is true exactly when the per-call data is
    /// a non-empty JSON object, so templates can branch on whether
    /// the call site passed any data at all
    /// (`{{#if has_data}}...{{/if}}`). It is injected here, not
    /// registered as a variable, because resolvers run before the
    /// merge and cannot see the per-call data. When `data` is not a
    /// JSON object no merge happens and `has_data` is not injected.
    pub(crate) fn merged_template_context(&self, data: &Value, ctx: &VariableContext) -> Value {
        let Some(call_data) = data.as_object() else {
            return data.clone();
        };
        let mut merged = match self.variables.resolve_all(ctx) {
            Ok(values) => values,
            Err(error) => {
                tracing::warn!("Failed to resolve template variables: {error}");
                serde_json::Map::new()
            }
        };
        for (key, value) in call_data {
            merged.insert(key.clone(), value.clone());
        }
        merged.insert(
            "has_data".to_string(),
            Value::Bool(!call_data.is_empty()),
        );
        Value::Object(merged)
    }

    /// Render a template of the active theme, resolving `layout` through
    /// the theme's own layout when the caller passes a page template.
    /// This is a convenience wrapper over `render`; page templates are
    /// expected to extend the layout with `{{> header}}` / `{{> footer}}`
    /// partials themselves.
    pub fn render_page(&self, template: &str, data: &Value) -> UnifiedResult<String> {
        self.render(template, data)
    }
}

/// Reject a disk theme template or partial larger than
/// `MAX_TEMPLATE_SIZE`. An unreadable file size does not fail the
/// check — the read itself reports that error next.
pub(crate) fn check_template_size(path: &Path, theme_id: &str, name: &str) -> UnifiedResult<()> {
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.len() > MAX_TEMPLATE_SIZE {
            return Err(UnifiedError::bad_request(format!(
                "Theme \"{theme_id}\" template \"{name}\" is too large: {} bytes (maximum {MAX_TEMPLATE_SIZE})",
                metadata.len()
            ))
            .with_metadata("theme_id", Value::String(theme_id.to_string())));
        }
    }
    Ok(())
}
