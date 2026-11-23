// Theme validity validation: a structured, collect-all-issues report
// for one theme, instead of the loader's first-error bail.
//
// `validate_theme_directory` checks everything it can — theme id,
// manifest presence and shape, required files, every declared template
// file, and the Handlebars syntax of every `.hbs` file the theme ships
// (templates, partials, and per-module overrides) — and returns all
// problems at once, so an admin UI can list every broken thing about a
// theme without attempting activation. Errors make the theme invalid
// and un-activatable; warnings (missing screenshot, unknown manifest
// keys, undeclared template files) do not.
//
// `ThemeEngine::validate_theme` (in `engine.rs`) is the entry point
// callers use: it handles the embedded default theme and adds the
// registered module templates to the report. `set_active_theme`
// refuses to activate a theme whose report is invalid.

use crate::themes::engine::{MAX_TEMPLATE_SIZE, REQUIRED_TEMPLATES, ThemeManifest, validate_theme_id};
use handlebars::Handlebars;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Manifest keys the loader understands. Any other top-level key in
/// `theme.yaml` is reported as a warning, not an error: it may be a
/// typo, or a key a newer loader version would understand.
const KNOWN_MANIFEST_KEYS: &[&str] = &[
    "name",
    "version",
    "description",
    "screenshot",
    "directories",
    "templates",
    "template_files",
];

/// One problem found while validating a theme.
///
/// `code` is machine-readable (see the `validate_theme_directory`
/// docs for the full list); `message` is human-readable; `path` names
/// the file the issue concerns, relative to the theme directory when
/// known (for example `theme.yaml` or `templates/home.hbs`), or the
/// module template key (`module:billing/invoice`) for registered
/// module templates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeIssue {
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

impl ThemeIssue {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// Structured validity report for one theme.
///
/// `valid` is true exactly when `errors` is empty. Warnings never
/// make a theme invalid. `templates_checked` counts the theme's own
/// `.hbs` files that were syntax-checked; `module_templates_checked`
/// counts registered module templates that were re-checked (they are
/// compiled at registration time already, so this is a second,
/// defensive pass — see `check_module_sources`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeValidationReport {
    pub id: String,
    pub valid: bool,
    pub errors: Vec<ThemeIssue>,
    pub warnings: Vec<ThemeIssue>,
    pub templates_checked: usize,
    pub module_templates_checked: usize,
}

impl ThemeValidationReport {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
            templates_checked: 0,
            module_templates_checked: 0,
        }
    }

    pub fn add_error(&mut self, issue: ThemeIssue) {
        self.errors.push(issue);
        self.valid = false;
    }

    pub fn add_warning(&mut self, issue: ThemeIssue) {
        self.warnings.push(issue);
    }

    /// True when the report has an error with this code.
    pub fn has_error(&self, code: &str) -> bool {
        self.errors.iter().any(|issue| issue.code == code)
    }

    /// True when the report has a warning with this code.
    pub fn has_warning(&self, code: &str) -> bool {
        self.warnings.iter().any(|issue| issue.code == code)
    }

    /// Machine-readable codes of all errors, in report order.
    pub fn error_codes(&self) -> Vec<String> {
        self.errors.iter().map(|issue| issue.code.clone()).collect()
    }

    /// Machine-readable codes of all warnings, in report order.
    pub fn warning_codes(&self) -> Vec<String> {
        self.warnings
            .iter()
            .map(|issue| issue.code.clone())
            .collect()
    }

    /// One-line summary of all errors (`"code: message; code: message"`),
    /// for error responses that cannot carry the full report. Empty
    /// when the report has no errors.
    pub fn error_summary(&self) -> String {
        self.errors
            .iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Re-compile every registered module template source (`(key,
    /// source)` pairs, keyed `module:<module>/<name>`) and record any
    /// syntax failure as an error. Module templates are already
    /// compiled at registration time, so a failure here means the
    /// stored source changed out from under the registry; the check
    /// exists so the validity report still covers them.
    pub fn check_module_sources(&mut self, sources: &[(String, String)]) {
        for (key, source) in sources {
            self.module_templates_checked += 1;
            if let Some(issue) = check_template_syntax(key, source, key) {
                self.add_error(issue);
            }
        }
    }
}

