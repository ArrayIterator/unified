// Asset manager: registration, dependency-ordered rendering, and
// fingerprinted static serving for CSS and JavaScript.
//
// Two pieces:
//
//   1. `AssetRegistry` / `AssetQueue` - modules and themes register
//      stylesheets and scripts by id, with dependencies (`inherits`),
//      HTML attributes (`async`, `defer`, `integrity`, `nonce`, ...),
//      and a header/footer placement for scripts. A per-render queue
//      resolves the dependencies (dependencies first, duplicates
//      removed, cycles rejected) and renders the `<link>` / `<script>`
//      tags for the page header and footer. Attribute names are
//      validated and values HTML-escaped, so a bad id or attribute
//      can never break out of a tag.
//
//   2. Static serving - `serve_asset` serves the global `assets/`
//      directory under `/assets/`, and `serve_theme_asset` serves one
//      theme's `assets/` directory under
//      `/theme/{id}/assets/`. Both reject path traversal, resolve the
//      file inside its base directory, and send long-lived immutable
//      cache headers when the URL carries a fingerprint query
//      (`?v=<hash>`). `fingerprinted_url` / `resolve_asset_url`
//      compute those fingerprints from file contents (first 12 hex
//      characters of the SHA-256 digest), so a changed file gets a new
//      URL and browsers never serve a stale copy.

/// Asset minification (JavaScript and CSS), declared here as a
/// child of the asset module: minification is part of the asset
/// pipeline, the same way `engine.rs` declares its own child
/// modules. See `minify.rs`.
pub mod minify;

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{HttpRequest, HttpResponse, web};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Kind of asset: a stylesheet or a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Css,
    Js,
}

/// Where an asset's content comes from: an external URL (or a path
/// under `/assets/`) or inline content rendered into the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetSource {
    Url(String),
    Inline(String),
}

/// One registered asset: an id, its kind, its source, the ids it
/// depends on (rendered before it), extra HTML attributes, and - for
/// scripts - whether it belongs in the page footer instead of the
/// header.
#[derive(Debug, Clone)]
pub struct Asset {
    pub id: String,
    pub kind: AssetKind,
    pub source: AssetSource,
    pub dependencies: Vec<String>,
    pub attributes: Vec<(String, String)>,
    /// Scripts only: render in the footer when true, in the header
    /// when false. Stylesheets always render in the header.
    pub footer: bool,
}

impl Asset {
    /// Render this asset as one HTML tag (`<link>` / `<style>` /
    /// `<script>`). Attribute names were validated at registration
    /// and values are escaped here.
    pub fn render(&self) -> String {
        let attrs = render_attributes(&self.attributes);
        match (&self.kind, &self.source) {
            (AssetKind::Css, AssetSource::Url(url)) => {
                let mut all = vec![
                    ("rel".to_string(), "stylesheet".to_string()),
                    ("href".to_string(), url.clone()),
                ];
                merge_attributes(&mut all, &self.attributes);
                format!("<link{}>", render_attributes(&all))
            }
            (AssetKind::Css, AssetSource::Inline(content)) => {
                format!("<style{attrs}>{content}</style>")
            }
            (AssetKind::Js, AssetSource::Url(url)) => {
                let mut all = vec![("src".to_string(), url.clone())];
                merge_attributes(&mut all, &self.attributes);
                format!("<script{}></script>", render_attributes(&all))
            }
            (AssetKind::Js, AssetSource::Inline(content)) => {
                format!("<script{attrs}>{content}</script>")
            }
        }
    }
}

/// Attribute names may only contain letters, digits, `-`, `_`, `:`,
/// and `.`. Anything else (spaces, quotes, `=`, `<`, `>`, `/`) is
/// rejected so an attribute can never break out of its tag.
fn valid_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

/// Validate and normalize attribute pairs: names are lowercased,
/// invalid names are rejected, and a later pair with the same name
/// replaces the earlier one.
fn normalize_attributes(
    attributes: &[(&str, &str)],
) -> UnifiedResult<Vec<(String, String)>> {
    let mut normalized: Vec<(String, String)> = Vec::new();
    for (name, value) in attributes {
        let name = name.trim().to_lowercase();
        if !valid_attribute_name(&name) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid HTML attribute name \"{name}\""
            )));
        }
        merge_attributes(&mut normalized, &[(name, value.to_string())]);
    }
    Ok(normalized)
}

/// Insert `extra` pairs into `base`, replacing any pair with the same
/// name. Keeps first-seen order for stable output.
fn merge_attributes(base: &mut Vec<(String, String)>, extra: &[(String, String)]) {
    for (name, value) in extra {
        if let Some(existing) = base.iter_mut().find(|(key, _)| key == name) {
            existing.1 = value.clone();
        } else {
            base.push((name.clone(), value.clone()));
        }
    }
}

