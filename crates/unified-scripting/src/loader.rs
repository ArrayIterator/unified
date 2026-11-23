//! Loading TypeScript modules from disk.
//!
//! Layout (one directory per module, under the modules directory
//! — `modules/` by default, configurable via
//! `scripting.modules_dir`):
//!
//! ```text
//! modules/
//!   hello-ts/
//!     module.ts        # required: the module source
//!     module.json      # optional manifest: name, version, description
//! ```
//!
//! The module id is the directory name; a manifest `id` that
//! differs from it is an error. Sources are size-capped
//! (`unified_ts::MAX_SCRIPT_SIZE`, 5 MiB) and transpiled in
//! memory by `unified-ts` (OXC) — no `.js` file is ever written.
//! A module that fails to transpile or evaluate is reported and
//! skipped; it never blocks the other modules or the server start.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use unified_core::error::{UnifiedError, UnifiedResult};

use crate::engine::{ModuleSummary, ScriptingEngine};

/// Metadata of one TypeScript module. The id is the directory
/// name; name/version/description come from the manifest or from
/// defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// Directory holding `module.ts` (+ `module.json`).
    #[serde(skip)]
    pub dir: PathBuf,
}

/// Optional manifest (`module.json`). Every field is optional;
/// unknown fields are ignored.
#[derive(Debug, Default, Deserialize)]
struct ModuleManifest {
    id: Option<String>,
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
}

/// One discovered module on disk: its metadata plus the path of
/// its TypeScript source.
#[derive(Debug, Clone)]
pub struct DiscoveredModule {
    pub meta: ModuleMeta,
    pub source_path: PathBuf,
}

/// Validate a module id: lowercase letters, digits, `_` and `-`,
/// starting with a letter or digit, at most 64 characters. The
/// same shape the native module ids use.
pub fn validate_module_id(id: &str) -> UnifiedResult<()> {
    if id.is_empty() || id.len() > 64 {
        return Err(UnifiedError::bad_request(
            "Module id must be 1..=64 characters",
        ));
    }
    if !id.chars().all(|char| {
        char.is_ascii_lowercase() || char.is_ascii_digit() || char == '_' || char == '-'
    }) {
        return Err(UnifiedError::bad_request(
            "Module id may only contain lowercase letters, digits, '_' and '-'",
        ));
    }
    Ok(())
}

/// Read one module directory: the manifest (when present) plus the
/// requirement that `module.ts` exists.
pub fn discover_one(dir: &Path) -> UnifiedResult<DiscoveredModule> {
    let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
        return Err(UnifiedError::bad_request(
            "Module directory has no usable name",
        ));
    };
    validate_module_id(id)?;
    let manifest_path = dir.join("module.json");
    let mut manifest = ModuleManifest::default();
    if manifest_path.is_file() {
        let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
            UnifiedError::bad_request(format!("Cannot read module.json for {id}: {error}"))
        })?;
        manifest = serde_json::from_str(&text).map_err(|error| {
            UnifiedError::bad_request(format!("Invalid module.json for {id}: {error}"))
        })?;
    }
    if let Some(manifest_id) = &manifest.id {
        if manifest_id != id {
            return Err(UnifiedError::bad_request(format!(
                "module.json id \"{manifest_id}\" does not match directory \"{id}\""
            )));
        }
    }
    let source_path = dir.join("module.ts");
    if !source_path.is_file() {
        return Err(UnifiedError::bad_request(format!(
            "Module {id} has no module.ts"
        )));
    }
    Ok(DiscoveredModule {
        meta: ModuleMeta {
            id: id.to_string(),
            name: manifest.name.unwrap_or_else(|| id.to_string()),
            version: manifest.version.unwrap_or_else(|| "0.1.0".to_string()),
            description: manifest.description.unwrap_or_default(),
            dir: dir.to_path_buf(),
        },
        source_path,
    })
}

/// Scan the modules directory for module directories, sorted by
/// id. Returns the discovered modules plus one `(id, error)` pair
/// per directory that could not be read as a module; a broken
/// directory never aborts the scan.
pub fn discover_modules(modules_dir: &Path) -> (Vec<DiscoveredModule>, Vec<(String, String)>) {
    let mut found: Vec<DiscoveredModule> = Vec::new();
    let mut errors: Vec<(String, String)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(modules_dir) else {
        return (found, errors);
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match discover_one(&dir) {
            Ok(module) => found.push(module),
            Err(error) => errors.push((name, error.to_string())),
        }
    }
    (found, errors)
}

