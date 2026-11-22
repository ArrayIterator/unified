// Theme Management module.
//
// Discovers themes from the `themes/` directory plus an embedded default
// theme, validates each theme strictly (manifest, semver version, layout
// template, no path traversal, no files escaping the theme directory),
// and activates one theme at a time. The active theme id is persisted
// to `data/theme.json`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
use actix_web::{web, HttpResponse};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeTemplates {
    pub layout: String,
    pub index: Option<String>,
    pub post: Option<String>,
    pub page: Option<String>,
}

impl Default for ThemeTemplates {
    fn default() -> Self {
        Self {
            layout: "layout.html".to_string(),
            index: None,
            post: None,
            page: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub templates: ThemeTemplates,
    pub screenshot: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub active: bool,
    pub source: String,
    pub path: Option<String>,
    pub screenshot: bool,
    pub templates: Vec<String>,
    pub valid: bool,
    pub problems: Vec<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ThemeState {
    pub themes_dir: PathBuf,
    pub active: Arc<RwLock<String>>,
    pub state_file: PathBuf,
}

impl ThemeState {
    pub fn new(themes_dir: PathBuf, state_file: PathBuf) -> Self {
        Self {
            themes_dir,
            active: Arc::new(RwLock::new("default".to_string())),
            state_file,
        }
    }

    pub fn active_id(&self) -> String {
        self.active.read().expect("theme lock poisoned").clone()
    }

    pub fn set_active(&self, id: &str) {
        *self.active.write().expect("theme lock poisoned") = id.to_string();
    }

    /// Load the persisted active theme id. An unknown or malformed id
    /// falls back to the embedded default theme.
    pub fn load_active(&self) {
        let Ok(text) = std::fs::read_to_string(&self.state_file) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Some(id) = value.get("active").and_then(|value| value.as_str()) {
            if validate_theme_id(id).is_ok() {
                self.set_active(id);
            }
        }
    }

    pub async fn persist_active(&self) -> UnifiedResult<()> {
        let text = serde_json::to_string_pretty(&json!({ "active": self.active_id() }))
            .map_err(UnifiedError::from)?;
        if let Some(parent) = self.state_file.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(UnifiedError::from)?;
        }
        let temp = self.state_file.with_extension("json.tmp");
        tokio::fs::write(&temp, text.as_bytes())
            .await
            .map_err(UnifiedError::from)?;
        tokio::fs::rename(&temp, &self.state_file)
            .await
            .map_err(UnifiedError::from)?;
        Ok(())
    }

    /// Synchronous version of `persist_active`, for the synchronous
    /// `theme.activate_requested` hook handler (hook handlers cannot
    /// await). Same format and same atomic write-temp-then-rename.
    pub fn persist_active_sync(&self) -> UnifiedResult<()> {
        let text = serde_json::to_string_pretty(&json!({ "active": self.active_id() }))
            .map_err(UnifiedError::from)?;
        if let Some(parent) = self.state_file.parent() {
            std::fs::create_dir_all(parent).map_err(UnifiedError::from)?;
        }
        let temp = self.state_file.with_extension("json.tmp");
        std::fs::write(&temp, text.as_bytes()).map_err(UnifiedError::from)?;
        std::fs::rename(&temp, &self.state_file).map_err(UnifiedError::from)?;
        Ok(())
    }
}

impl Default for ThemeState {
    fn default() -> Self {
        Self::new(
            PathBuf::from("themes"),
            PathBuf::from("data/theme.json"),
        )
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A theme id must be a safe directory name: lowercase letters, digits,
/// `-` and `_`, starting with a letter or digit. Anything else is
/// rejected before it ever touches the filesystem.
pub fn validate_theme_id(id: &str) -> Result<String, UnifiedError> {
    let pattern =
        Regex::new(r"^[a-z0-9][a-z0-9_-]*$").expect("static regex is valid");
    if !pattern.is_match(id) {
        return Err(UnifiedError::bad_request(format!(
            "invalid theme id \"{id}\": use lowercase letters, digits, '-' or '_'"
        )));
    }
    if id.chars().count() > 100 {
        return Err(UnifiedError::bad_request(
            "theme id must be at most 100 characters",
        ));
    }
    Ok(id.to_string())
}

const SCREENSHOT_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// Check one declared template path: it must be relative, must not
/// contain `..`, must exist, and must resolve inside the theme directory.
fn check_template_file(
    dir: &Path,
    canonical_dir: &Path,
    declared: &str,
    field: &str,
    problems: &mut Vec<String>,
) {
    if declared.trim().is_empty() {
        problems.push(format!("{field} must not be empty"));
        return;
    }
    let declared_path = Path::new(declared);
    if declared_path.is_absolute() {
        problems.push(format!(
            "{field} must be a relative path, got \"{declared}\""
        ));
        return;
    }
    if declared
        .split('/')
        .any(|segment| segment == "..")
    {
        problems.push(format!(
            "{field} must not contain \"..\": \"{declared}\""
        ));
        return;
    }
    let full = dir.join(declared_path);
    if !full.is_file() {
        problems.push(format!(
            "{field} file \"{declared}\" does not exist"
        ));
        return;
    }
    match full.canonicalize() {
        Ok(canonical) => {
            if !canonical.starts_with(canonical_dir) {
                problems.push(format!(
                    "{field} file \"{declared}\" escapes the theme directory"
                ));
            }
        }
        Err(_) => problems.push(format!(
            "{field} file \"{declared}\" cannot be resolved"
        )),
    }
}

/// Validate a theme directory thoroughly. Returns the parsed manifest on
/// success, or the full list of problems on failure.
pub fn validate_theme(dir: &Path) -> Result<ThemeManifest, Vec<String>> {
    let mut problems: Vec<String> = Vec::new();
    if !dir.is_dir() {
        problems.push("theme directory does not exist".to_string());
        return Err(problems);
    }
    let manifest_path = dir.join("theme.yaml");
    if !manifest_path.is_file() {
        problems.push("theme.yaml is missing".to_string());
        return Err(problems);
    }
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) => {
            problems.push(format!("cannot read theme.yaml: {error}"));
            return Err(problems);
        }
    };
    let manifest: ThemeManifest = match serde_yaml::from_str(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            problems.push(format!("theme.yaml is not valid YAML: {error}"));
            return Err(problems);
        }
    };
    if manifest.name.trim().is_empty() {
        problems.push("theme.yaml: name must not be empty".to_string());
    }
    if manifest.name.chars().count() > 200 {
        problems.push("theme.yaml: name must be at most 200 characters".to_string());
    }
    if manifest.version.trim().is_empty() {
        problems.push("theme.yaml: version must not be empty".to_string());
    } else if Version::parse(manifest.version.trim()).is_err() {
        problems.push(format!(
            "theme.yaml: version \"{}\" is not a valid semver version",
            manifest.version
        ));
    }
    if manifest.description.trim().is_empty() {
        problems.push("theme.yaml: description must not be empty".to_string());
    }
    let canonical_dir = match dir.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            problems.push("theme directory cannot be resolved".to_string());
            return Err(problems);
        }
    };
    check_template_file(
        dir,
        &canonical_dir,
        &manifest.templates.layout,
        "templates.layout",
        &mut problems,
    );
    for (field, declared) in [
        ("templates.index", &manifest.templates.index),
        ("templates.post", &manifest.templates.post),
        ("templates.page", &manifest.templates.page),
    ] {
        if let Some(declared) = declared {
            check_template_file(dir, &canonical_dir, declared, field, &mut problems);
        }
    }
    if let Some(screenshot) = &manifest.screenshot {
        let extension = Path::new(screenshot)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !SCREENSHOT_EXTENSIONS.contains(&extension.as_str()) {
            problems.push(format!(
                "screenshot \"{screenshot}\" must be a png, jpg, jpeg, or webp file"
            ));
        } else {
            check_template_file(dir, &canonical_dir, screenshot, "screenshot", &mut problems);
        }
    }
    if problems.is_empty() {
        Ok(manifest)
    } else {
        Err(problems)
    }
}