/// Escape an attribute value for inclusion inside double quotes.
fn escape_attr_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Render attribute pairs as ` name="value"` (or a bare name for an
/// empty value, i.e. a boolean attribute such as `defer`).
fn render_attributes(attributes: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in attributes {
        if value.is_empty() {
            out.push(' ');
            out.push_str(name);
        } else {
            out.push(' ');
            out.push_str(name);
            out.push_str("=\"");
            out.push_str(&escape_attr_value(value));
            out.push('"');
        }
    }
    out
}

/// Validate an asset id: non-empty, letters, digits, `-` and `_` only.
fn validate_asset_id(id: &str) -> UnifiedResult<()> {
    if id.is_empty() {
        return Err(UnifiedError::bad_request("Asset id must not be empty"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid asset id \"{id}\": only letters, digits, '-' and '_' are allowed"
        )));
    }
    Ok(())
}

/// Global registry of CSS and JavaScript assets. Thread-safe; modules
/// register their assets once at startup.
pub struct AssetRegistry {
    assets: RwLock<HashMap<String, Arc<Asset>>>,
}

impl AssetRegistry {
    pub fn new() -> Self {
        Self {
            assets: RwLock::new(HashMap::new()),
        }
    }

    /// Register one asset. Fails on an empty or invalid id, on a
    /// duplicate id, or on an invalid attribute name.
    pub fn register(&self, asset: Asset) -> UnifiedResult<Arc<Asset>> {
        validate_asset_id(&asset.id)?;
        for (name, _) in &asset.attributes {
            if !valid_attribute_name(name) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid HTML attribute name \"{name}\""
                )));
            }
        }
        let mut assets = self.assets.write().expect("asset registry lock poisoned");
        if assets.contains_key(&asset.id) {
            return Err(UnifiedError::conflict(format!(
                "Asset \"{}\" is already registered",
                asset.id
            )));
        }
        let asset = Arc::new(asset);
        assets.insert(asset.id.clone(), Arc::clone(&asset));
        Ok(asset)
    }

    /// Register a stylesheet from a URL.
    pub fn register_css(
        &self,
        id: &str,
        url: &str,
        dependencies: &[&str],
        attributes: &[(&str, &str)],
    ) -> UnifiedResult<Arc<Asset>> {
        self.register(Asset {
            id: id.to_string(),
            kind: AssetKind::Css,
            source: AssetSource::Url(url.to_string()),
            dependencies: dependencies.iter().map(|item| item.to_string()).collect(),
            attributes: normalize_attributes(attributes)?,
            footer: false,
        })
    }

    /// Register a stylesheet with inline content.
    pub fn register_css_inline(
        &self,
        id: &str,
        content: &str,
        dependencies: &[&str],
        attributes: &[(&str, &str)],
    ) -> UnifiedResult<Arc<Asset>> {
        self.register(Asset {
            id: id.to_string(),
            kind: AssetKind::Css,
            source: AssetSource::Inline(content.to_string()),
            dependencies: dependencies.iter().map(|item| item.to_string()).collect(),
            attributes: normalize_attributes(attributes)?,
            footer: false,
        })
    }

    /// Register a script from a URL. `footer` selects the footer
    /// (true) or the header (false) placement.
    pub fn register_js(
        &self,
        id: &str,
        url: &str,
        dependencies: &[&str],
        attributes: &[(&str, &str)],
        footer: bool,
    ) -> UnifiedResult<Arc<Asset>> {
        self.register(Asset {
            id: id.to_string(),
            kind: AssetKind::Js,
            source: AssetSource::Url(url.to_string()),
            dependencies: dependencies.iter().map(|item| item.to_string()).collect(),
            attributes: normalize_attributes(attributes)?,
            footer,
        })
    }

    /// Register a script with inline content.
    pub fn register_js_inline(
        &self,
        id: &str,
        content: &str,
        dependencies: &[&str],
        attributes: &[(&str, &str)],
        footer: bool,
    ) -> UnifiedResult<Arc<Asset>> {
        self.register(Asset {
            id: id.to_string(),
            kind: AssetKind::Js,
            source: AssetSource::Inline(content.to_string()),
            dependencies: dependencies.iter().map(|item| item.to_string()).collect(),
            attributes: normalize_attributes(attributes)?,
            footer,
        })
    }

    /// Remove an asset from the registry.
    pub fn deregister(&self, id: &str) -> Option<Arc<Asset>> {
        self.assets
            .write()
            .expect("asset registry lock poisoned")
            .remove(id)
    }

    /// Look up one asset by id.
    pub fn get(&self, id: &str) -> Option<Arc<Asset>> {
        self.assets
            .read()
            .expect("asset registry lock poisoned")
            .get(id)
            .cloned()
    }

    /// True when an asset with this id is registered.
    pub fn is_registered(&self, id: &str) -> bool {
        self.assets
            .read()
            .expect("asset registry lock poisoned")
            .contains_key(id)
    }

    /// Ids of every registered asset, sorted.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .assets
            .read()
            .expect("asset registry lock poisoned")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

