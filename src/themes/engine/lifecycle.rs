// engine — lifecycle.
// Engine construction, theme discovery, loading, validation.
//
// Split from `src/themes/engine.rs`; moved verbatim, no behaviour changes.

use super::manifest::EmbeddedTemplates;
use super::manifest::embedded_default_meta;
use super::manifest::validate_template_name;
use super::render::check_template_size;
use super::*;
use crate::themes::assets::resolve_asset_url;
use crate::themes::module_icons::{MODULE_ICONS_SPRITE, module_icon_use};
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
    /// Build a new engine: register the embedded default theme and
    /// discover themes from `themes_dir` on disk. Discovery never fails
    /// construction — an unreadable or invalid disk theme is skipped and
    /// reported through the log; it can still be retried later with
    /// `load_theme`, which returns the validation error.
    pub fn new(themes_dir: impl Into<PathBuf>) -> Self {
        let engine = Self {
            handlebars: RwLock::new(Handlebars::new()),
            themes_dir: themes_dir.into(),
            assets_dir: PathBuf::from("assets"),
            active_theme: Arc::new(RwLock::new(DEFAULT_THEME_ID.to_string())),
            known_templates: RwLock::new(HashSet::new()),
            discovered: RwLock::new(HashMap::new()),
            routes: Arc::new(RwLock::new(Arc::new(RouteRegistry::new()))),
            variables: Arc::new(VariableRegistry::with_builtins()),
            module_templates: ModuleTemplateRegistry::new(),
            theme_module_templates: RwLock::new(HashSet::new()),
        };
        if let Err(error) = engine.register_embedded_default() {
            tracing::warn!("Failed to register embedded default theme: {error}");
        }
        engine.discover_disk_themes();
        engine
    }

    /// Use a different directory for global assets (`/assets/...`).
    /// The default is `assets`, relative to the working directory.
    pub fn with_assets_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.assets_dir = dir.into();
        self
    }

    /// Attach the shared named-route registry (the same instance the
    /// application keeps on `AppContext`) so the `route_url` helper
    /// resolves config and settings overrides instead of the built-in
    /// defaults. Until this is called the helper resolves through a
    /// fresh default registry, so templates still render.
    pub fn set_route_registry(&self, registry: Arc<RouteRegistry>) {
        *self.routes.write().expect("route registry lock poisoned") = registry;
    }

    /// Builder form of `set_route_registry`.
    pub fn with_route_registry(self, registry: Arc<RouteRegistry>) -> Self {
        self.set_route_registry(registry);
        self
    }

    /// Resolve the effective path of a named route through the
    /// attached registry (or the built-in default when no registry
    /// is attached). An unknown name resolves to `/`.
    pub fn route_url(&self, name: &str) -> String {
        self.routes
            .read()
            .expect("route registry lock poisoned")
            .route_url(name)
    }

    /// Directory disk themes are discovered from.
    pub fn themes_dir(&self) -> &Path {
        &self.themes_dir
    }

    /// Directory global assets are served from.
    pub fn assets_dir(&self) -> &Path {
        &self.assets_dir
    }

    /// Id of the currently active theme.
    pub fn active_theme(&self) -> String {
        self.active_theme
            .read()
            .expect("active theme lock poisoned")
            .clone()
    }

    /// Resolve a public URL for an asset path, for templates and
    /// handlers: `theme:<path>` resolves to the active theme's assets
    /// directory, anything else to the global assets directory. The
    /// URL carries a `?v=` content fingerprint when the file exists
    /// on disk. Absolute URLs, absolute paths, fragments, and queries
    /// pass through unchanged. Also available inside templates as the
    /// `asset_url` helper, for example
    /// `{{asset_url "css/dashboard.css"}}`.
    pub fn asset_url(&self, path: &str) -> String {
        resolve_asset_url(
            &self.assets_dir,
            &self.themes_dir,
            &self.active_theme(),
            path,
        )
    }

    /// True when a theme with this id is known (embedded default or a
    /// discovered disk theme).
    pub fn has_theme(&self, id: &str) -> bool {
        if id == DEFAULT_THEME_ID {
            return true;
        }
        self.discovered
            .read()
            .expect("discovered themes lock poisoned")
            .contains_key(id)
    }

    /// Metadata of every known theme: the embedded default first, then
    /// discovered disk themes sorted by id.
    pub fn list_themes(&self) -> Vec<ThemeMeta> {
        let mut themes = vec![embedded_default_meta()];
        let mut disk: Vec<ThemeMeta> = self
            .discovered
            .read()
            .expect("discovered themes lock poisoned")
            .values()
            .cloned()
            .collect();
        disk.sort_by(|a, b| a.id.cmp(&b.id));
        themes.extend(disk);
        themes
    }

    /// Names of the templates currently registered and renderable.
    pub fn template_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .known_templates
            .read()
            .expect("known templates lock poisoned")
            .iter()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// The template variable registry backing this engine: the
    /// built-in variables (`site`, `user`, `tenant`, `settings`, the
    /// logic variables, `page`, `route`) plus any variable a module
    /// registered. Use it to register custom variables or inspect
    /// the catalog.
    pub fn variables(&self) -> &VariableRegistry {
        &self.variables
    }

    /// Register a custom template variable on this engine. A name that
    /// is already registered is rejected (HTTP 409); use
    /// `VariableRegistry::replace` through `variables()` to overwrite
    /// one deliberately.
    pub fn register_variable(
        &self,
        name: &str,
        description: &str,
        resolver: VariableResolver,
    ) -> UnifiedResult<()> {
        self.variables.register(name, description, resolver)
    }

    /// Load and validate a theme, register its templates, and return its
    /// metadata. Does not change the active theme; use
    /// `set_active_theme` for that.
    ///
    /// Strict validation: the theme id must pass `validate_theme_id`,
    /// `theme.yaml` must exist and parse, the manifest must name the
    /// theme and a version, every declared template file must exist, and
    /// the `layout` template must exist.
    pub fn load_theme(&self, id: &str) -> UnifiedResult<ThemeMeta> {
        validate_theme_id(id)?;
        if id == DEFAULT_THEME_ID {
            self.register_embedded_default()?;
            return Ok(embedded_default_meta());
        }

        let dir = self.themes_dir.join(id);
        let manifest_path = dir.join("theme.yaml");
        if !manifest_path.is_file() {
            return Err(UnifiedError::not_found(format!(
                "Theme \"{id}\" has no theme.yaml manifest"
            ))
            .with_metadata("theme_id", Value::String(id.to_string())));
        }
        let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
            UnifiedError::internal(format!("Cannot read theme manifest for \"{id}\": {error}"))
                .with_metadata("theme_id", Value::String(id.to_string()))
        })?;
        let manifest: ThemeManifest = serde_yaml::from_str(&text).map_err(|error| {
            UnifiedError::bad_request(format!(
                "Theme \"{id}\" has an invalid theme.yaml: {error}"
            ))
            .with_metadata("theme_id", Value::String(id.to_string()))
            .with_trace(format!("{error:?}"))
        })?;
        if manifest.name.trim().is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "Theme \"{id}\" has an empty name in theme.yaml"
            ))
            .with_metadata("theme_id", Value::String(id.to_string())));
        }
        if manifest.version.trim().is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "Theme \"{id}\" has an empty version in theme.yaml"
            ))
            .with_metadata("theme_id", Value::String(id.to_string())));
        }

        let templates_dir = dir.join("templates");
        let layout_path = templates_dir.join("layout.hbs");
        if !layout_path.is_file() {
            return Err(UnifiedError::bad_request(format!(
                "Theme \"{id}\" is missing the required layout template (templates/layout.hbs)"
            ))
            .with_metadata("theme_id", Value::String(id.to_string())));
        }

        // Template list: the manifest's list, or the required set when the
        // manifest declares none.
        let mut template_names: Vec<String> = if manifest.templates.is_empty() {
            REQUIRED_TEMPLATES.iter().map(|name| name.to_string()).collect()
        } else {
            manifest.templates.clone()
        };
        for name in REQUIRED_TEMPLATES {
            if !template_names.iter().any(|item| item == name) {
                template_names.push(name.to_string());
            }
        }

        // Read every template file before touching the live registry.
        // The registry is then built fresh and swapped in as a whole, so
        // a broken theme cannot leave a half-registered set behind.
        // A logical name may map to a different file stem through
        // `template_files` in theme.yaml, for example `layout: main`.
        let mut rendered: Vec<(String, String)> = Vec::new();
        for name in &template_names {
            validate_template_name(name).map_err(|error| {
                error.with_metadata("theme_id", Value::String(id.to_string()))
            })?;
            let stem = manifest
                .template_files
                .get(name)
                .map(String::as_str)
                .unwrap_or(name);
            validate_template_name(stem).map_err(|error| {
                error.with_metadata("theme_id", Value::String(id.to_string()))
            })?;
            let path = templates_dir.join(format!("{stem}.hbs"));
            if !path.is_file() {
                return Err(UnifiedError::bad_request(format!(
                    "Theme \"{id}\" declares template \"{name}\" but templates/{stem}.hbs is missing"
                ))
                .with_metadata("theme_id", Value::String(id.to_string())));
            }
            check_template_size(&path, id, name)?;
            let content = std::fs::read_to_string(&path).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot read template \"{name}\" of theme \"{id}\": {error}"
                ))
                .with_metadata("theme_id", Value::String(id.to_string()))
            })?;
            rendered.push((name.clone(), content));
        }

        // Optional partials: `partials/<name>.hbs`, registered under the
        // file stem so templates use them as `{{> header}}`.
        let partials_dir = dir.join("partials");
        if partials_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&partials_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_template = path.is_file()
                        && path.extension().is_some_and(|ext| ext == "hbs");
                    if !is_template {
                        continue;
                    }
                    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                        if validate_template_name(stem).is_ok() {
                            check_template_size(&path, id, stem)?;
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                rendered.push((stem.to_string(), content));
                            }
                        }
                    }
                }
            }
        }

        // Theme overrides for module templates:
        // `templates/<module>/<name>.hbs` (for example
        // `templates/billing/invoice.hbs`) is registered under
        // `module:<module>/<name>` and wins over the module's own
        // registered default; see `crate::themes::module_templates`.
        // Invalid module or template names are skipped with a warning.
        if let Ok(entries) = std::fs::read_dir(&templates_dir) {
            for entry in entries.flatten() {
                let module_path = entry.path();
                if !module_path.is_dir() {
                    continue;
                }
                let Some(module) = module_path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if validate_module_template_name(module).is_err() {
                    tracing::warn!(
                        "Theme \"{id}\": skipping module override directory with invalid name: {module}"
                    );
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&module_path) else {
                    continue;
                };
                for file in files.flatten() {
                    let path = file.path();
                    let is_template =
                        path.is_file() && path.extension().is_some_and(|ext| ext == "hbs");
                    if !is_template {
                        continue;
                    }
                    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                        continue;
                    };
                    if validate_module_template_name(stem).is_err() {
                        tracing::warn!(
                            "Theme \"{id}\": skipping module override with invalid name: {module}/{stem}"
                        );
                        continue;
                    }
                    check_template_size(&path, id, stem)?;
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        rendered.push((module_template_key(module, stem), content));
                    }
                }
            }
        }

        let fresh = self
            .build_registry(&rendered, &format!("Theme \"{id}\""))
            .map_err(|error| {
                error.with_metadata("theme_id", Value::String(id.to_string()))
            })?;
        self.swap_registry(fresh, &rendered);

        let screenshot = match manifest.screenshot.as_deref() {
            Some(name) if !name.trim().is_empty() => {
                // The screenshot name must stay inside the theme directory.
                let file_name = Path::new(name)
                    .file_name()
                    .map(|part| part.to_string_lossy().into_owned());
                match file_name {
                    Some(safe_name) => dir.join(safe_name).is_file(),
                    None => false,
                }
            }
            _ => dir.join("screenshot.png").is_file(),
        };
        let meta = ThemeMeta {
            id: id.to_string(),
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            screenshot,
        };
        self.discovered
            .write()
            .expect("discovered themes lock poisoned")
            .insert(id.to_string(), meta.clone());
        Ok(meta)
    }

    /// Activate a theme: validate it fully first, then load it and
    /// make it the active theme for subsequent `render` calls.
    /// Activation is impossible for an invalid theme: when the
    /// validity report has any error, this returns HTTP 400 listing
    /// every error (code and message) instead of switching. Warnings
    /// never block activation. The active theme is left untouched on
    /// failure.
    pub fn set_active_theme(&self, id: &str) -> UnifiedResult<()> {
        let report = self.validate_theme(id);
        if !report.valid {
            return Err(UnifiedError::bad_request(format!(
                "Theme \"{id}\" failed validation: {}",
                report.error_summary()
            ))
            .with_metadata("theme_id", Value::String(id.to_string()))
            .with_metadata(
                "validation_errors",
                Value::Array(
                    report
                        .error_codes()
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            ));
        }
        let meta = self.load_theme(id)?;
        *self
            .active_theme
            .write()
            .expect("active theme lock poisoned") = meta.id;
        Ok(())
    }

    /// Apply a `theme.activated` hook event to this engine: switch
    /// rendering to the event's `theme_id`. This is how activation
    /// reaches rendering — the theme module persists the active id
    /// and emits the event, and every engine subscribed to it calls
    /// this handler. Activation is validated by `set_active_theme`,
    /// so an unknown, invalid, or broken theme is logged and ignored
    /// and the previously active theme keeps rendering; the handler
    /// never fails the event. A missing `theme_id` is ignored the
    /// same way.
    pub fn on_theme_activated(&self, ctx: &mut HookContext) {
        let Some(id) = ctx
            .get("theme_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        else {
            tracing::warn!("Ignoring theme.activated event without a theme_id");
            return;
        };
        if let Err(error) = self.set_active_theme(&id) {
            tracing::warn!("Ignoring theme.activated for theme \"{id}\": {error}");
        }
    }

    /// Register the embedded default theme (all `templates/default/*.hbs`
    /// files plus `templates/default/partials/*.hbs`) into the live
    /// registry. Re-registering replaces any previously active disk
    /// theme's templates.
    fn register_embedded_default(&self) -> UnifiedResult<()> {
        let mut rendered: Vec<(String, String)> = Vec::new();
        for file in EmbeddedTemplates::iter() {
            let path = file.as_ref();
            let Some(relative) = path.strip_prefix("default/") else {
                continue;
            };
            if !relative.ends_with(".hbs") {
                continue;
            }
            // `layout.hbs` -> `layout`; `partials/header.hbs` ->
            // `header`; `<module>/<name>.hbs` ->
            // `module:<module>/<name>` (a theme override for a module
            // template). Deeper nesting is not a template the engine
            // serves and is skipped.
            let stem = relative.strip_suffix(".hbs").unwrap_or(relative);
            let name = if let Some(rest) = stem.strip_prefix("partials/") {
                validate_template_name(rest)?;
                rest.to_string()
            } else if let Some((module, template)) = stem.split_once('/') {
                if template.contains('/') {
                    continue;
                }
                validate_module_template_name(module)?;
                validate_module_template_name(template)?;
                module_template_key(module, template)
            } else {
                validate_template_name(stem)?;
                stem.to_string()
            };
            let Some(embedded) = EmbeddedTemplates::get(path) else {
                continue;
            };
            let content = String::from_utf8(embedded.data.into_owned()).map_err(|error| {
                UnifiedError::internal(format!(
                    "Embedded template \"{path}\" is not valid UTF-8: {error}"
                ))
            })?;
            rendered.push((name, content));
        }
        for name in REQUIRED_TEMPLATES {
            if !rendered.iter().any(|(item, _)| item == name) {
                return Err(UnifiedError::internal(format!(
                    "Embedded default theme is missing the required template \"{name}\""
                )));
            }
        }
        let fresh = self.build_registry(&rendered, "Embedded default theme")?;
        self.swap_registry(fresh, &rendered);
        Ok(())
    }

    /// Scan `themes_dir` for disk themes and record their metadata.
    /// Invalid themes are skipped with a warning instead of failing.
    fn discover_disk_themes(&self) {
        let Ok(entries) = std::fs::read_dir(&self.themes_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if validate_theme_id(id).is_err() {
                tracing::warn!("Skipping theme directory with invalid id: {id}");
                continue;
            }
            let manifest_path = path.join("theme.yaml");
            let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_yaml::from_str::<ThemeManifest>(&text) else {
                tracing::warn!("Skipping theme \"{id}\": theme.yaml does not parse");
                continue;
            };
            if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
                tracing::warn!("Skipping theme \"{id}\": theme.yaml has an empty name or version");
                continue;
            }
            let screenshot = match manifest.screenshot.as_deref() {
                Some(name) if !name.trim().is_empty() => {
                    let safe_name = Path::new(name)
                        .file_name()
                        .map(|part| part.to_string_lossy().into_owned());
                    safe_name
                        .map(|safe| path.join(safe).is_file())
                        .unwrap_or(false)
                }
                _ => path.join("screenshot.png").is_file(),
            };
            let meta = ThemeMeta {
                id: id.to_string(),
                name: manifest.name,
                version: manifest.version,
                description: manifest.description,
                screenshot,
            };
            self.discovered
                .write()
                .expect("discovered themes lock poisoned")
                .insert(id.to_string(), meta);
        }
    }
}

