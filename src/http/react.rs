// React track: serve the Vite build output under `/react/`.
//
// The React dashboard is built by Vite into `assets/react/dist/`
// (`npm run build:react`) and the output is committed, so the server
// only serves files from disk at runtime — Node/npm are build-time
// only and the server never builds anything here. The files are
// already minified by esbuild, so they bypass the startup
// minification pipeline (`crate::themes::assets::minify`) entirely:
// the bytes on disk are the bytes served.
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
// A `/react/*` path that matches no file serves `dist/index.html`
// (SPA fallback), so client-side routes resolve once React mounts.

use crate::themes::assets::{StaticAssetConfig, content_type_for};
use actix_web::{HttpRequest, HttpResponse, web};
use std::path::{Path, PathBuf};
use unified_core::error::UnifiedError;
use unified_core::json;

/// Mount point of the React track. A constant, not a config value:
/// the Vite `base` (`/react/`) is baked into the committed build
/// output — `dist/index.html` references
/// `/react/assets/index-<hash>.js` — so a runtime rename would serve
/// a shell whose assets 404. Renaming the mount means rebuilding
/// with a new base.
pub const REACT_MOUNT: &str = "/react";

/// Build output directory, relative to the global assets directory
/// (`assets/react/dist/`).
const REACT_DIST_DIR: &str = "react/dist";

/// Long cache lifetime for hashed assets (one year), matching the
/// fingerprinted-asset headers in `crate::themes::assets`.
const HASHED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Cache lifetime for unhashed files, matching the
/// unfingerprinted-asset headers in `crate::themes::assets`.
const PLAIN_CACHE_CONTROL: &str = "public, max-age=3600";

/// `GET /react`: the SPA shell.
pub async fn serve_react_root(
    config: web::Data<StaticAssetConfig>,
    _req: HttpRequest,
) -> HttpResponse {
    serve_react_path(&config, "")
}

/// `GET /react/{path:.*}`: one built file, or the SPA shell when the
/// path matches no file (a client-side route).
pub async fn serve_react(
    config: web::Data<StaticAssetConfig>,
    _req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    serve_react_path(&config, &path.into_inner())
}

/// Serve one React path: the shell for an empty path, the file when
/// it exists under the dist directory, the shell again for any other
/// path (SPA fallback). A path that tries to escape the dist
/// directory is a 400, never the shell and never a file.
fn serve_react_path(config: &StaticAssetConfig, relative: &str) -> HttpResponse {
    let base = config.assets_dir.join(REACT_DIST_DIR);
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
    // The canonicalized file must stay inside the canonicalized dist
    // directory: a symlink inside `dist/` pointing outside it serves
    // nothing. A missing file is not an error here — it falls through
    // to the SPA shell below.
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

/// Serve `dist/index.html` with `no-cache`. A missing build output
/// is a 404 in the standard error envelope, naming the file that is
/// absent so the operator knows to run `npm run build:react`.
fn serve_index(base: &Path, debug: bool) -> HttpResponse {
    match std::fs::read(base.join("index.html")) {
        Ok(bytes) => HttpResponse::Ok()
            .content_type("text/html")
            .insert_header(("Cache-Control", "no-cache"))
            .body(bytes),
        Err(_) => json::error_response(
            &UnifiedError::not_found(
                "React track is not built: assets/react/dist/index.html is missing (run `npm run build:react`)",
            ),
            debug,
        ),
    }
}

/// True when the relative path stays inside the dist directory:
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
/// The handler itself resolves from `StaticAssetConfig`.
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
}
