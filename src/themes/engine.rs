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

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::themes::assets::resolve_asset_url;
use handlebars::{Context, Handlebars, Helper, Output, RenderContext, RenderError, RenderErrorReason};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Id of the embedded default theme. It is always available.
pub const DEFAULT_THEME_ID: &str = "default";

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
struct EmbeddedTemplates;

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
fn validate_template_name(name: &str) -> UnifiedResult<()> {
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
fn embedded_default_meta() -> ThemeMeta {
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
}

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

    /// Render a registered template with the given JSON data.
    ///
    /// Returns a 404 `UnifiedError` when the template is not registered
    /// and a 500 `UnifiedError` when rendering fails.
    pub fn render(&self, template: &str, data: &Value) -> UnifiedResult<String> {
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
        let handlebars = self
            .handlebars
            .read()
            .expect("handlebars lock poisoned");
        handlebars.render(template, data).map_err(|error| {
            UnifiedError::internal(format!("Failed to render template \"{template}\": {error}"))
                .with_metadata("template", Value::String(template.to_string()))
                .with_trace(format!("{error:?}"))
        })
    }

    /// Render a template of the active theme, resolving `layout` through
    /// the theme's own layout when the caller passes a page template.
    /// This is a convenience wrapper over `render`; page templates are
    /// expected to extend the layout with `{{> header}}` / `{{> footer}}`
    /// partials themselves.
    pub fn render_page(&self, template: &str, data: &Value) -> UnifiedResult<String> {
        self.render(template, data)
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

    /// Activate a theme: validate and load it, then make it the active
    /// theme for subsequent `render` calls.
    pub fn set_active_theme(&self, id: &str) -> UnifiedResult<()> {
        let meta = self.load_theme(id)?;
        *self
            .active_theme
            .write()
            .expect("active theme lock poisoned") = meta.id;
        Ok(())
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
            // `layout.hbs` -> `layout`; `partials/header.hbs` -> `header`.
            let stem = relative.strip_suffix(".hbs").unwrap_or(relative);
            let name = stem.strip_prefix("partials/").unwrap_or(stem).to_string();
            validate_template_name(&name)?;
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
    fn swap_registry(&self, handlebars: Handlebars<'static>, rendered: &[(String, String)]) {
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
    }

    /// Register the template helpers on a fresh Handlebars registry:
    /// `asset_url` resolves an asset path to its public, fingerprinted
    /// URL (see `ThemeEngine::asset_url`). The helper reads the active
    /// theme id at render time, so switching themes needs no
    /// re-registration.
    fn register_helpers(&self, handlebars: &mut Handlebars<'static>) {
        let assets_dir = self.assets_dir.clone();
        let themes_dir = self.themes_dir.clone();
        let active_theme = Arc::clone(&self.active_theme);
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
    }

    /// Build a fresh Handlebars registry from `(name, source)` pairs
    /// without touching the live registry, and register the template
    /// helpers on it. `error_prefix` names the theme in compile
    /// errors, for example `Theme "dark"` or `Embedded default theme`.
    fn build_registry(
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

/// Reject a disk theme template or partial larger than
/// `MAX_TEMPLATE_SIZE`. An unreadable file size does not fail the
/// check — the read itself reports that error next.
fn check_template_size(path: &Path, theme_id: &str, name: &str) -> UnifiedResult<()> {
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