impl ThemeEngine {
    /// Swap the live Handlebars registry for a freshly built one and
    /// replace the known template set with exactly the new names, so no
    /// template from a previously active theme stays renderable.
    /// Registered module defaults are re-applied to the fresh
    /// registry for every key the new theme does not override, and
    /// the theme-override set is rebuilt from the new template names.
    pub(crate) fn swap_registry(
        &self,
        handlebars: Handlebars<'static>,
        rendered: &[(String, String)],
    ) {
        let mut handlebars = handlebars;
        let rendered_names: HashSet<String> =
            rendered.iter().map(|(name, _)| name.clone()).collect();
        let module_keys = self.module_templates.register_into(&mut handlebars, &rendered_names);
        *self
            .handlebars
            .write()
            .expect("handlebars lock poisoned") = handlebars;
        let mut known = self
            .known_templates
            .write()
            .expect("known templates lock poisoned");
        known.clear();
        for (name, _) in rendered {
            known.insert(name.clone());
        }
        for key in module_keys {
            known.insert(key);
        }
        let mut overrides = self
            .theme_module_templates
            .write()
            .expect("theme module templates lock poisoned");
        overrides.clear();
        for name in rendered_names {
            if name.starts_with(crate::themes::module_templates::MODULE_TEMPLATE_PREFIX) {
                overrides.insert(name);
            }
        }
    }

