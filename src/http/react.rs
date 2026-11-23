// React track: serve the per-theme Vite build output under `/react/`.
// A theme that requires `react` is built by Vite into
// `assets/react/dist/<theme_id>/` (`npm run build:react:all`), and
// the output is committed, so the server only serves files from disk
// at runtime — Node/npm are build-time only and the server never
// builds anything here. The files are already minified by esbuild,
// so they bypass the startup minification pipeline
// (`crate::themes::assets::minify`) entirely: the bytes on disk are
// the bytes served.
//
// Which bundle answers `/react` is resolved from the active theme
// through the shared [`ThemeEngine`] (see `resolve_react_base`):
// the active theme's own bundle when it requires `react` and that
// bundle is built, otherwise the Untitled bundle — the React app is
// built on the Untitled component library, so Untitled is the React
// track's theme whatever HBS theme the server-rendered pages use.
// When neither bundle exists the track answers 404 naming the build
// command. There is no legacy single-build `dist/index.html` anymore:
// `vite build` always writes into `dist/<theme>/`.
//
// Authentication is the global `AuthMiddleware`, exactly as for the
// HBS user dashboard (`/dashboard`): `/react` is not on the public
// allowlist, so an anonymous request is rejected with 401 in the
// standard error envelope before this handler runs. There is no
// login redirect anywhere in the dashboard mechanism — the HBS
// dashboard answers 401 the same way — so this handler adds none.
// Tenant-host blocking is identical to `/dashboard` too: the server
// factory appends `/react` to the tenant middleware's blocked paths
// whenever `/dashboard` is blocked (see `src/http/server.rs`).
//
// Caching: the Vite content hash in the file name
// (`index-<hash>.js` / `index-<hash>.css`) is the fingerprint —
// hashed files are served immutable for a year, `index.html` is
// served `no-cache`, and any other file gets the same one-hour
// cache as an unfingerprinted theme asset. No `?v=` fingerprint is
// appended: the hash already changes whenever the bytes change.
// A `/react/*` path that matches no file serves the resolved
// bundle's `index.html` (SPA fallback), so client-side routes
// resolve once React mounts.

use crate::themes::assets::{content_type_for, StaticAssetConfig};
use crate::themes::engine::ThemeEngine;
use actix_web::{web, HttpRequest, HttpResponse};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use unified_core::error::UnifiedError;
use unified_core::json;

/// Mount point of the React track. A constant, not a config value:
/// the Vite `base` (`/react/`) is baked into the committed build
/// output — a per-theme `index.html` references
/// `/react/assets/index-<hash>.js` — so a runtime rename would serve
/// a shell whose assets 404. Renaming the mount means rebuilding
/// with a new base.
pub const REACT_MOUNT: &str = "/react";

/// Build output directory, relative to the global assets directory
/// (`assets/react/dist/`). Per-theme bundles live one level deeper:
/// `react/dist/<theme_id>/index.html`.
const REACT_DIST_DIR: &str = "react/dist";

/// Fallback theme for the React track: the app is built on the
/// Untitled component library, so when the active HBS theme has no
/// React bundle of its own, `/react` serves the Untitled bundle.
const REACT_FALLBACK_THEME: &str = "untitled";

/// Long cache lifetime for hashed assets (one year), matching the
/// fingerprinted-asset headers in `crate::themes::assets`.
const HASHED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Cache lifetime for unhashed files, matching the
/// unfingerprinted-asset headers in `crate::themes::assets`.
const PLAIN_CACHE_CONTROL: &str = "public, max-age=3600";

/// `GET /react`: the SPA shell.
pub async fn serve_react_root(
    config: web::Data<StaticAssetConfig>,
    engine: web::Data<Arc<ThemeEngine>>,
    _req: HttpRequest,
) -> HttpResponse {
    serve_react_path(&config, &engine, "")
}

/// `GET /react/{path:.*}`: one built file, or the SPA shell when the
/// path matches no file (a client-side route).
pub async fn serve_react(
    config: web::Data<StaticAssetConfig>,
    engine: web::Data<Arc<ThemeEngine>>,
    _req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    serve_react_path(&config, &engine, &path.into_inner())
}

