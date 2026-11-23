// engine — manifest.
// Theme identity: constants, metadata, manifest, embedded theme.
//
// Split from `src/themes/engine.rs`; moved verbatim, no behaviour changes.

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
/// Id of the embedded default theme. It is always available.
pub const DEFAULT_THEME_ID: &str = "default";

/// Hook event emitted (by the theme module) after a theme is
/// activated, and on startup with the persisted active theme, so the
/// rendering engines can switch to it. Payload:
/// `{ "theme_id": "<id>", "active": true }`. The engines subscribe
/// to this event; nothing else switches the rendered theme.
pub const THEME_ACTIVATED_EVENT: &str = "theme.activated";

/// Templates every complete theme must provide.
pub const REQUIRED_TEMPLATES: &[&str] = &["layout", "dashboard", "home", "error"];

/// Maximum size of one disk theme template or partial. Larger files
/// are rejected at load time so a broken or hostile theme cannot make
/// the engine read unbounded data into memory.
pub const MAX_TEMPLATE_SIZE: u64 = 5 * 1024 * 1024;

/// Templates embedded in the binary, from the `templates/` directory at
/// the crate root. Paths are relative to that directory, for example
/// `default/layout.hbs`.
#[derive(RustEmbed)]
#[folder = "templates/"]
pub(crate) struct EmbeddedTemplates;

/// Public metadata for one theme, as listed by `ThemeEngine::list_themes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// True when the theme ships a screenshot file.
    pub screenshot: bool,
}

/// The on-disk `theme.yaml` manifest of one theme.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Screenshot file name relative to the theme directory, for example
    /// `screenshot.png`. Empty or missing means no screenshot.
    #[serde(default)]
    pub screenshot: Option<String>,
    /// Subdirectories the theme ships, for example `templates` and
    /// `partials`. Informational; the engine reads `templates/` and
    /// `partials/` by convention.
    #[serde(default)]
    pub directories: Vec<String>,
    /// Template names (without extension) the theme provides, for
    /// example `layout`, `dashboard`, `home`, `error`.
    #[serde(default)]
    pub templates: Vec<String>,
    /// Optional mapping of logical template names to file stems inside
    /// `templates/` (without the `.hbs` extension), for example
    /// `layout: main` loads the `layout` template from
    /// `templates/main.hbs`. A name without a mapping loads from
    /// `templates/<name>.hbs`. Mapped stems follow the same naming
    /// rules as template names (letters, digits, `-`, `_`).
    #[serde(default)]
    pub template_files: HashMap<String, String>,
}

/// Validate a theme id. Only letters, digits, `-` and `_` are allowed, so
/// the id can never contain a path separator, `.`, or `..`.
pub fn validate_theme_id(id: &str) -> UnifiedResult<()> {
    if id.is_empty() {
        return Err(UnifiedError::bad_request("Theme id must not be empty"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid theme id \"{id}\": only letters, digits, '-' and '_' are allowed"
        )));
    }
    Ok(())
}

/// Validate a template name (no directories, no extension, no traversal).
pub(crate) fn validate_template_name(name: &str) -> UnifiedResult<()> {
    if name.is_empty() {
        return Err(UnifiedError::bad_request(
            "Template name must not be empty",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid template name \"{name}\": only letters, digits, '-' and '_' are allowed"
        )));
    }
    Ok(())
}

/// Metadata of the embedded default theme, parsed from its `theme.yaml`.
/// Falls back to built-in values when the manifest cannot be parsed.
pub(crate) fn embedded_default_meta() -> ThemeMeta {
    let fallback = ThemeMeta {
        id: DEFAULT_THEME_ID.to_string(),
        name: "Default".to_string(),
        version: "1.0.0".to_string(),
        description: "Default Unified theme: dashboard, public home page, and error page."
            .to_string(),
        screenshot: false,
    };
    let Some(file) = EmbeddedTemplates::get("default/theme.yaml") else {
        return fallback;
    };
    let Ok(text) = std::str::from_utf8(&file.data) else {
        return fallback;
    };
    let Ok(manifest) = serde_yaml::from_str::<ThemeManifest>(text) else {
        return fallback;
    };
    ThemeMeta {
        id: DEFAULT_THEME_ID.to_string(),
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        screenshot: manifest
            .screenshot
            .as_deref()
            .is_some_and(|name| EmbeddedTemplates::get(&format!("default/{name}")).is_some()),
    }
}