    /// Register the template helpers on a fresh Handlebars registry:
    /// `asset_url` resolves an asset path to its public, fingerprinted
    /// URL (see `ThemeEngine::asset_url`). The helper reads the active
    /// theme id at render time, so switching themes needs no
    /// re-registration. `route_url` resolves a named page route
    /// (`login`, `user_dashboard`, `admin_dashboard`, `status_page`,
    /// ...) through the shared route registry, so templates never
    /// hardcode a dashboard or auth path: `{{route_url
    /// "user_dashboard"}}` renders the route's effective path, and an
    /// optional second argument appends a sub-path, for example
    /// `{{route_url "user_dashboard" "/tasks"}}`. Unknown names fall
    /// back to `/`. The helper reads the attached registry at render
    /// time, so a registry attached after construction still applies.
    fn register_helpers(&self, handlebars: &mut Handlebars<'static>) {
        let assets_dir = self.assets_dir.clone();
        let themes_dir = self.themes_dir.clone();
        let active_theme = Arc::clone(&self.active_theme);
        let routes = Arc::clone(&self.routes);
        handlebars.register_helper(
            "route_url",
            Box::new(
                move |helper: &Helper,
                      _: &Handlebars,
                      _: &Context,
                      _: &mut RenderContext,
                      out: &mut dyn Output|
                      -> Result<(), RenderError> {
                    let name = helper
                        .param(0)
                        .and_then(|value| value.value().as_str())
                        .unwrap_or("");
                    let suffix = helper
                        .param(1)
                        .and_then(|value| value.value().as_str())
                        .unwrap_or("");
                    let base = routes
                        .read()
                        .expect("route registry lock poisoned")
                        .route_url(name);
                    let url = if suffix.is_empty() {
                        base
                    } else if let Some(rest) = suffix.strip_prefix('/') {
                        format!("{base}/{rest}")
                    } else {
                        format!("{base}/{suffix}")
                    };
                    out.write(&url).map_err(|error| {
                        RenderErrorReason::Other(format!("Failed to write route url: {error}"))
                    })?;
                    Ok(())
                },
            ),
        );
        handlebars.register_helper(
            "asset_url",
            Box::new(
                move |helper: &Helper,
                      _: &Handlebars,
                      _: &Context,
                      _: &mut RenderContext,
                      out: &mut dyn Output|
                      -> Result<(), RenderError> {
                    let path = helper
                        .param(0)
                        .and_then(|value| value.value().as_str())
                        .unwrap_or("");
                    let active = active_theme
                        .read()
                        .expect("active theme lock poisoned")
                        .clone();
                    let url = resolve_asset_url(&assets_dir, &themes_dir, &active, path);
                    out.write(&url).map_err(|error| {
                        RenderErrorReason::Other(format!("Failed to write asset url: {error}"))
                    })?;
                    Ok(())
                },
            ),
        );
        handlebars.register_helper(
            "module_icons_sprite",
            Box::new(
                move |_: &Helper,
                      _: &Handlebars,
                      _: &Context,
                      _: &mut RenderContext,
                      out: &mut dyn Output|
                      -> Result<(), RenderError> {
                    out.write(MODULE_ICONS_SPRITE).map_err(|error| {
                        RenderErrorReason::Other(format!(
                            "Failed to write module icons sprite: {error}"
                        ))
                    })?;
                    Ok(())
                },
            ),
        );
        handlebars.register_helper(
            "module_icon_use",
            Box::new(
                move |helper: &Helper,
                      _: &Handlebars,
                      _: &Context,
                      _: &mut RenderContext,
                      out: &mut dyn Output|
                      -> Result<(), RenderError> {
                    let name = helper
                        .param(0)
                        .and_then(|value| value.value().as_str())
                        .unwrap_or("");
                    let markup = module_icon_use(name);
                    out.write(&markup).map_err(|error| {
                        RenderErrorReason::Other(format!("Failed to write module icon: {error}"))
                    })?;
                    Ok(())
                },
            ),
        );
    }