/// Compile-check one Handlebars source. Returns `None` when the source
/// parses, or the `invalid-template-syntax` issue when it does not.
/// `display` names the template in the message; `path` is recorded on
/// the issue as the file (or module template key) it concerns.
pub fn check_template_syntax(display: &str, source: &str, path: &str) -> Option<ThemeIssue> {
    let mut handlebars = Handlebars::new();
    match handlebars.register_template_string(display, source) {
        Ok(()) => None,
        Err(error) => Some(
            ThemeIssue::new(
                "invalid-template-syntax",
                format!("Template \"{display}\" has invalid Handlebars syntax: {error}"),
            )
            .with_path(path),
        ),
    }
}

/// Validate theme sources that are already in memory (the embedded
/// default theme): syntax-check every source and require the
/// `required` templates to be present. `has_screenshot` decides
/// whether a `missing-screenshot` warning is added. Module templates
/// are not checked here; the caller adds them with
/// `ThemeValidationReport::check_module_sources`.
pub fn report_for_sources(
    id: &str,
    sources: &[(String, String)],
    required: &[&str],
    has_screenshot: bool,
) -> ThemeValidationReport {
    let mut report = ThemeValidationReport::new(id);
    for (name, source) in sources {
        report.templates_checked += 1;
        if let Some(issue) = check_template_syntax(name, source, &format!("{name}.hbs")) {
            report.add_error(issue);
        }
    }
    for name in required {
        if !sources.iter().any(|(item, _)| item == name) {
            report.add_error(
                ThemeIssue::new(
                    "missing-template",
                    format!("Theme \"{id}\" is missing the required template \"{name}\""),
                )
                .with_path(format!("templates/{name}.hbs")),
            );
        }
    }
    if !has_screenshot {
        report.add_warning(
            ThemeIssue::new(
                "missing-screenshot",
                format!("Theme \"{id}\" ships no screenshot"),
            )
            .with_path("screenshot.png"),
        );
    }
    report
}