/// Resolve the bundle directory that answers `/react` from the
/// active theme, through the engine — the only place a theme id
/// becomes a filesystem path here:
///
/// 1. The active theme's own bundle, when the active theme requires
///    `react` and that bundle is built.
/// 2. Otherwise the Untitled bundle (the React track's own theme).
/// 3. `None` when neither bundle exists: the caller answers 404
///    naming the build command instead of serving a stale shell.
///
/// The embedded default theme has no metadata, so it takes the
/// Untitled branch like any theme without a React bundle. A theme
/// whose bundle was deleted after activation also falls through to
/// Untitled rather than 404ing on a half-built theme.
fn resolve_react_base(engine: &ThemeEngine) -> Option<PathBuf> {
    if let Some(meta) = engine.active_theme_meta() {
        if meta.require.iter().any(|required| required == "react") {
            if let Some(bundle) = engine.react_bundle_dir(&meta.id) {
                return Some(bundle);
            }
        }
    }
    engine.react_bundle_dir(REACT_FALLBACK_THEME)
}

/// Serve one React path: the shell for an empty path, the file when
/// it exists under the resolved bundle directory, the shell again
/// for any other path (SPA fallback). A path that tries to escape
/// the bundle directory is a 400, never the shell and never a file.
/// When no bundle is built at all, the answer is a 404 naming the
/// build command instead of a shell.
fn serve_react_path(
    config: &StaticAssetConfig,
    engine: &ThemeEngine,
    relative: &str,
) -> HttpResponse {
    let Some(base) = resolve_react_base(engine) else {
        return json::error_response(
            &UnifiedError::not_found(
                "React track is not built: no per-theme bundle under assets/react/dist (run `npm run build:react:all`)",
            ),
            config.debug,
        );
    };
    if relative.is_empty() {
        return serve_index(&base, config.debug);
    }
    if !is_safe_relative_path(relative) {
        return json::error_response(
            &UnifiedError::bad_request(format!(
                "Invalid React path \"{relative}\": path traversal is not allowed"
            )),
            config.debug,
        );
    }
    // The canonicalized file must stay inside the canonicalized
    // bundle directory: a symlink inside the bundle pointing outside
    // it serves nothing. A missing file is not an error here — it
    // falls through to the SPA shell below.
    let Ok(canonical_base) = base.canonicalize() else {
        return serve_index(&base, config.debug);
    };
    let file_path = canonical_base.join(relative);
    let resolved = file_path.canonicalize().ok().filter(|canonical_file| {
        canonical_file.starts_with(&canonical_base) && canonical_file.is_file()
    });
    let Some(file) = resolved else {
        return serve_index(&base, config.debug);
    };
    let Ok(bytes) = std::fs::read(&file) else {
        return serve_index(&base, config.debug);
    };
    let file_name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    // The shell is never cached: a new build must reach browsers
    // immediately, and its hashed asset URLs change with it.
    let cache_control = if file_name == "index.html" {
        "no-cache"
    } else if is_hashed_filename(file_name) {
        HASHED_CACHE_CONTROL
    } else {
        PLAIN_CACHE_CONTROL
    };
    HttpResponse::Ok()
        .content_type(content_type_for(&file))
        .insert_header(("Cache-Control", cache_control))
        .body(bytes)
}

/// Serve `<bundle>/index.html` with `no-cache`. A missing shell is
/// a 404 in the standard error envelope, naming the file that is
/// absent so the operator knows to run `npm run build:react:all`.
/// (The bundle directory was resolved only when its `index.html`
/// existed, so this fires only when the file vanished mid-request.)
fn serve_index(base: &Path, debug: bool) -> HttpResponse {
    match std::fs::read(base.join("index.html")) {
        Ok(bytes) => HttpResponse::Ok()
            .content_type("text/html")
            .insert_header(("Cache-Control", "no-cache"))
            .body(bytes),
        Err(_) => json::error_response(
            &UnifiedError::not_found(format!(
                "React track is not built: {}/index.html is missing (run `npm run build:react:all`)",
                base.display()
            )),
            debug,
        ),
    }
}