impl Default for AssetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-render queue of assets: enqueue ids, resolve their
/// dependencies in order (dependencies first, each asset rendered at
/// most once), then render the header and footer tag blocks. Build a
/// fresh queue for every page render; the queue borrows the registry.
pub struct AssetQueue<'a> {
    registry: &'a AssetRegistry,
    /// Asset ids in render order.
    order: Vec<String>,
    queued: HashSet<String>,
}

impl<'a> AssetQueue<'a> {
    pub fn new(registry: &'a AssetRegistry) -> Self {
        Self {
            registry,
            order: Vec::new(),
            queued: HashSet::new(),
        }
    }

    /// Enqueue an asset and its dependencies (dependencies first).
    /// Fails on an unknown id or on a dependency cycle.
    pub fn enqueue(&mut self, id: &str) -> UnifiedResult<()> {
        let mut visiting: Vec<String> = Vec::new();
        self.enqueue_inner(id, &mut visiting)
    }

    fn enqueue_inner(&mut self, id: &str, visiting: &mut Vec<String>) -> UnifiedResult<()> {
        if self.queued.contains(id) {
            return Ok(());
        }
        if visiting.iter().any(|item| item == id) {
            visiting.push(id.to_string());
            return Err(UnifiedError::bad_request(format!(
                "Asset dependency cycle: {}",
                visiting.join(" -> ")
            )));
        }
        let Some(asset) = self.registry.get(id) else {
            return Err(UnifiedError::not_found(format!(
                "Asset \"{id}\" is not registered"
            )));
        };
        visiting.push(id.to_string());
        for dependency in &asset.dependencies {
            self.enqueue_inner(dependency, visiting)?;
        }
        visiting.pop();
        self.queued.insert(id.to_string());
        self.order.push(id.to_string());
        Ok(())
    }

    /// Remove an asset from the queue (its dependencies stay queued).
    /// Returns true when the asset was queued.
    pub fn dequeue(&mut self, id: &str) -> bool {
        if !self.queued.remove(id) {
            return false;
        }
        self.order.retain(|item| item != id);
        true
    }

    /// True when the asset is queued.
    pub fn is_queued(&self, id: &str) -> bool {
        self.queued.contains(id)
    }

    /// Ids in render order.
    pub fn queued_ids(&self) -> &[String] {
        &self.order
    }

    /// Render the header tags: every queued stylesheet plus every
    /// queued header script, in dependency order.
    pub fn render_header(&self) -> String {
        let mut tags: Vec<String> = Vec::new();
        for id in &self.order {
            let Some(asset) = self.registry.get(id) else {
                continue;
            };
            if asset.kind == AssetKind::Css || !asset.footer {
                tags.push(asset.render());
            }
        }
        tags.join("\n")
    }

    /// Render the footer tags: every queued footer script, in
    /// dependency order.
    pub fn render_footer(&self) -> String {
        let mut tags: Vec<String> = Vec::new();
        for id in &self.order {
            let Some(asset) = self.registry.get(id) else {
                continue;
            };
            if asset.kind == AssetKind::Js && asset.footer {
                tags.push(asset.render());
            }
        }
        tags.join("\n")
    }
}

// Fingerprinting

/// Short content fingerprint of bytes: the first 12 hex characters
/// of the SHA-256 digest. The fingerprint of the exact bytes the
/// server serves - see `minify::served_fingerprint`.
pub fn fingerprint_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut fingerprint = String::with_capacity(12);
    for &byte in &hash[..6] {
        fingerprint.push(HEX[(byte >> 4) as usize] as char);
        fingerprint.push(HEX[(byte & 0x0f) as usize] as char);
    }
    fingerprint
}

/// Short content fingerprint of a file: the first 12 hex characters
/// of its SHA-256 digest. `None` when the file cannot be read.
pub fn fingerprint_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(fingerprint_bytes(&bytes))
}