fn screenshot_present(dir: &Path) -> bool {
    ["screenshot.png", "screenshot.jpg", "screenshot.jpeg", "screenshot.webp"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

fn template_files(dir: &Path, manifest: &ThemeManifest) -> Vec<String> {
    let mut files = vec![manifest.templates.layout.clone()];
    for declared in [
        &manifest.templates.index,
        &manifest.templates.post,
        &manifest.templates.page,
    ] {
        if let Some(declared) = declared {
            files.push(declared.clone());
        }
    }
    files.sort();
    files.dedup();
    // Keep only files that actually exist, so the listing is honest.
    files
        .into_iter()
        .filter(|file| dir.join(file).is_file())
        .collect()
}

fn embedded_default_info(active_id: &str) -> ThemeInfo {
    ThemeInfo {
        id: "default".to_string(),
        name: "Default".to_string(),
        version: "1.0.0".to_string(),
        description: "Built-in default theme.".to_string(),
        author: None,
        active: active_id == "default",
        source: "embedded".to_string(),
        path: None,
        screenshot: false,
        templates: vec!["layout.html".to_string()],
        valid: true,
        problems: Vec::new(),
    }
}

/// Discover every theme: the embedded default first, then each directory
/// under `themes_dir` that contains a `theme.yaml`. Invalid themes are
/// still listed, with `valid: false` and their problems, so an admin can
/// see what needs fixing.
fn discover_themes(state: &ThemeState) -> Vec<ThemeInfo> {
    let active_id = state.active_id();
    let mut themes = vec![embedded_default_info(&active_id)];
    let Ok(entries) = std::fs::read_dir(&state.themes_dir) else {
        return themes;
    };
    let mut discovered: Vec<ThemeInfo> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if validate_theme_id(&id).is_err() {
            continue;
        }
        match validate_theme(&path) {
            Ok(manifest) => discovered.push(ThemeInfo {
                id: id.clone(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                description: manifest.description.clone(),
                author: manifest.author.clone(),
                active: active_id == id,
                source: "directory".to_string(),
                path: Some(path.to_string_lossy().to_string()),
                screenshot: manifest.screenshot.is_some()
                    || screenshot_present(&path),
                templates: template_files(&path, &manifest),
                valid: true,
                problems: Vec::new(),
            }),
            Err(problems) => discovered.push(ThemeInfo {
                id: id.clone(),
                name: id.clone(),
                version: String::new(),
                description: String::new(),
                author: None,
                active: false,
                source: "directory".to_string(),
                path: Some(path.to_string_lossy().to_string()),
                screenshot: screenshot_present(&path),
                templates: Vec::new(),
                valid: false,
                problems,
            }),
        }
    }
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    themes.append(&mut discovered);
    themes
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_themes(state: web::Data<ThemeState>) -> HttpResponse {
    let themes = discover_themes(&state);
    api_json::success_response(json!({ "themes": themes }), 200)
}

async fn get_theme(state: web::Data<ThemeState>, id: web::Path<String>) -> HttpResponse {
    let id = id.into_inner();
    if let Err(error) = validate_theme_id(&id) {
        return api_json::error_response(&error, false);
    }
    let themes = discover_themes(&state);
    match themes.into_iter().find(|theme| theme.id == id) {
        Some(theme) => api_json::success_response(theme, 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("theme \"{id}\" not found")),
            false,
        ),
    }
}

async fn activate_theme(
    state: web::Data<ThemeState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let id = id.into_inner();
    if let Err(error) = validate_theme_id(&id) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if id != "default" {
        let dir = state.themes_dir.join(&id);
        match validate_theme(&dir) {
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
    state.set_active(&id);
    if let Err(error) = state.persist_active().await {
        return api_json::error_response(&error, ctx.is_debug());
    }
    ctx.hooks.emit(
        "theme.activated",
        json!({ "theme_id": id, "active": true }),
    );
    let themes = discover_themes(&state);
    match themes.into_iter().find(|theme| theme.id == id) {
        Some(theme) => api_json::success_response(theme, 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("theme \"{id}\" not found")),
            ctx.is_debug(),
        ),
    }
}

async fn get_active_theme(state: web::Data<ThemeState>) -> HttpResponse {
    let active_id = state.active_id();
    let themes = discover_themes(&state);
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
fn on_theme_exists(state: &ThemeState, ctx: &mut HookContext) {
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
fn on_theme_render_block(ctx: &mut HookContext) {
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
fn on_theme_activate_requested(state: &ThemeState, ctx: &mut HookContext) {
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

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ThemeModule {
    state: ThemeState,
}

impl ThemeModule {
    pub fn new() -> Self {
        Self {
            state: ThemeState::default(),
        }
    }
}

impl Default for ThemeModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ThemeModule {
    fn id(&self) -> &'static str {
        "theme"
    }

    fn name(&self) -> &'static str {
        "Theme Management"
    }

    fn description(&self) -> &'static str {
        "Theme discovery, strict validation, and activation."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        self.state.load_active();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "theme.exists",
            "theme",
            Arc::new(move |ctx: &mut HookContext| on_theme_exists(&state, ctx)),
        );
        bus.register(
            "theme.render_block",
            "theme",
            Arc::new(on_theme_render_block),
        );
        let state = self.state.clone();
        bus.register(
            "theme.activate_requested",
            "theme",
            Arc::new(move |ctx: &mut HookContext| {
                on_theme_activate_requested(&state, ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/theme")
                .app_data(state)
                .route("/themes", web::get().to(list_themes))
                .route("/themes/{id}", web::get().to(get_theme))
                .route("/themes/{id}/activate", web::post().to(activate_theme))
                .route("/active", web::get().to(get_active_theme)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ThemeModule::new()));
}