/// True when the relative path stays inside the bundle directory:
/// not absolute and with no `..` segment. The canonicalization in
/// `serve_react_path` is the second check; this one rejects the
/// attempt before any filesystem access.
fn is_safe_relative_path(relative: &str) -> bool {
    let path = Path::new(relative);
    !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// True when the file name carries a Vite content hash: the stem
/// ends with `-` plus at least 8 alphanumeric characters
/// (`index-CBMk9mMd.js`, `index-CulBbyPw.css`). Vite hashes are
/// base64url-ish, not hex, so hex is not required.
fn is_hashed_filename(file_name: &str) -> bool {
    let Some(stem) = file_name.rsplit_once('.').map(|(stem, _)| stem) else {
        return false;
    };
    let Some((_, hash)) = stem.rsplit_once('-') else {
        return false;
    };
    hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Canonical dist directory for a relative assets dir, for tests.
/// The handler itself resolves from the engine.
#[cfg(test)]
fn dist_dir(assets_dir: &Path) -> PathBuf {
    assets_dir.join(REACT_DIST_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_path_rejects_traversal_and_absolute_paths() {
        assert!(is_safe_relative_path("assets/index-CBMk9mMd.js"));
        assert!(is_safe_relative_path("index.html"));
        assert!(!is_safe_relative_path("../index.html"));
        assert!(!is_safe_relative_path("assets/../../index.html"));
        assert!(!is_safe_relative_path("/etc/passwd"));
    }

    #[test]
    fn hashed_filename_matches_vite_output_only() {
        assert!(is_hashed_filename("index-CBMk9mMd.js"));
        assert!(is_hashed_filename("index-CulBbyPw.css"));
        assert!(!is_hashed_filename("index.html"));
        assert!(!is_hashed_filename("index.js"));
        assert!(!is_hashed_filename("logo.png"));
        // Short suffixes and non-alphanumeric hashes are not Vite hashes.
        assert!(!is_hashed_filename("index-abc123.js"));
        assert!(!is_hashed_filename("index-CBMk9mM!.js"));
    }

    #[test]
    fn dist_dir_is_react_dist_under_the_assets_dir() {
        assert_eq!(
            dist_dir(Path::new("assets")),
            PathBuf::from("assets/react/dist")
        );
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unified-react-resolve-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// Minimal valid theme: a `require: [react]` manifest plus the
    /// four templates the engine requires for activation.
    fn write_react_theme(base: &Path, id: &str) {
        let theme = base.join("themes").join(id);
        std::fs::create_dir_all(theme.join("templates")).expect("create theme dir");
        std::fs::write(
            theme.join("theme.yaml"),
            "name: Test\nversion: 1.2.3\ndescription: Test theme\ntemplates:\n  - layout\n  - dashboard\n  - home\n  - error\nrequire:\n  - react\nframework: untitled\n",
        )
        .expect("write manifest");
        for name in ["layout", "dashboard", "home", "error"] {
            std::fs::write(
                theme.join("templates").join(format!("{name}.hbs")),
                "<h1>{{title}}</h1>",
            )
            .expect("write template");
        }
    }

    fn write_bundle(dist: &Path, theme_id: &str) {
        let bundle = dist.join(theme_id);
        std::fs::create_dir_all(&bundle).expect("create bundle dir");
        std::fs::write(bundle.join("index.html"), "<html></html>").expect("write shell");
    }

    fn engine_with(base: &Path) -> ThemeEngine {
        ThemeEngine::new(base.join("themes")).with_react_dist_dir(base.join("react-dist"))
    }

    #[test]
    fn resolve_prefers_the_active_themes_own_bundle() {
        let base = test_dir("active");
        write_react_theme(&base, "reacty");
        write_bundle(&base.join("react-dist"), "reacty");
        write_bundle(&base.join("react-dist"), "untitled");
        let engine = engine_with(&base);
        engine.set_active_theme("reacty").expect("activate reacty");
        assert_eq!(
            resolve_react_base(&engine),
            Some(base.join("react-dist").join("reacty"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_falls_back_to_untitled_without_a_react_theme() {
        let base = test_dir("fallback");
        write_bundle(&base.join("react-dist"), "untitled");
        let engine = engine_with(&base);
        assert_eq!(engine.active_theme(), "default");
        assert_eq!(
            resolve_react_base(&engine),
            Some(base.join("react-dist").join("untitled"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_is_none_when_no_bundle_is_built() {
        let base = test_dir("none");
        let engine = engine_with(&base);
        assert_eq!(resolve_react_base(&engine), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_falls_back_to_untitled_when_the_active_bundle_is_missing() {
        let base = test_dir("missing");
        write_react_theme(&base, "reacty");
        write_bundle(&base.join("react-dist"), "reacty");
        write_bundle(&base.join("react-dist"), "untitled");
        let engine = engine_with(&base);
        engine.set_active_theme("reacty").expect("activate reacty");
        // The active theme's bundle is deleted after activation:
        // /react keeps serving Untitled instead of 404ing.
        std::fs::remove_dir_all(base.join("react-dist").join("reacty"))
            .expect("delete active bundle");
        assert_eq!(
            resolve_react_base(&engine),
            Some(base.join("react-dist").join("untitled"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
