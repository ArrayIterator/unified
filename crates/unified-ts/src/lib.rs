// TypeScript-to-JavaScript transpiler for the Unified dashboard frontend.
//
// Transpile-only: OXC parses the TypeScript source, strips and
// transforms the TypeScript syntax, and emits plain JavaScript. No
// JavaScript engine is involved anywhere — the server never executes
// the scripts; the browser runs the emitted JavaScript. That is why
// this crate is small and has no runtime cost beyond startup.
//
// Transpiler choice (checked 2026-11-22): OXC 0.159.0 over SWC.
// OXC ships its parser, transformer, and code generator as small
// lockstep-versioned crates with a plain API (`Parser` ->
// `SemanticBuilder` -> `Transformer` -> `Codegen`); SWC's equivalent set is larger, its
// major versions churn often (swc_ecma_parser is at 48.x), and its
// compile time is longer. Both are pure-Rust transpilers, not
// engines. A V8/deno/boa/quickjs embedding was deliberately not
// chosen: the server only transpiles, and an embedded engine would
// also break the musl static build.
//
// The server calls `compile_scripts_dir` once at startup with the
// `assets/js/` directory; the returned map (public path ->
// JavaScript source) is held in RAM and served from memory, so no
// `.js` files exist in the repository. `tsc --noEmit` (package.json
// `check` script) is the dev-time type check — the transpiler itself
// never type-checks.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{ESTarget, TransformOptions, Transformer};

/// Maximum size of one TypeScript source file (5 MiB, the same cap
/// the theme engine applies to templates): a larger file is rejected
/// so a broken or hostile file cannot make the server read unbounded
/// data into memory.
pub const MAX_SCRIPT_SIZE: u64 = 5 * 1024 * 1024;

/// Error from transpiling one TypeScript source.
#[derive(Debug)]
pub struct TranspileError {
    /// File the error came from (source name passed by the caller).
    pub file: String,
    /// Human-readable diagnostics, one per line.
    pub message: String,
}

impl fmt::Display for TranspileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Failed to transpile TypeScript \"{}\": {}",
            self.file, self.message
        )
    }
}

impl std::error::Error for TranspileError {}

fn failure(file: &str, message: String) -> TranspileError {
    TranspileError {
        file: file.to_string(),
        message,
    }
}