/// Append a fingerprint query (`?v=<hash>`) to a URL when the file
/// exists, so a changed file gets a new URL. Returns the URL
/// unchanged when the file cannot be read.
pub fn fingerprinted_url(url: &str, file: &Path) -> String {
    match fingerprint_file(file) {
        Some(hash) => {
            let separator = if url.contains('?') { '&' } else { '?' };
            format!("{url}{separator}v={hash}")
        }
        None => url.to_string(),
    }
}

/// Resolve a public URL for an asset path, as used by the theme
/// engine's `asset_url` helper and by handlers that build asset URLs:
///
///   - Absolute URLs, absolute paths, fragments, and queries pass
///     through unchanged.
///   - `theme:<path>` resolves to the active theme's assets
///     directory, served under `/theme/<id>/assets/<path>`.
///   - Anything else resolves to the global assets directory,
///     served under `/assets/<path>`.
///
/// The returned URL carries a `?v=` fingerprint: for assets served
/// from the compiled maps (transpiled/minified JavaScript and
/// compiled stylesheets) the fingerprint hashes the exact served
/// bytes, registered at startup; otherwise it hashes the file on
/// disk when the file exists.
pub fn resolve_asset_url(
    assets_dir: &Path,
    themes_dir: &Path,
    active_theme: &str,
    path: &str,
) -> String {
    if path.is_empty()
        || path.starts_with("http://")
        || path.starts_with("https://")
        || path.starts_with("//")
        || path.starts_with('/')
        || path.starts_with('#')
        || path.starts_with('?')
    {
        return path.to_string();
    }
    if let Some(theme_path) = path.strip_prefix("theme:") {
        let theme_path = theme_path.trim_start_matches('/');
        // Never fingerprint outside the theme directory.
        if theme_path
            .split('/')
            .any(|segment| segment == "..")
        {
            return format!("/theme/{active_theme}/assets/{theme_path}");
        }
        let url = format!("/theme/{active_theme}/assets/{theme_path}");
        let file = themes_dir
            .join(active_theme)
            .join("assets")
            .join(theme_path);
        return fingerprinted_url(&url, &file);
    }
    if path.split('/').any(|segment| segment == "..") {
        return format!("/assets/{path}");
    }
    let url = format!("/assets/{path}");
    // Served from the compiled maps (transpiled/minified JavaScript
    // and compiled stylesheets held in RAM): the fingerprint hashes
    // the exact served bytes, registered at startup, so it changes
    // exactly when the served bytes change.
    if let Some(hash) = minify::served_fingerprint(path) {
        let separator = if url.contains('?') { '&' } else { '?' };
        return format!("{url}{separator}v={hash}");
    }
    let file = assets_dir.join(path);
    if file.is_file() {
        return fingerprinted_url(&url, &file);
    }
    // Dashboard scripts: no `.js` file exists on disk anymore - the
    // JavaScript is compiled from the `.ts` source at startup. The
    // fingerprint above already covers the compiled scripts (it
    // hashes the served bytes from the registry); this fallback
    // only applies to a script that failed to compile and is not
    // in the registry, so the fingerprint still changes when the
    // source changes.
    if let Some(ts_path) = path.strip_suffix(".js") {
        let ts_file = assets_dir.join(format!("{ts_path}.ts"));
        if ts_file.is_file() {
            return fingerprinted_url(&url, &ts_file);
        }
    }
    url
}

// Static serving

/// Directories the static asset handlers serve from. Defaults match
/// the theme module: `assets/` for global assets, `themes/` for
/// per-theme assets.
#[derive(Debug, Clone)]
pub struct StaticAssetConfig {
    pub assets_dir: PathBuf,
    pub themes_dir: PathBuf,
    /// Dashboard scripts compiled from TypeScript at startup
    /// (`assets/js/*.ts` -> JavaScript, via the `unified-ts` crate):
    /// public path (`js/<name>.js`) to compiled JavaScript source,
    /// held in RAM and served from memory by `serve_asset`. No `.js`
    /// files exist in the repository anymore.
    pub compiled_scripts: Arc<HashMap<String, String>>,
    /// Stylesheets compiled at startup (`assets/css/*.css`,
    /// minified in release builds, as written in debug builds):
    /// public path (`css/<name>.css`) to served CSS source, held
    /// in RAM and served from memory by `serve_asset`, mirroring
    /// `compiled_scripts`.
    pub compiled_styles: Arc<HashMap<String, String>>,
    /// Render error traces in responses (debug mode only).
    pub debug: bool,
}