/// Validate a disk theme at `dir` (`themes/<id>/`) and collect every
/// issue instead of stopping at the first one. `module_sources` are
/// the engine's registered module templates, re-checked defensively
/// (see `ThemeValidationReport::check_module_sources`).
///
/// Error codes used:
///
/// * `invalid-theme-id` — the id fails `validate_theme_id`.
/// * `missing-theme-dir` — `themes/<id>/` does not exist.
/// * `missing-manifest` — no `theme.yaml` in the theme directory.
/// * `unreadable-manifest` — `theme.yaml` cannot be read.
/// * `invalid-yaml` — `theme.yaml` is not valid YAML, is not a YAML
///   mapping, or has invalid field types.
/// * `invalid-manifest-field` — the manifest has no (or an empty)
///   `name` or `version`.
/// * `missing-layout` — `templates/layout.hbs` does not exist.
/// * `missing-template` — a manifest-declared (or required) template
///   file, or a `template_files` mapping target, does not exist.
/// * `invalid-template-name` — a declared template name or a
///   `template_files` stem breaks the template naming rules.
/// * `template-too-large` — a `.hbs` file exceeds `MAX_TEMPLATE_SIZE`.
/// * `unreadable-template` — a `.hbs` file cannot be read.
/// * `invalid-template-syntax` — a `.hbs` file (or a registered module
///   template source) fails to parse as Handlebars.
///
/// Warning codes used (never invalidating):
///
/// * `missing-screenshot` — no screenshot file ships with the theme.
/// * `unknown-manifest-field` — a top-level `theme.yaml` key the
///   loader does not know.
/// * `undeclared-template` — a `.hbs` file directly under
///   `templates/` that the manifest does not declare, so the loader
///   ignores it. (Files in subdirectories are per-module overrides
///   and are never warned about.)
pub fn validate_theme_directory(
    id: &str,
    dir: &Path,
    module_sources: &[(String, String)],
) -> ThemeValidationReport {
    let mut report = ThemeValidationReport::new(id);
    if let Err(error) = validate_theme_id(id) {
        report.add_error(ThemeIssue::new("invalid-theme-id", error.message).with_path(id));
        return report;
    }
    if !dir.is_dir() {
        report.add_error(
            ThemeIssue::new(
                "missing-theme-dir",
                format!("Theme \"{id}\" directory does not exist"),
            )
            .with_path(dir.to_string_lossy()),
        );
        return report;
    }

    let manifest_path = dir.join("theme.yaml");
    let mut manifest: Option<ThemeManifest> = None;
    let mut template_files: HashMap<String, String> = HashMap::new();
    // Declared templates: the manifest's list, or the required set
    // when the manifest declares none (same fallback as the loader).
    let mut declared: HashSet<String> =
        REQUIRED_TEMPLATES.iter().map(|name| name.to_string()).collect();
    if !manifest_path.is_file() {
        report.add_error(
            ThemeIssue::new(
                "missing-manifest",
                format!("Theme \"{id}\" has no theme.yaml manifest"),
            )
            .with_path("theme.yaml"),
        );
    } else {
        match std::fs::read_to_string(&manifest_path) {
            Err(error) => {
                report.add_error(
                    ThemeIssue::new(
                        "unreadable-manifest",
                        format!("Cannot read theme manifest for \"{id}\": {error}"),
                    )
                    .with_path("theme.yaml"),
                );
            }
            Ok(text) => {
                match serde_yaml::from_str::<serde_yaml::Value>(&text) {
                    Err(error) => {
                        report.add_error(
                            ThemeIssue::new(
                                "invalid-yaml",
                                format!("Theme \"{id}\" has an invalid theme.yaml: {error}"),
                            )
                            .with_path("theme.yaml"),
                        );
                    }
                    Ok(value) => {
                        let Some(mapping) = value.as_mapping() else {
                            report.add_error(
                                ThemeIssue::new(
                                    "invalid-yaml",
                                    format!("Theme \"{id}\" theme.yaml must contain a YAML mapping"),
                                )
                                .with_path("theme.yaml"),
                            );
                            return finish_report(
                                report,
                                dir,
                                &declared,
                                &template_files,
                                &None,
                                module_sources,
                            );
                        };
                        for (key, _) in mapping {
                            if let Some(key) = key.as_str() {
                                if !KNOWN_MANIFEST_KEYS.contains(&key) {
                                    report.add_warning(
                                        ThemeIssue::new(
                                            "unknown-manifest-field",
                                            format!(
                                                "Theme \"{id}\" theme.yaml has unknown field \"{key}\""
                                            ),
                                        )
                                        .with_path("theme.yaml"),
                                    );
                                }
                            }
                        }
                        let name_ok = mapping
                            .get("name")
                            .and_then(|value| value.as_str())
                            .is_some_and(|name| !name.trim().is_empty());
                        let version_ok = mapping
                            .get("version")
                            .and_then(|value| value.as_str())
                            .is_some_and(|version| !version.trim().is_empty());
                        if !name_ok {
                            report.add_error(
                                ThemeIssue::new(
                                    "invalid-manifest-field",
                                    format!("Theme \"{id}\" has an empty or missing name in theme.yaml"),
                                )
                                .with_path("theme.yaml"),
                            );
                        }
                        if !version_ok {
                            report.add_error(
                                ThemeIssue::new(
                                    "invalid-manifest-field",
                                    format!("Theme \"{id}\" has an empty or missing version in theme.yaml"),
                                )
                                .with_path("theme.yaml"),
                            );
                        }
                        // Only parse the typed manifest when the
                        // required fields are present: otherwise the
                        // deserializer would report the same missing
                        // fields a second time as `invalid-yaml`.
                        if name_ok && version_ok {
                            match serde_yaml::from_str::<ThemeManifest>(&text) {
                                Ok(parsed) => {
                                    if !parsed.templates.is_empty() {
                                        declared =
                                            parsed.templates.iter().cloned().collect();
                                        for name in REQUIRED_TEMPLATES {
                                            declared.insert(name.to_string());
                                        }
                                    }
                                    template_files = parsed.template_files.clone();
                                    manifest = Some(parsed);
                                }
                                Err(error) => {
                                    report.add_error(
                                        ThemeIssue::new(
                                            "invalid-yaml",
                                            format!(
                                                "Theme \"{id}\" theme.yaml has invalid field types: {error}"
                                            ),
                                        )
                                        .with_path("theme.yaml"),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    finish_report(
        report,
        dir,
        &declared,
        &template_files,
        &manifest,
        module_sources,
    )
}

/// File checks shared by `validate_theme_directory`: declared template
/// files, every `.hbs` file under `templates/` and `partials/`,
/// screenshot presence, and the registered module templates. Split
/// out so the early `invalid-yaml` return above still runs them.
fn finish_report(
    mut report: ThemeValidationReport,
    dir: &Path,
    declared: &HashSet<String>,
    template_files: &HashMap<String, String>,
    manifest: &Option<ThemeManifest>,
    module_sources: &[(String, String)],
) -> ThemeValidationReport {
    let id = report.id.clone();
    let templates_dir = dir.join("templates");

    if !templates_dir.join("layout.hbs").is_file() {
        report.add_error(
            ThemeIssue::new(
                "missing-layout",
                format!("Theme \"{id}\" is missing the required layout template (templates/layout.hbs)"),
            )
            .with_path("templates/layout.hbs"),
        );
    }

    // Every declared template (plus the required set) must name a
    // valid file that exists.
    for name in declared {
        if crate::themes::module_templates::validate_module_template_name(name).is_err() {
            report.add_error(
                ThemeIssue::new(
                    "invalid-template-name",
                    format!("Theme \"{id}\" declares template \"{name}\" with an invalid name"),
                )
                .with_path("theme.yaml"),
            );
            continue;
        }
        let stem = template_files.get(name).map(String::as_str).unwrap_or(name);
        if crate::themes::module_templates::validate_module_template_name(stem).is_err() {
            report.add_error(
                ThemeIssue::new(
                    "invalid-template-name",
                    format!("Theme \"{id}\" maps template \"{name}\" to invalid file stem \"{stem}\""),
                )
                .with_path("theme.yaml"),
            );
            continue;
        }
        if !templates_dir.join(format!("{stem}.hbs")).is_file() {
            report.add_error(
                ThemeIssue::new(
                    "missing-template",
                    format!("Theme \"{id}\" declares template \"{name}\" but templates/{stem}.hbs is missing"),
                )
                .with_path(format!("templates/{stem}.hbs")),
            );
        }
    }
    // `template_files` targets for names outside the declared set.
    for (name, stem) in template_files {
        if declared.contains(name) {
            continue;
        }
        if templates_dir.join(format!("{stem}.hbs")).is_file() {
            continue;
        }
        report.add_error(
            ThemeIssue::new(
                "missing-template",
                format!("Theme \"{id}\" maps template \"{name}\" to missing file templates/{stem}.hbs"),
            )
            .with_path(format!("templates/{stem}.hbs")),
        );
    }

    // Syntax-check every `.hbs` file the theme ships, including
    // per-module overrides in subdirectories of `templates/`.
    let mut hbs_files = Vec::new();
    collect_hbs_files(&templates_dir, &mut hbs_files);
    hbs_files.sort();
    for path in hbs_files {
        let relative = path
            .strip_prefix(&templates_dir)
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.len() > MAX_TEMPLATE_SIZE {
                report.add_error(
                    ThemeIssue::new(
                        "template-too-large",
                        format!(
                            "Theme \"{id}\" template \"{relative}\" is too large: {} bytes (maximum {MAX_TEMPLATE_SIZE})",
                            metadata.len()
                        ),
                    )
                    .with_path(format!("templates/{relative}")),
                );
                continue;
            }
        }
        match std::fs::read_to_string(&path) {
            Err(error) => {
                report.add_error(
                    ThemeIssue::new(
                        "unreadable-template",
                        format!("Cannot read template \"{relative}\" of theme \"{id}\": {error}"),
                    )
                    .with_path(format!("templates/{relative}")),
                );
            }
            Ok(source) => {
                report.templates_checked += 1;
                if let Some(issue) =
                    check_template_syntax(&relative, &source, &format!("templates/{relative}"))
                {
                    report.add_error(issue);
                }
            }
        }
        // A top-level `.hbs` file the manifest never declares is
        // ignored by the loader; warn instead of failing. Files in
        // subdirectories are per-module overrides, not stray files.
        if Path::new(&relative).components().count() == 1 {
            if let Some(stem) = Path::new(&relative).file_stem().and_then(|stem| stem.to_str()) {
                if !declared.contains(stem) {
                    report.add_warning(
                        ThemeIssue::new(
                            "undeclared-template",
                            format!(
                                "Theme \"{id}\" ships templates/{relative} but does not declare it in theme.yaml, so the loader ignores it"
                            ),
                        )
                        .with_path(format!("templates/{relative}")),
                    );
                }
            }
        }
    }

    // Partials live in `partials/` next to `templates/` for disk
    // themes (the loader reads that directory, not `templates/`).
    let partials_dir = dir.join("partials");
    if partials_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&partials_dir) {
            let mut partials: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file() && path.extension().is_some_and(|ext| ext == "hbs")
                })
                .collect();
            partials.sort();
            for path in partials {
                let file_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Ok(metadata) = std::fs::metadata(&path) {
                    if metadata.len() > MAX_TEMPLATE_SIZE {
                        report.add_error(
                            ThemeIssue::new(
                                "template-too-large",
                                format!(
                                    "Theme \"{id}\" partial \"{file_name}\" is too large: {} bytes (maximum {MAX_TEMPLATE_SIZE})",
                                    metadata.len()
                                ),
                            )
                            .with_path(format!("partials/{file_name}")),
                        );
                        continue;
                    }
                }
                match std::fs::read_to_string(&path) {
                    Err(error) => {
                        report.add_error(
                            ThemeIssue::new(
                                "unreadable-template",
                                format!("Cannot read partial \"{file_name}\" of theme \"{id}\": {error}"),
                            )
                            .with_path(format!("partials/{file_name}")),
                        );
                    }
                    Ok(source) => {
                        report.templates_checked += 1;
                        if let Some(issue) = check_template_syntax(
                            &file_name,
                            &source,
                            &format!("partials/{file_name}"),
                        ) {
                            report.add_error(issue);
                        }
                    }
                }
            }
        }
    }

    // Screenshot: the manifest names one, or `screenshot.png` is the
    // convention. Absence is a warning only.
    let screenshot_present = match manifest
        .as_ref()
        .and_then(|manifest| manifest.screenshot.as_deref())
    {
        Some(name) if !name.trim().is_empty() => Path::new(name)
            .file_name()
            .map(|safe| dir.join(safe).is_file())
            .unwrap_or(false),
        _ => dir.join("screenshot.png").is_file(),
    };
    if !screenshot_present {
        report.add_warning(
            ThemeIssue::new(
                "missing-screenshot",
                format!("Theme \"{id}\" ships no screenshot"),
            )
            .with_path("screenshot.png"),
        );
    }

    report.check_module_sources(module_sources);
    report
}

/// Collect every `.hbs` file under `dir`, recursively, into `out`.
fn collect_hbs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_hbs_files(&path, out);
        } else if path.is_file() && path.extension().is_some_and(|ext| ext == "hbs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes::engine::ThemeEngine;
    use std::path::{Path, PathBuf};

    const VALID_MANIFEST: &str = "name: Test\nversion: 1.2.3\ndescription: Test theme\ntemplates:\n  - layout\n  - dashboard\n  - home\n  - error\n";
    const GOOD_HBS: &str = "<h1>{{title}}</h1>";

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("unified-theme-validity-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// Write a theme under `base/<id>/` and return the theme dir.
    /// `files` are `(relative path, content)` pairs; `theme.yaml` is
    /// written from `manifest` (pass `None` to omit it).
    fn write_theme(
        base: &Path,
        id: &str,
        manifest: Option<&str>,
        files: &[(&str, &str)],
    ) -> PathBuf {
        let theme = base.join(id);
        std::fs::create_dir_all(theme.join("templates")).unwrap();
        if let Some(manifest) = manifest {
            std::fs::write(theme.join("theme.yaml"), manifest).unwrap();
        }
        for (relative, content) in files {
            let path = theme.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
        theme
    }

    fn valid_files() -> Vec<(&'static str, &'static str)> {
        vec![
            ("templates/layout.hbs", GOOD_HBS),
            ("templates/dashboard.hbs", GOOD_HBS),
            ("templates/home.hbs", GOOD_HBS),
            ("templates/error.hbs", GOOD_HBS),
            ("screenshot.png", "fake-png"),
        ]
    }

    #[test]
    fn valid_disk_theme_is_valid() {
        let base = test_dir("valid");
        let theme = write_theme(&base, "good", Some(VALID_MANIFEST), &valid_files());
        let report = validate_theme_directory("good", &theme, &[]);
        assert!(report.valid, "errors: {:?}", report.errors);
        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());
        assert_eq!(report.templates_checked, 4);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_theme_dir_reports_error() {
        let base = test_dir("missing-dir");
        let report = validate_theme_directory("ghost", &base.join("ghost"), &[]);
        assert!(!report.valid);
        assert!(report.has_error("missing-theme-dir"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_manifest_reports_error() {
        let base = test_dir("missing-manifest");
        let theme = write_theme(&base, "bare", None, &valid_files());
        let report = validate_theme_directory("bare", &theme, &[]);
        assert!(!report.valid);
        assert!(report.has_error("missing-manifest"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn malformed_yaml_reports_invalid_yaml() {
        let base = test_dir("bad-yaml");
        let theme = write_theme(
            &base,
            "badyaml",
            Some("name: [unclosed\nversion: 1.0\n"),
            &valid_files(),
        );
        let report = validate_theme_directory("badyaml", &theme, &[]);
        assert!(!report.valid);
        assert!(report.has_error("invalid-yaml"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_name_and_version_report_invalid_manifest_field() {
        let base = test_dir("empty-fields");
        let theme = write_theme(
            &base,
            "empty",
            Some("name: \"\"\nversion: \"\"\n"),
            &valid_files(),
        );
        let report = validate_theme_directory("empty", &theme, &[]);
        assert!(!report.valid);
        assert_eq!(
            report
                .errors
                .iter()
                .filter(|issue| issue.code == "invalid-manifest-field")
                .count(),
            2
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_layout_reports_error() {
        let base = test_dir("missing-layout");
        let files: Vec<(&str, &str)> = valid_files()
            .into_iter()
            .filter(|(path, _)| *path != "templates/layout.hbs")
            .collect();
        let theme = write_theme(&base, "nolayout", Some(VALID_MANIFEST), &files);
        let report = validate_theme_directory("nolayout", &theme, &[]);
        assert!(!report.valid);
        assert!(report.has_error("missing-layout"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn declared_but_missing_template_reports_error() {
        let base = test_dir("missing-declared");
        let manifest = "name: Test\nversion: 1.0.0\ntemplates:\n  - layout\n  - custom\n";
        let theme = write_theme(&base, "missingtpl", Some(manifest), &valid_files());
        let report = validate_theme_directory("missingtpl", &theme, &[]);
        assert!(!report.valid);
        assert!(report.has_error("missing-template"));
        assert!(
            report
                .errors
                .iter()
                .any(|issue| issue.message.contains("custom")),
            "errors: {:?}",
            report.errors
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn invalid_handlebars_syntax_reports_error_and_collects_all_issues() {
        let base = test_dir("bad-syntax");
        let mut files = valid_files();
        files.push(("templates/home.hbs", "{{#if x}}unclosed"));
        // Drop the layout too: the report must list both problems.
        files.retain(|(path, _)| *path != "templates/layout.hbs");
        let theme = write_theme(&base, "badsyntax", Some(VALID_MANIFEST), &files);
        let report = validate_theme_directory("badsyntax", &theme, &[]);
        assert!(!report.valid);
        assert!(report.has_error("invalid-template-syntax"));
        assert!(report.has_error("missing-layout"));
        assert!(report.errors.len() >= 2, "errors: {:?}", report.errors);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unknown_manifest_key_is_warning_only() {
        let base = test_dir("unknown-key");
        let manifest = "name: Test\nversion: 1.0.0\ncustom_field: true\ntemplates:\n  - layout\n  - dashboard\n  - home\n  - error\n";
        let theme = write_theme(&base, "unknownkey", Some(manifest), &valid_files());
        let report = validate_theme_directory("unknownkey", &theme, &[]);
        assert!(report.valid, "errors: {:?}", report.errors);
        assert!(report.has_warning("unknown-manifest-field"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_screenshot_is_warning_only() {
        let base = test_dir("no-screenshot");
        let files: Vec<(&str, &str)> = valid_files()
            .into_iter()
            .filter(|(path, _)| *path != "screenshot.png")
            .collect();
        let theme = write_theme(&base, "noshot", Some(VALID_MANIFEST), &files);
        let report = validate_theme_directory("noshot", &theme, &[]);
        assert!(report.valid, "errors: {:?}", report.errors);
        assert!(report.has_warning("missing-screenshot"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn undeclared_template_file_is_warning_only() {
        let base = test_dir("undeclared");
        let mut files = valid_files();
        files.push(("templates/extra.hbs", GOOD_HBS));
        let theme = write_theme(&base, "undeclared", Some(VALID_MANIFEST), &files);
        let report = validate_theme_directory("undeclared", &theme, &[]);
        assert!(report.valid, "errors: {:?}", report.errors);
        assert!(report.has_warning("undeclared-template"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn invalid_theme_id_reports_error() {
        let base = test_dir("bad-id");
        let report = validate_theme_directory("../evil", &base, &[]);
        assert!(!report.valid);
        assert!(report.has_error("invalid-theme-id"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn module_template_sources_are_checked() {
        let base = test_dir("module-check");
        let theme = write_theme(&base, "modcheck", Some(VALID_MANIFEST), &valid_files());
        let good = vec![("module:billing/invoice".to_string(), "<p>{{title}}</p>".to_string())];
        let report = validate_theme_directory("modcheck", &theme, &good);
        assert!(report.valid, "errors: {:?}", report.errors);
        assert_eq!(report.module_templates_checked, 1);

        let bad = vec![("module:billing/invoice".to_string(), "{{#if}}".to_string())];
        let report = validate_theme_directory("modcheck", &theme, &bad);
        assert!(!report.valid);
        assert!(report.has_error("invalid-template-syntax"));
        assert_eq!(report.module_templates_checked, 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn oversized_template_reports_error() {
        let base = test_dir("oversized");
        let theme = base.join("big");
        std::fs::create_dir_all(theme.join("templates")).unwrap();
        std::fs::write(theme.join("theme.yaml"), VALID_MANIFEST).unwrap();
        let big = vec![b'x'; MAX_TEMPLATE_SIZE as usize + 1];
        std::fs::write(theme.join("templates/layout.hbs"), big).unwrap();
        for name in ["dashboard", "home", "error"] {
            std::fs::write(theme.join(format!("templates/{name}.hbs")), GOOD_HBS).unwrap();
        }
        std::fs::write(theme.join("screenshot.png"), "fake-png").unwrap();
        let report = validate_theme_directory("big", &theme, &[]);
        assert!(!report.valid);
        assert!(report.has_error("template-too-large"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_validates_embedded_default_theme() {
        let base = test_dir("engine-default");
        let engine = ThemeEngine::new(base.clone());
        let report = engine.validate_theme("default");
        assert!(report.valid, "errors: {:?}", report.errors);
        assert!(report.errors.is_empty());
        // The embedded default ships no screenshot, so that warning
        // is expected — and it must not invalidate the theme.
        assert!(report.has_warning("missing-screenshot"));
        assert!(report.templates_checked > 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_validate_theme_on_missing_dir() {
        let base = test_dir("engine-missing");
        let engine = ThemeEngine::new(base.clone());
        let report = engine.validate_theme("ghost");
        assert!(!report.valid);
        assert!(report.has_error("missing-theme-dir"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn engine_set_active_theme_refuses_invalid_theme() {
        let base = test_dir("engine-refuse");
        // Broken theme: valid manifest, but no layout template.
        let files: Vec<(&str, &str)> = valid_files()
            .into_iter()
            .filter(|(path, _)| *path != "templates/layout.hbs")
            .collect();
        write_theme(&base, "broken", Some(VALID_MANIFEST), &files);
        write_theme(&base, "good", Some(VALID_MANIFEST), &valid_files());
        let engine = ThemeEngine::new(base.clone());

        let error = engine.set_active_theme("broken").unwrap_err();
        assert_eq!(error.code, 400);
        assert!(
            error.message.contains("missing-layout"),
            "message: {}",
            error.message
        );
        assert_eq!(engine.active_theme(), "default");

        engine.set_active_theme("good").unwrap();
        assert_eq!(engine.active_theme(), "good");
        let _ = std::fs::remove_dir_all(&base);
    }
}
