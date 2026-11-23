// Theme engine: Handlebars rendering for the Unified dashboard and public
// pages.
//
// Themes come from two sources:
//
//   1. The embedded default theme, compiled into the binary from
//      `templates/default/` with `rust-embed`. It is always available and
//      is the fallback whenever no other theme is active.
//   2. Disk themes, discovered from the configured themes directory
//      (`themes/<id>/theme.yaml` plus `themes/<id>/templates/*.hbs`).
//      A disk theme is only loaded after strict validation; its templates
//      replace the active template set when the theme is activated.
//
// Template names are plain identifiers (`layout`, `dashboard`, `home`,
// `error`, `header`, `footer`). Partials are registered under the same
// plain names, so a template includes one with `{{> header}}`. Theme ids
// are restricted to letters, digits, `-` and `_`, which makes path
// traversal through a theme id impossible.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::routes::RouteRegistry;
use crate::themes::assets::resolve_asset_url;
use crate::themes::module_templates::{
    MODULE_TEMPLATE_PREFIX, ModuleTemplateRegistry, module_template_key,
    validate_module_template_name,
};
use crate::themes::validity::{
    ThemeIssue, ThemeValidationReport, report_for_sources, validate_theme_directory,
};
use crate::themes::variables::{VariableContext, VariableRegistry, VariableResolver};
use handlebars::{Context, Handlebars, Helper, Output, RenderContext, RenderError, RenderErrorReason};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub mod lifecycle;
pub mod manifest;
pub mod module_templates;
pub mod render;
#[cfg(test)]
mod tests;

pub(crate) use self::lifecycle::*;
pub(crate) use self::manifest::*;
pub(crate) use self::module_templates::*;
pub(crate) use self::render::*;

pub struct ThemeEngine {
    handlebars: RwLock<Handlebars<'static>>,
    themes_dir: PathBuf,
    /// Directory global assets (`/assets/...`) are served from.
    assets_dir: PathBuf,
    /// Shared with the `asset_url` helper so it always resolves
    /// `theme:` paths against the theme that is active at render time.
    active_theme: Arc<RwLock<String>>,
    /// Names of the templates currently registered in the Handlebars
    /// registry. Tracked here so `render` can report a clean 404 for an
    /// unknown template instead of a generic render failure.
    known_templates: RwLock<HashSet<String>>,
    /// Metadata of disk themes discovered in `themes_dir`, keyed by id.
    discovered: RwLock<HashMap<String, ThemeMeta>>,
    /// Shared named-route registry the `route_url` helper resolves
    /// through. Defaults to a fresh registry (built-in default paths)
    /// until the application attaches the shared instance from
    /// `AppContext` with `set_route_registry`; the helper reads the
    /// current registry at render time, so attaching later takes
    /// effect without re-registering any template.
    routes: Arc<RwLock<Arc<RouteRegistry>>>,
    /// Template variable registry: named variables (`site`, `user`,
    /// `tenant`, `settings`, the logic variables, `page`, `route`,
    /// plus module-registered ones) resolved at render time and
    /// merged into the template context. Built-ins are registered at
    /// construction; see `crate::themes::variables`.
    variables: Arc<VariableRegistry>,
    /// Module-provided templates (`module:<module>/<name>`), registered
    /// from in-code sources; see `crate::themes::module_templates`.
    /// A theme override for a key wins over the stored default.
    module_templates: ModuleTemplateRegistry,
    /// Keys (`module:<module>/<name>`) the active theme overrides
    /// with its own `templates/<module>/<name>.hbs` files. Rebuilt on
    /// every registry swap, so a theme switch restores the module
    /// defaults for keys the new theme does not override.
    theme_module_templates: RwLock<HashSet<String>>,
}