impl Default for StaticAssetConfig {
    fn default() -> Self {
        Self {
            assets_dir: PathBuf::from("assets"),
            themes_dir: PathBuf::from("themes"),
            compiled_scripts: Arc::new(HashMap::new()),
            compiled_styles: Arc::new(HashMap::new()),
            debug: false,
        }
    }
}

/// `GET /assets/{path}`: serve a file from the global assets
/// directory - or, for a dashboard script, the JavaScript compiled
/// from its TypeScript source at startup and held in RAM (see
/// `StaticAssetConfig::compiled_scripts`).
pub async fn serve_asset(
    config: web::Data<StaticAssetConfig>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let relative = path.into_inner();
    if let Some(js) = config.compiled_scripts.get(&relative) {
        let cache_control = if has_fingerprint_query(&req) {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=3600"
        };
        return HttpResponse::Ok()
            .content_type("text/javascript")
            .insert_header(("Cache-Control", cache_control))
            .body(js.clone());
    }
    if let Some(css) = config.compiled_styles.get(&relative) {
        let cache_control = if has_fingerprint_query(&req) {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=3600"
        };
        return HttpResponse::Ok()
            .content_type("text/css")
            .insert_header(("Cache-Control", cache_control))
            .body(css.clone());
    }
    serve_file(&config.assets_dir, &relative, &req, config.debug)
}

/// `GET /theme/{id}/assets/{path}`: serve a file from one theme's
/// `assets/` directory. The theme id must be a valid theme id.
pub async fn serve_theme_asset(
    config: web::Data<StaticAssetConfig>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (id, relative) = path.into_inner();
    if crate::themes::engine::validate_theme_id(&id).is_err() {
        return json::error_response(
            &UnifiedError::bad_request(format!("Invalid theme id \"{id}\"")),
            config.debug,
        );
    }
    let base = config.themes_dir.join(&id).join("assets");
    serve_file(&base, &relative, &req, config.debug)
}

/// Content type by file extension. Unknown extensions fall back to
/// `application/octet-stream`. Shared with the React track handler
/// (`crate::http::react`), which serves the same extensions from the
/// Vite build output.
pub(crate) fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "map" | "json" => "application/json",
        "html" => "text/html",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    }
}

/// True when the request URL carries a `v` fingerprint query, which
/// means the URL changes whenever the file changes and the response
/// can be cached immutably.
fn has_fingerprint_query(req: &HttpRequest) -> bool {
    req.query_string()
        .split('&')
        .any(|pair| pair == "v" || pair.starts_with("v="))
}

/// Serve one file from inside `base_dir`. The relative path must stay
/// inside the directory: absolute paths and `..` segments are
/// rejected, and the canonicalized file must start with the
/// canonicalized base directory.
fn serve_file(base_dir: &Path, relative: &str, req: &HttpRequest, debug: bool) -> HttpResponse {
    if relative.is_empty() {
        return json::error_response(
            &UnifiedError::not_found("Asset path must not be empty"),
            debug,
        );
    }
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return json::error_response(
            &UnifiedError::bad_request(format!(
                "Invalid asset path \"{relative}\": path traversal is not allowed"
            )),
            debug,
        );
    }
    let Ok(canonical_base) = base_dir.canonicalize() else {
        return json::error_response(
            &UnifiedError::not_found("Asset directory not found"),
            debug,
        );
    };
    let file_path = canonical_base.join(relative_path);
    let Ok(canonical_file) = file_path.canonicalize() else {
        return json::error_response(
            &UnifiedError::not_found(format!("Asset \"{relative}\" not found")),
            debug,
        );
    };
    if !canonical_file.starts_with(&canonical_base) || !canonical_file.is_file() {
        return json::error_response(
            &UnifiedError::not_found(format!("Asset \"{relative}\" not found")),
            debug,
        );
    }
    let Ok(bytes) = std::fs::read(&canonical_file) else {
        return json::error_response(
            &UnifiedError::not_found(format!("Asset \"{relative}\" not found")),
            debug,
        );
    };
    let cache_control = if has_fingerprint_query(req) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    HttpResponse::Ok()
        .content_type(content_type_for(&canonical_file))
        .insert_header(("Cache-Control", cache_control))
        .body(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_file_hashes_first_six_digest_bytes_as_hex() {
        // SHA-256("abc") starts with ba7816bf8f01cfea...; only the
        // first 6 digest bytes (12 hex chars) are kept.
        let path = std::env::temp_dir().join("unified-fingerprint-test.txt");
        std::fs::write(&path, b"abc").expect("write test file");
        let fingerprint = fingerprint_file(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(fingerprint.as_deref(), Some("ba7816bf8f01"));
        assert_eq!(fingerprint_file(&path), None);
    }
}