/// Transpile one TypeScript source to plain JavaScript.
///
/// `source` is the TypeScript text; `filename` is only used for
/// error messages and for the source type, the file is never read
/// from disk here. Type annotations, interfaces, and other
/// TypeScript-only syntax are removed; the emitted JavaScript keeps
/// the original runtime behaviour. This never type-checks: a type
/// error in the source still transpiles (that is `tsc --noEmit`'s
/// job, run at dev time).
pub fn transpile_typescript(source: &str, filename: &str) -> Result<String, TranspileError> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(Path::new(filename)).unwrap_or(SourceType::ts());
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        let details = parsed
            .diagnostics
            .iter()
            .map(|error| format!("{error}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(failure(filename, format!("Parse errors: {details}")));
    }
    let mut program = parsed.program;
    // The transformer needs semantic scoping (OXC 0.159 runs it via
    // `build_with_scoping`); a plain semantic pass is enough here —
    // the dashboard scripts use no enums or scope-dependent syntax.
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    // `TransformOptions::from(ESTarget)` sets the ES2022 environment
    // target (matching tsconfig `target: ES2022`); the TypeScript
    // transform itself is on by default and strips the types.
    let options = TransformOptions::from(ESTarget::ES2022);
    let transformed = Transformer::new(&allocator, Path::new(filename), &options)
        .build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        let details = transformed
            .diagnostics
            .iter()
            .map(|error| format!("{error}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(failure(filename, format!("Transform errors: {details}")));
    }
    Ok(Codegen::new().build(&program).code)
}

/// Transpile every `.ts` file directly inside `scripts_dir`
/// (non-recursive) and return the compiled scripts keyed by their
/// public path (`js/<stem>.js`), plus one `(file, error)` pair per
/// file that could not be read or transpiled. Files that fail are
/// skipped, never fatal: the caller logs the errors and serves the
/// scripts that did compile.
pub fn compile_scripts_dir(scripts_dir: &Path) -> (HashMap<String, String>, Vec<(String, String)>) {
    let mut compiled: HashMap<String, String> = HashMap::new();
    let mut errors: Vec<(String, String)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(scripts_dir) else {
        return (compiled, errors);
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ts"))
        .collect();
    paths.sort();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let public_path = format!("js/{stem}.js");
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_SCRIPT_SIZE => {
                errors.push((
                    file,
                    format!(
                        "File is too large: {} bytes (maximum {MAX_SCRIPT_SIZE})",
                        metadata.len()
                    ),
                ));
                continue;
            }
            Err(error) => {
                errors.push((file, format!("Cannot read file metadata: {error}")));
                continue;
            }
            _ => {}
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                errors.push((file, format!("Cannot read file: {error}")));
                continue;
            }
        };
        match transpile_typescript(&source, &file) {
            Ok(js) => {
                compiled.insert(public_path, js);
            }
            Err(error) => errors.push((file, error.to_string())),
        }
    }
    (compiled, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative dashboard script: IIFE, interface, typed
    /// locals, arrow functions, template literal. Mirrors what the
    /// converted `assets/js/*.ts` files look like.
    const SAMPLE: &str = r#"
(function (): void {
  "use strict";
  interface Widget { id: string; label: string; }
  const widgets: Widget[] = [{ id: "a", label: "A" }];
  function render(list: Widget[]): string {
    return list.map((w: Widget): string => w.label).join(", ");
  }
  const out: string = render(widgets);
  console.log(out);
})();
"#;

    #[test]
    fn transpiles_typed_script_to_plain_javascript() {
        let js = transpile_typescript(SAMPLE, "sample.ts").expect("sample must transpile");
        assert!(
            !js.contains("interface"),
            "output still contains an interface"
        );
        assert!(
            !js.contains(": Widget"),
            "output still contains a type annotation"
        );
        assert!(
            !js.contains(": string"),
            "output still contains a type annotation"
        );
        assert!(js.contains("render(widgets)"), "output lost the call");
        assert!(js.contains("\"use strict\""), "output lost the directive");
    }

    #[test]
    fn invalid_syntax_is_an_error() {
        let error = transpile_typescript("const x: = 1;", "bad.ts").expect_err("must fail");
        assert!(error.to_string().contains("bad.ts"), "error names the file");
    }

    #[test]
    fn type_errors_are_not_transpile_errors() {
        // The transpiler strips types without checking them; tsc owns
        // type errors. A mistyped assignment still produces JavaScript.
        // Codegen normalizes string quotes to double quotes, so the
        // value is checked in either form.
        let js = transpile_typescript("const n: number = 'a';", "types.ts")
            .expect("type errors must not fail transpilation");
        assert!(
            js.contains("'a'") || js.contains("\"a\""),
            "output lost the value: {js}"
        );
    }

    /// Transpile the real dashboard scripts from the workspace. Skips
    /// quietly when the `.ts` files do not exist yet (the conversion
    /// is still in progress) or the layout differs, so the crate stays
    /// independently testable.
    #[test]
    fn transpiles_the_dashboard_scripts() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/js");
        if !dir.is_dir() {
            return;
        }
        let (compiled, errors) = compile_scripts_dir(&dir);
        if compiled.is_empty() && errors.is_empty() {
            return;
        }
        assert!(errors.is_empty(), "transpile errors: {errors:?}");
        for name in [
            "js/dashboard.js",
            "js/admin-pages.js",
            "js/auth.js",
            "js/admin.js",
            "js/client.js",
        ] {
            let js = compiled
                .get(name)
                .unwrap_or_else(|| panic!("{name} was not compiled"));
            assert!(!js.is_empty(), "{name} compiled to empty output");
            assert!(
                !js.contains("interface "),
                "{name} still contains TypeScript"
            );
        }
    }
}
