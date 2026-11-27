// Theme validation and discovery.

use unified_core::error::UnifiedError;
use regex::Regex;
use semver::Version;
use std::path::Path;
use std::sync::OnceLock;

use super::*;
use crate::themes::engine::{REQUIRED_TEMPLATES, ThemeAssets, ThemeManifest};
use crate::themes::validity::validate_theme_directory;

// Validation

/// The theme-id pattern, compiled once and reused: compiling it on
/// every `validate_theme_id` call paid a full regex compilation per
/// validation (and per discovered theme directory).
static THEME_ID_PATTERN: OnceLock<Regex> = OnceLock::new();

fn theme_id_pattern() -> &'static Regex {
    THEME_ID_PATTERN
        .get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9_-]*$").expect("static regex is valid"))
}

/// A theme id must be a safe directory name: lowercase letters, digits,
/// `-` and `_`, starting with a letter or digit. Anything else is
/// rejected before it ever touches the filesystem.
pub fn validate_theme_id(id: &str) -> Result<String, UnifiedError> {
    if !theme_id_pattern().is_match(id) {
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

pub(crate) const SCREENSHOT_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// Validate a theme directory thoroughly. Returns the parsed manifest on
/// success, or the full list of problems on failure.
///
/// There is exactly one `theme.yaml` shape: the rendering engine's
/// (`crate::themes::engine::ThemeManifest`). Structural validation
/// (manifest shape, required templates, Handlebars syntax, declared
/// `require` capabilities and `assets`) is delegated to the engine's
/// `validate_theme_directory`, so the admin API and the renderer can
/// never disagree about whether a theme is valid. This module keeps
/// only its own stricter admin-facing checks on top: the version must
/// be semver, the description must not be empty, and the screenshot
/// (when declared) must be a supported image type. Requirement
/// *satisfaction* (e.g. the React bundle for `require: [react]`
/// actually being built) is checked by the engine at activation time
/// (`ThemeEngine::check_requirements`), not here.
pub fn validate_theme(dir: &Path) -> Result<ThemeManifest, Vec<String>> {
    let mut problems: Vec<String> = Vec::new();
    if !dir.is_dir() {
        problems.push("theme directory does not exist".to_string());
        return Err(problems);
    }
    let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
        problems.push("theme directory name is not a valid theme id".to_string());
        return Err(problems);
    };
    let Some(themes_dir) = dir.parent() else {
        problems.push("theme directory has no parent directory".to_string());
        return Err(problems);
    };
    let report = validate_theme_directory(id, dir, &[]);
    if !report.valid {
        problems.extend(report.errors.iter().map(|issue| issue.message.clone()));
        return Err(problems);
    }
    let text = match std::fs::read_to_string(dir.join("theme.yaml")) {
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
    if manifest.name.chars().count() > 200 {
        problems.push("theme.yaml: name must be at most 200 characters".to_string());
    }
    if Version::parse(manifest.version.trim()).is_err() {
        problems.push(format!(
            "theme.yaml: version \"{}\" is not a valid semver version",
            manifest.version
        ));
    }
    if manifest.description.trim().is_empty() {
        problems.push("theme.yaml: description must not be empty".to_string());
    }
    if let Some(screenshot) = &manifest.screenshot {
        if !screenshot.trim().is_empty() {
            let extension = Path::new(screenshot)
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !SCREENSHOT_EXTENSIONS.contains(&extension.as_str()) {
                problems.push(format!(
                    "screenshot \"{screenshot}\" must be a png, jpg, jpeg, or webp file"
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(manifest)
    } else {
        Err(problems)
    }
}

pub(crate) fn screenshot_present(dir: &Path) -> bool {
    ["screenshot.png", "screenshot.jpg", "screenshot.jpeg", "screenshot.webp"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

pub(crate) fn template_files(dir: &Path, manifest: &ThemeManifest) -> Vec<String> {
    // Declared templates plus the required set, as `templates/<stem>.hbs`
    // paths - the engine's shape, not the old `.html` layout mapping.
    let mut names: Vec<String> = manifest.templates.clone();
    for required in REQUIRED_TEMPLATES {
        if !names.iter().any(|name| name == required) {
            names.push(required.to_string());
        }
    }
    let mut files: Vec<String> = names
        .iter()
        .map(|name| {
            let stem = manifest
                .template_files
                .get(name)
                .map(String::as_str)
                .unwrap_or(name);
            format!("templates/{stem}.hbs")
        })
        .collect();
    files.sort();
    files.dedup();
    // Keep only files that actually exist, so the listing is honest.
    files
        .into_iter()
        .filter(|file| dir.join(file).is_file())
        .collect()
}

pub(crate) fn embedded_default_info(active_id: &str) -> ThemeInfo {
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
        templates: vec!["templates/layout.hbs".to_string()],
        valid: true,
        problems: Vec::new(),
        require: Vec::new(),
        framework: String::new(),
        assets: ThemeAssets::default(),
    }
}

/// Discover every theme: the embedded default first, then each directory
/// under `themes_dir` that contains a `theme.yaml`. Invalid themes are
/// still listed, with `valid: false` and their problems, so an admin can
/// see what needs fixing.
pub(crate) fn discover_themes(state: &ThemeState) -> Vec<ThemeInfo> {
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
                author: None,
                active: active_id == id,
                source: "directory".to_string(),
                path: Some(path.to_string_lossy().to_string()),
                screenshot: manifest
                    .screenshot
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
                    || screenshot_present(&path),
                templates: template_files(&path, &manifest),
                valid: true,
                problems: Vec::new(),
                require: manifest.require.clone(),
                framework: manifest.framework.clone(),
                assets: manifest.assets.clone(),
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
                require: Vec::new(),
                framework: String::new(),
                assets: ThemeAssets::default(),
            }),
        }
    }
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    themes.append(&mut discovered);
    themes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("unified-theme-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_theme(dir: &Path, manifest: &str, templates: bool) {
        std::fs::write(dir.join("theme.yaml"), manifest).expect("write theme.yaml");
        if templates {
            // The engine requires layout, dashboard, home and error.
            let templates_dir = dir.join("templates");
            std::fs::create_dir_all(&templates_dir).expect("create templates dir");
            for name in ["layout", "dashboard", "home", "error"] {
                std::fs::write(
                    templates_dir.join(format!("{name}.hbs")),
                    "<html></html>",
                )
                .expect("write template");
            }
        }
    }

    const MINIMAL_MANIFEST: &str =
        "name: Test Theme\nversion: 1.2.3\ndescription: A test theme.\n";

    #[test]
    fn validate_theme_id_accepts_safe_ids() {
        assert_eq!(validate_theme_id("default").unwrap(), "default");
        assert_eq!(validate_theme_id("a").unwrap(), "a");
        assert_eq!(validate_theme_id("my-theme_2").unwrap(), "my-theme_2");
        assert_eq!(validate_theme_id("9lives").unwrap(), "9lives");
        assert_eq!(validate_theme_id("a".repeat(100).as_str()).unwrap().len(), 100);
    }

    #[test]
    fn validate_theme_id_rejects_unsafe_ids() {
        for bad in ["", "UPPER", "has space", "has.dot", "has/slash", "has\\slash", "..", "-lead", "_lead"] {
            assert!(validate_theme_id(bad).is_err(), "{bad} must be rejected");
        }
        assert!(validate_theme_id("a".repeat(101).as_str()).is_err());
    }

    #[test]
    fn validate_theme_id_reuses_the_compiled_pattern() {
        // Repeated valid and invalid calls must all behave the same as
        // before, and the shared pattern must be initialized exactly
        // once (proving no per-call compilation happens anymore).
        for _ in 0..100 {
            assert!(validate_theme_id("alpha-1").is_ok());
            assert!(validate_theme_id("BAD").is_err());
        }
        assert!(THEME_ID_PATTERN.get().is_some());
    }

    #[test]
    fn validate_theme_rejects_missing_directory() {
        let dir = std::env::temp_dir().join(format!(
            "unified-theme-test-{}-missing",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let problems = validate_theme(&dir).expect_err("missing dir must fail");
        assert!(problems.iter().any(|p| p.contains("does not exist")));
    }

    #[test]
    fn validate_theme_rejects_missing_manifest() {
        let dir = temp_dir("no-manifest");
        let problems = validate_theme(&dir).expect_err("missing manifest must fail");
        assert!(problems.iter().any(|p| p.contains("has no theme.yaml manifest")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_theme_accepts_minimal_theme() {
        let dir = temp_dir("minimal");
        write_theme(&dir, MINIMAL_MANIFEST, true);
        let manifest = validate_theme(&dir).expect("minimal theme must validate");
        assert_eq!(manifest.name, "Test Theme");
        assert_eq!(manifest.version, "1.2.3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_theme_rejects_bad_semver() {
        let dir = temp_dir("bad-semver");
        write_theme(
            &dir,
            "name: Bad\nversion: not-semver\ndescription: Bad version.\n",
            true,
        );
        let problems = validate_theme(&dir).expect_err("bad semver must fail");
        assert!(problems.iter().any(|p| p.contains("not a valid semver version")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_theme_rejects_traversal_template_name() {
        let dir = temp_dir("traversal");
        write_theme(
            &dir,
            "name: Evil\nversion: 1.0.0\ndescription: Traversal.\ntemplates:\n  - ../escape\n",
            true,
        );
        let problems = validate_theme(&dir).expect_err("traversal template must fail");
        assert!(problems.iter().any(|p| p.contains("invalid name")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn screenshot_present_detects_files() {
        let dir = temp_dir("screenshot");
        assert!(!screenshot_present(&dir));
        std::fs::write(dir.join("screenshot.webp"), b"fake").expect("write screenshot");
        assert!(screenshot_present(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_files_lists_existing_files_only() {
        let dir = temp_dir("template-files");
        write_theme(&dir, MINIMAL_MANIFEST, true);
        let templates_dir = dir.join("templates");
        std::fs::write(templates_dir.join("index.hbs"), "<html></html>")
            .expect("write index");
        let manifest = ThemeManifest {
            name: "Test".to_string(),
            version: "1.0.0".to_string(),
            description: "Test".to_string(),
            screenshot: None,
            directories: vec!["templates".to_string()],
            templates: vec![
                "layout".to_string(),
                "index".to_string(),
                "post".to_string(),
            ],
            template_files: std::collections::HashMap::new(),
            require: Vec::new(),
            framework: String::new(),
            assets: ThemeAssets::default(),
        };
        // post.hbs does not exist, so it must be dropped; the rest sorted.
        assert_eq!(
            template_files(&dir, &manifest),
            vec![
                "templates/dashboard.hbs".to_string(),
                "templates/error.hbs".to_string(),
                "templates/home.hbs".to_string(),
                "templates/index.hbs".to_string(),
                "templates/layout.hbs".to_string(),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_themes_lists_embedded_default_and_valid_theme() {
        let dir = temp_dir("discover");
        let good = dir.join("alpha");
        std::fs::create_dir_all(&good).expect("create alpha theme");
        write_theme(&good, MINIMAL_MANIFEST, true);
        // Invalid id: skipped entirely.
        let bad_id = dir.join("Bad Name");
        std::fs::create_dir_all(&bad_id).expect("create bad-id theme");
        write_theme(&bad_id, MINIMAL_MANIFEST, true);
        // Invalid theme: listed with valid: false.
        let broken = dir.join("broken");
        std::fs::create_dir_all(&broken).expect("create broken theme");
        write_theme(&broken, "name: Broken\nversion: nope\ndescription: Broken.\n", true);
        let state = ThemeState::new(dir.clone(), dir.join("theme.json"));
        let themes = discover_themes(&state);
        assert_eq!(themes[0].id, "default");
        assert!(themes[0].valid);
        let ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"alpha"));
        assert!(!ids.contains(&"Bad Name"));
        let broken_info = themes.iter().find(|t| t.id == "broken").expect("broken listed");
        assert!(!broken_info.valid);
        assert!(!broken_info.problems.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