/// Read and transpile one module's TypeScript source. The size cap
/// is the same one the dashboard transpiler applies.
pub fn load_and_transpile(source_path: &Path) -> UnifiedResult<String> {
    let metadata = std::fs::metadata(source_path).map_err(|error| {
        UnifiedError::bad_request(format!("Cannot read module source: {error}"))
    })?;
    if metadata.len() > unified_ts::MAX_SCRIPT_SIZE {
        return Err(UnifiedError::bad_request(format!(
            "Module source exceeds the {}-byte limit",
            unified_ts::MAX_SCRIPT_SIZE
        )));
    }
    let source = std::fs::read_to_string(source_path).map_err(|error| {
        UnifiedError::bad_request(format!("Cannot read module source: {error}"))
    })?;
    let filename = source_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "module.ts".to_string());
    unified_ts::transpile_typescript(&source, &filename).map_err(crate::error::transpile_error)
}

/// Outcome of loading one module at startup (or reload).
#[derive(Debug, Clone, Serialize)]
pub struct LoadReport {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ModuleSummary>,
}

/// Load every discovered module in `modules_dir` into the engine.
/// A module that fails (bad manifest, transpile error, evaluation
/// error) is reported in the returned list and skipped; the rest
/// load normally.
pub fn load_all(engine: &ScriptingEngine, modules_dir: &Path) -> Vec<LoadReport> {
    let (discovered, scan_errors) = discover_modules(modules_dir);
    let mut reports: Vec<LoadReport> = scan_errors
        .into_iter()
        .map(|(id, error)| LoadReport {
            id,
            ok: false,
            error: Some(error),
            summary: None,
        })
        .collect();
    for module in discovered {
        let id = module.meta.id.clone();
        let report = match load_and_transpile(&module.source_path) {
            Ok(js) => match engine.load_module(module.meta, js) {
                Ok(summary) => LoadReport {
                    id,
                    ok: true,
                    error: None,
                    summary: Some(summary),
                },
                Err(error) => LoadReport {
                    id,
                    ok: false,
                    error: Some(error),
                    summary: None,
                },
            },
            Err(error) => LoadReport {
                id,
                ok: false,
                error: Some(error.to_string()),
                summary: None,
            },
        };
        reports.push(report);
    }
    reports.sort_by(|a, b| a.id.cmp(&b.id));
    reports
}

/// Re-read one module from disk and reload it into the engine
/// (fresh context, same Rust wiring).
pub fn reload_one(
    engine: &ScriptingEngine,
    modules_dir: &Path,
    id: &str,
) -> Result<ModuleSummary, String> {
    validate_module_id(id).map_err(|error| error.to_string())?;
    let dir = modules_dir.join(id);
    let discovered = discover_one(&dir).map_err(|error| error.to_string())?;
    let js = load_and_transpile(&discovered.source_path).map_err(|error| error.to_string())?;
    engine.reload_module(id, js)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unified-scripting-loader-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn discovers_modules_and_reports_broken_ones() {
        let root = temp_dir("discover");
        let good = root.join("good-mod");
        fs::create_dir_all(&good).expect("mkdir");
        fs::write(
            good.join("module.json"),
            r#"{"name": "Good", "version": "1.2.3", "description": "d"}"#,
        )
        .expect("write manifest");
        fs::write(good.join("module.ts"), "Unified.log.info('hi');").expect("write source");
        let no_source = root.join("empty-mod");
        fs::create_dir_all(&no_source).expect("mkdir");
        let bad_id = root.join("Bad_ID");
        fs::create_dir_all(&bad_id).expect("mkdir");
        fs::write(bad_id.join("module.ts"), "").expect("write source");
        let (found, errors) = discover_modules(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].meta.id, "good-mod");
        assert_eq!(found[0].meta.version, "1.2.3");
        assert_eq!(errors.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_id_mismatch_is_an_error() {
        let root = temp_dir("mismatch");
        let dir = root.join("real-id");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("module.json"), r#"{"id": "other-id"}"#).expect("write");
        fs::write(dir.join("module.ts"), "").expect("write");
        assert!(discover_one(&dir).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn transpile_rejects_bad_typescript() {
        let root = temp_dir("transpile");
        let bad = root.join("bad.ts");
        fs::write(&bad, "const x: = 1;").expect("write");
        assert!(load_and_transpile(&bad).is_err());
        let good = root.join("good.ts");
        fs::write(&good, "interface A { x: string }\nconst a: A = { x: 'y' };").expect("write");
        let js = load_and_transpile(&good).expect("transpile");
        assert!(!js.contains("interface"));
        let _ = fs::remove_dir_all(&root);
    }
}