    /// Build a fresh Handlebars registry from `(name, source)` pairs
    /// without touching the live registry, and register the template
    /// helpers on it. `error_prefix` names the theme in compile
    /// errors, for example `Theme "dark"` or `Embedded default theme`.
    pub(crate) fn build_registry(
        &self,
        rendered: &[(String, String)],
        error_prefix: &str,
    ) -> UnifiedResult<Handlebars<'static>> {
        let mut handlebars = Handlebars::new();
        for (name, content) in rendered {
            handlebars
                .register_template_string(name, content)
                .map_err(|error| {
                    UnifiedError::internal(format!(
                        "{error_prefix} template \"{name}\" failed to compile: {error}"
                    ))
                    .with_trace(format!("{error:?}"))
                })?;
        }
        self.register_helpers(&mut handlebars);
        Ok(handlebars)
    }
}

impl ThemeEngine {
    /// Validate a theme without activating it, collecting every issue
    /// into a `ThemeValidationReport` instead of failing on the first
    /// error. This never fails as an operation: even an unknown theme
    /// id or a missing theme directory comes back as a report with
    /// errors, so an admin UI can show broken themes without
    /// attempting activation. See `crate::themes::validity` for the
    /// error and warning codes. The report also re-checks the
    /// registered module templates (compiled at registration already).
    pub fn validate_theme(&self, id: &str) -> ThemeValidationReport {
        if let Err(error) = validate_theme_id(id) {
            let mut report = ThemeValidationReport::new(id);
            report.add_error(
                ThemeIssue::new("invalid-theme-id", error.message).with_path(id),
            );
            return report;
        }
        let module_sources = self.module_templates.sources();
        if id == DEFAULT_THEME_ID {
            let mut sources: Vec<(String, String)> = Vec::new();
            for file in EmbeddedTemplates::iter() {
                let path = file.as_ref();
                let Some(relative) = path.strip_prefix("default/") else {
                    continue;
                };
                if !relative.ends_with(".hbs") {
                    continue;
                }
                let stem = relative.strip_suffix(".hbs").unwrap_or(relative);
                let name = if let Some(rest) = stem.strip_prefix("partials/") {
                    rest.to_string()
                } else if let Some((module, template)) = stem.split_once('/') {
                    if template.contains('/') {
                        continue;
                    }
                    module_template_key(module, template)
                } else {
                    stem.to_string()
                };
                let Some(embedded) = EmbeddedTemplates::get(path) else {
                    continue;
                };
                let Ok(content) = String::from_utf8(embedded.data.into_owned()) else {
                    continue;
                };
                sources.push((name, content));
            }
            let mut report = report_for_sources(
                id,
                &sources,
                REQUIRED_TEMPLATES,
                embedded_default_meta().screenshot,
            );
            report.check_module_sources(&module_sources);
            return report;
        }
        validate_theme_directory(id, &self.themes_dir.join(id), &module_sources)
    }
}
