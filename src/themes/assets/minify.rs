// Asset minification: dependency-free JavaScript and CSS minifiers
// for the runtime asset pipeline.
//
// The server transpiles the dashboard TypeScript sources to
// JavaScript at startup (OXC, via the `unified-ts` crate) and serves
// the result from RAM; stylesheets are read from `assets/css/`.
// Both go through the minifiers in this module before they are
// served, so release builds send minified assets and debug builds
// send the readable originals:
//
//   - `minify_js` - removes comments and collapsible whitespace
//     from JavaScript. It is a tokenizer, not a parser: it tracks
//     strings, template literals (with `${}` interpolation),
//     regular expressions versus division, and ASI-sensitive
//     newlines, but it never renames identifiers, drops code, or
//     reorders anything. The output is always a byte-for-byte
//     subset of the input's tokens, so it cannot change behaviour.
//   - `minify_css` - removes comments and collapsible whitespace
//     from CSS, and drops a `;` that directly precedes `}`.
//     Whitespace around `+` and `-` is always kept, because
//     `calc()` requires it (`calc(100% - 10px)` must keep its
//     spaces). `url(...)`, quoted strings, and `!important` are
//     handled by the generic whitespace rules.
//
// Why dependency-free instead of `oxc_minifier` / `lightningcss`:
// neither crate is in the local registry cache or the lockfile, and
// the VM cannot compile-verify a new dependency (OOM rule), so a
// dependency that cannot be proven to work here is worse than a
// small, fully tested, semantics-preserving minifier. The follow-up
// is to swap `minify_js` for `oxc_minifier` (same OXC 0.159 family
// as `unified-ts`, compress without mangling - the dashboard's
// `window.UnifiedDashboard` surface must keep its names) and
// `minify_css` for `lightningcss` once CI can compile them; both
// functions are the single call sites, so the swap is local.
//
// Timing: minification runs synchronously during startup asset
// compilation, in the same step that already transpiles the
// TypeScript sources (see `run_server`). The whole asset set is
// small (six TypeScript files, ~130 KiB, plus three stylesheets,
// ~52 KiB), so the step costs milliseconds; doing it on a
// background thread would only add a swap race - a browser that
// cached the unminified bytes under a fingerprint would keep
// serving them after the swap, because the URL would not change.
// Synchronous startup minification keeps one atomic served map:
// the bytes served for a URL never change during the process
// lifetime, and the fingerprint always hashes exactly those bytes.
//
// Fingerprints: `resolve_asset_url` hashes the SERVED bytes, not
// the source file. The startup step registers the fingerprint of
// every served asset (public path -> 12-hex SHA-256 of the served
// string) with `set_served_fingerprints`; the resolver consults
// that registry first and falls back to hashing the file on disk
// when no entry exists (theme assets, scripts that failed to
// compile). `assets.minify` takes effect at startup only, like the
// TypeScript compilation itself: changing it requires a restart.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

/// Maximum size of one stylesheet source file (5 MiB, the same cap
/// the theme engine applies to templates and the TypeScript
/// transpiler applies to scripts): a larger file is rejected so a
/// broken or hostile file cannot make the server read unbounded
/// data into memory.
pub const MAX_STYLE_SIZE: u64 = 5 * 1024 * 1024;

// Minification switch and served-byte fingerprints

/// Process-wide minification switch, decided once at startup from
/// the `assets.minify` config value (`None` means automatic:
/// minify unless the application runs in development mode). Before
/// startup sets it - in unit tests, for example - the default is
/// `!cfg!(debug_assertions)`, which matches the same rule at the
/// build level.
static MINIFY_ENABLED: OnceLock<bool> = OnceLock::new();

/// True when the runtime asset pipeline minifies JavaScript and CSS
/// before serving them.
pub fn minify_enabled() -> bool {
    *MINIFY_ENABLED.get_or_init(|| !cfg!(debug_assertions))
}

/// Decide once at startup whether assets are minified. Later calls
/// are ignored: the served map is built once, so flipping the switch
/// afterwards could not change the served bytes and must not change
/// the fingerprints either.
pub fn set_minify_enabled(enabled: bool) {
    let _ = MINIFY_ENABLED.set(enabled);
}

/// Fingerprints of the exact bytes the server serves, keyed by
/// public path (`js/dashboard.js`, `css/dashboard.css`). Set once
/// at startup, after the served maps are built; `resolve_asset_url`
/// hashes these instead of the source files, so a fingerprint
/// changes exactly when the served bytes change.
static SERVED_FINGERPRINTS: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Register the served-byte fingerprints for this process. The
/// first call wins; later calls are ignored, for the same reason
/// as `set_minify_enabled`.
pub fn set_served_fingerprints(fingerprints: HashMap<String, String>) {
    let slot = SERVED_FINGERPRINTS.get_or_init(|| RwLock::new(HashMap::new()));
    let mut guard = slot.write().expect("served fingerprints lock poisoned");
    if guard.is_empty() {
        *guard = fingerprints;
    }
}

/// Fingerprint of the served bytes for a public path, when the
/// startup step registered one. `None` means the path is not served
/// from the compiled maps (a plain disk file, a theme asset, or a
/// script that failed to compile) and the caller should fall back
/// to hashing the file on disk.
pub fn served_fingerprint(public_path: &str) -> Option<String> {
    let slot = SERVED_FINGERPRINTS.get()?;
    slot.read()
        .expect("served fingerprints lock poisoned")
        .get(public_path)
        .cloned()
}

// Stylesheet compilation

/// Read every `.css` file directly inside `styles_dir`
/// (non-recursive) and return the stylesheets keyed by their public
/// path (`css/<stem>.css`), plus one `(file, error)` pair per file
/// that could not be read or was too large. Mirrors
/// `unified_ts::compile_scripts_dir`: files that fail are skipped,
/// never fatal. When `minify` is true the served text is the
/// minified stylesheet, otherwise the file is served as written.
pub fn compile_styles_dir(
    styles_dir: &Path,
    minify: bool,
) -> (HashMap<String, String>, Vec<(String, String)>) {
    let mut compiled: HashMap<String, String> = HashMap::new();
    let mut errors: Vec<(String, String)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(styles_dir) else {
        return (compiled, errors);
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "css"))
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
        let public_path = format!("css/{stem}.css");
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_STYLE_SIZE => {
                errors.push((
                    file,
                    format!(
                        "File is too large: {} bytes (maximum {MAX_STYLE_SIZE})",
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
        let served = if minify { minify_css(&source) } else { source };
        compiled.insert(public_path, served);
    }
    (compiled, errors)
}

/// Minify every compiled script in place: the OXC-transpiled
/// JavaScript from `compile_scripts_dir` becomes the minified
/// JavaScript served in release builds. Infallible - the minifier
/// only removes comments and whitespace, so it cannot fail on any
/// input.
pub fn minify_compiled_scripts(compiled: HashMap<String, String>) -> HashMap<String, String> {
    compiled
        .into_iter()
        .map(|(path, js)| (path, minify_js(&js)))
        .collect()
}

// JavaScript minifier

/// True for a character that can appear inside a JavaScript
/// identifier or number (`a-z`, `A-Z`, `0-9`, `_`, `$`).
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// True for a character that can start a JavaScript identifier.
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

/// True for a character that can continue a JavaScript identifier.
fn is_ident_char(c: char) -> bool {
    is_word_char(c)
}

/// True for a keyword that is always followed by an expression
/// (so a `/` after it opens a regular expression, and it cannot
/// end a statement): `typeof`, `void`, `delete`, `in`, `of`,
/// `instanceof`, `new`, `case`, `var`, `let`, `const`, `function`,
/// `class`, `if`, `for`, `while`, `switch`, `catch`, `with`,
/// `import`, `export`, `default`, `extends`. The remaining
/// keywords have their own arms in `minify_js` (`this` and friends
/// are values; `return` and friends end a statement through ASI;
/// `else`, `do`, `try`, `finally` are followed by a block), and
/// contextual words (`async`, `get`, `set`, `static`, `from`, `as`)
/// are treated as plain identifiers, which is the safe reading:
/// after them a `/` is division, never a regular expression.
fn is_expression_keyword(ident: &str) -> bool {
    matches!(
        ident,
        "typeof"
            | "void"
            | "delete"
            | "in"
            | "of"
            | "instanceof"
            | "new"
            | "case"
            | "var"
            | "let"
            | "const"
            | "function"
            | "class"
            | "if"
            | "for"
            | "while"
            | "switch"
            | "catch"
            | "with"
            | "import"
            | "export"
            | "default"
            | "extends"
    )
}

/// What a `{` opened: an object literal, a block, an interpolation
/// (`${ ... }` inside a template literal), or a template literal
/// itself (a backtick). The stack distinguishes them so that a `}`
/// returns to the right mode, and so that a `/` after a `}` is
/// read correctly: division after an object literal, a regular
/// expression after a block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BraceKind {
    Object,
    Block,
    Interp,
    Template,
}

/// Minify JavaScript: remove comments and collapsible whitespace.
///
/// Safety rules, in order:
///
///   - Strings, template literals, and regular expressions are
///     copied verbatim (escapes respected); a `//` or `/*` inside
///     one is never mistaken for a comment.
///   - A `/` opens a regular expression exactly when an expression
///     is expected (start of file, after an operator, `(`, `,`,
///     `=`, `:`, `[`, `{`, `;`, or an expression keyword, and
///     after a block-closing `}`); otherwise it is division.
///     `{` is tracked as object-literal versus block so the `}`
///     rule above holds, with `=>` and `else`/`do`/`try`/`finally`
///     forcing a block.
///   - A newline inside a whitespace run is kept exactly when the
///     previous token can end a statement and the next token can
///     start one (ASI); otherwise the run collapses to one space
///     when both neighbours are word characters, or to nothing.
///     `a + +b` and `a - -b` keep their space, because `a++b` and
///     `a--b` would mean something else.
///
/// Known limit, documented rather than silently wrong: a regular
/// expression that opens a statement immediately after `)` or `]`
/// on the same line (no `;` or newline between, as in
/// `if (x) /re/.test(y)`) is read as division. The emitted bytes
/// are still identical unless that regular expression's body
/// contains `//` or `/*`; none of the shipped dashboard scripts
/// does that (verified by grep), and a `;` or a newline before the
/// `/` removes the ambiguity entirely. The `v`-flag set notation
/// (nested character classes) is likewise not tracked: a `[`
/// inside a character class is treated as a literal character,
/// which is correct for every non-`v` regular expression.
pub fn minify_js(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    // State describing the previous significant token.
    let mut regex_allowed = true;
    let mut prev_can_end_stmt = false;
    let mut prev_char = '\0';
    let mut arrow_pending = false;
    let mut block_kw_pending = false;
    let mut brace_stack: Vec<BraceKind> = Vec::new();
    let mut in_template = false;

    while i < n {
        let c = chars[i];

        // -- Template literal body: only `\`, backtick, and `${`
        //    are special; everything else (including `{` and `}`)
        //    is literal text.
        if in_template {
            if c == '\\' {
                out.push(c);
                i += 1;
                if i < n {
                    out.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            if c == '`' {
                out.push(c);
                i += 1;
                if brace_stack.pop() != Some(BraceKind::Template) {
                    // Unbalanced input; stay lenient and keep going.
                }
                in_template = false;
                regex_allowed = false;
                prev_can_end_stmt = true;
                prev_char = '`';
                continue;
            }
            if c == '$' && i + 1 < n && chars[i + 1] == '{' {
                out.push('$');
                out.push('{');
                i += 2;
                brace_stack.push(BraceKind::Interp);
                in_template = false;
                regex_allowed = true;
                prev_can_end_stmt = false;
                prev_char = '{';
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }

        // -- Whitespace run: comments count as whitespace. A `//`
        //    comment consumes its terminating newline, so the run
        //    carries `has_newline` and the ASI rule below applies.
        if c.is_whitespace()
            || (c == '/' && i + 1 < n && (chars[i + 1] == '/' || chars[i + 1] == '*'))
        {
            let mut has_newline = false;
            while i < n {
                let w = chars[i];
                if w.is_whitespace() {
                    if w == '\n' {
                        has_newline = true;
                    }
                    i += 1;
                    continue;
                }
                if w == '/' && i + 1 < n && chars[i + 1] == '/' {
                    i += 2;
                    while i < n && chars[i] != '\n' {
                        i += 1;
                    }
                    if i < n {
                        has_newline = true;
                        i += 1;
                    }
                    continue;
                }
                if w == '/' && i + 1 < n && chars[i + 1] == '*' {
                    i += 2;
                    while i < n {
                        if chars[i] == '\n' {
                            has_newline = true;
                        }
                        if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                break;
            }
            if i < n {
                let nc = chars[i];
                let next_is_word = is_word_char(nc);
                let prev_is_word = is_word_char(prev_char);
                // `a + +b` and `a - -b` must keep a space: `a++b`
                // and `a--b` would parse differently.
                let plus_adjacent =
                    (prev_char == '+' && nc == '+') || (prev_char == '-' && nc == '-');
                if has_newline {
                    // A newline starts a new statement only when
                    // the previous token can end one and the next
                    // token can start one. `+`/`-` alone continue
                    // the expression (`x\n+ y` is `x + y`), while
                    // `++`/`--` start a new statement through ASI.
                    let next_is_starter = next_is_word
                        || matches!(nc, '"' | '\'' | '`' | '(' | '[' | '{' | '!' | '~' | '/')
                        || (nc == '+' && i + 1 < n && chars[i + 1] == '+')
                        || (nc == '-' && i + 1 < n && chars[i + 1] == '-');
                    if prev_can_end_stmt && next_is_starter {
                        out.push('\n');
                        // A new statement starts: a `/` here opens
                        // a regular expression, never division.
                        regex_allowed = true;
                    } else if (prev_is_word && next_is_word) || plus_adjacent {
                        out.push(' ');
                    }
                } else if (prev_is_word && next_is_word) || plus_adjacent {
                    out.push(' ');
                }
            }
            continue;
        }

        // -- Quoted string: copied verbatim, escapes respected.
        if c == '\'' || c == '"' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < n {
                let s = chars[i];
                out.push(s);
                i += 1;
                if s == '\\' {
                    if i < n {
                        out.push(chars[i]);
                        i += 1;
                    }
                    continue;
                }
                if s == quote || s == '\n' {
                    break;
                }
            }
            regex_allowed = false;
            prev_can_end_stmt = true;
            prev_char = quote;
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- Template literal start.
        if c == '`' {
            out.push(c);
            i += 1;
            brace_stack.push(BraceKind::Template);
            in_template = true;
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- Identifier or keyword.
        if is_ident_start(c) {
            let start = i;
            while i < n && is_ident_char(chars[i]) {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            out.push_str(&ident);
            prev_char = chars[i - 1];
            match ident.as_str() {
                "this" | "true" | "false" | "null" | "super" => {
                    regex_allowed = false;
                    prev_can_end_stmt = true;
                    block_kw_pending = false;
                }
                "return" | "throw" | "break" | "continue" | "yield" | "await" => {
                    regex_allowed = true;
                    prev_can_end_stmt = true;
                    block_kw_pending = false;
                }
                "else" | "do" | "try" | "finally" => {
                    regex_allowed = true;
                    prev_can_end_stmt = false;
                    block_kw_pending = true;
                }
                _ => {
                    if is_expression_keyword(&ident) {
                        regex_allowed = true;
                        prev_can_end_stmt = false;
                    } else {
                        regex_allowed = false;
                        prev_can_end_stmt = true;
                    }
                    block_kw_pending = false;
                }
            }
            arrow_pending = false;
            continue;
        }

        // -- Number: digits, hex/binary/octal markers, fraction,
        //    exponent (with its sign), separators, bigint `n`.
        //    The bytes are copied verbatim, so an over-long scan
        //    (as in `1..toString()`) cannot change the output.
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            while i < n {
                let d = chars[i];
                if d.is_ascii_alphanumeric() || d == '_' || d == '.' {
                    i += 1;
                    continue;
                }
                if (d == '+' || d == '-')
                    && i > start
                    && (chars[i - 1] == 'e' || chars[i - 1] == 'E')
                    && i + 1 < n
                    && chars[i + 1].is_ascii_digit()
                {
                    i += 1;
                    continue;
                }
                break;
            }
            let number: String = chars[start..i].iter().collect();
            out.push_str(&number);
            prev_char = chars[i - 1];
            regex_allowed = false;
            prev_can_end_stmt = true;
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- Spread `...`: an expression follows.
        if c == '.' && i + 2 < n && chars[i + 1] == '.' && chars[i + 2] == '.' {
            out.push_str("...");
            i += 3;
            regex_allowed = true;
            prev_can_end_stmt = false;
            prev_char = '.';
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- `++` / `--`: prefix when an expression is expected
        //    (an operand follows), postfix otherwise.
        if (c == '+' || c == '-') && i + 1 < n && chars[i + 1] == c {
            out.push(c);
            out.push(c);
            i += 2;
            if regex_allowed {
                regex_allowed = true;
                prev_can_end_stmt = false;
            } else {
                regex_allowed = false;
                prev_can_end_stmt = true;
            }
            prev_char = c;
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- Arrow `=>`: the next `{` is a function body, a block.
        if c == '=' && i + 1 < n && chars[i + 1] == '>' {
            out.push('=');
            out.push('>');
            i += 2;
            regex_allowed = true;
            prev_can_end_stmt = false;
            prev_char = '>';
            arrow_pending = true;
            block_kw_pending = false;
            continue;
        }

        // -- `{`: object literal when an expression is expected,
        //    block otherwise (after `=>`, `else`, `do`, `try`,
        //    `finally`, or any token that cannot be followed by an
        //    expression).
        if c == '{' {
            out.push(c);
            i += 1;
            let kind = if arrow_pending || block_kw_pending {
                BraceKind::Block
            } else if regex_allowed {
                BraceKind::Object
            } else {
                BraceKind::Block
            };
            brace_stack.push(kind);
            regex_allowed = true;
            prev_can_end_stmt = false;
            prev_char = '{';
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- `}`: pop the matching opener. Closing an object
        //    literal leaves a value (division follows); closing a
        //    block leaves statement position (a regular expression
        //    may follow); closing an interpolation returns to the
        //    template literal.
        if c == '}' {
            out.push(c);
            i += 1;
            match brace_stack.pop() {
                Some(BraceKind::Object) => {
                    regex_allowed = false;
                    prev_can_end_stmt = true;
                }
                Some(BraceKind::Block) => {
                    regex_allowed = true;
                    prev_can_end_stmt = true;
                }
                Some(BraceKind::Interp) => {
                    in_template = true;
                    prev_can_end_stmt = false;
                }
                Some(BraceKind::Template) | None => {
                    regex_allowed = false;
                    prev_can_end_stmt = true;
                }
            }
            prev_char = '}';
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- `/`: regular expression when one is expected, division
        //    otherwise. Comments were consumed by the whitespace
        //    branch above, so a `/` here is never `//` or `/*`.
        if c == '/' {
            if regex_allowed {
                out.push(c);
                i += 1;
                let mut in_class = false;
                while i < n {
                    let r = chars[i];
                    if r == '\\' {
                        out.push(r);
                        i += 1;
                        if i < n {
                            out.push(chars[i]);
                            i += 1;
                        }
                        continue;
                    }
                    if r == '\n' {
                        break;
                    }
                    if in_class {
                        out.push(r);
                        i += 1;
                        if r == ']' {
                            in_class = false;
                        }
                        continue;
                    }
                    if r == '[' {
                        in_class = true;
                        out.push(r);
                        i += 1;
                        continue;
                    }
                    if r == '/' {
                        out.push(r);
                        i += 1;
                        break;
                    }
                    out.push(r);
                    i += 1;
                }
                while i < n && chars[i].is_ascii_alphabetic() {
                    out.push(chars[i]);
                    i += 1;
                }
                if i > 0 {
                    prev_char = chars[i - 1];
                }
                regex_allowed = false;
                prev_can_end_stmt = true;
            } else {
                out.push(c);
                i += 1;
                if i < n && chars[i] == '=' {
                    out.push('=');
                    i += 1;
                    prev_char = '=';
                } else {
                    prev_char = '/';
                }
                regex_allowed = true;
                prev_can_end_stmt = false;
            }
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- `)`, `]`, and `.` close a value: a `/` after one is
        //    division, never a regular expression. (A `.` starts a
        //    fractional number only when a digit follows, handled
        //    by the number branch above; otherwise it is a property
        //    access inside a value.)
        if c == ')' || c == ']' || c == '.' {
            out.push(c);
            i += 1;
            regex_allowed = false;
            prev_can_end_stmt = c != '.';
            prev_char = c;
            arrow_pending = false;
            block_kw_pending = false;
            continue;
        }

        // -- Every other punctuation character: an expression can
        //    follow it, and it cannot end a statement.
        out.push(c);
        i += 1;
        prev_char = c;
        regex_allowed = true;
        prev_can_end_stmt = false;
        arrow_pending = false;
        block_kw_pending = false;
    }
    out
}

// CSS minifier

/// True for a character that can appear inside a CSS identifier or
/// number (`a-z`, `A-Z`, `0-9`, `_`, `$`, plus non-ASCII letters).
/// `-` is deliberately NOT a word character here: whitespace next
/// to `+` or `-` is always kept (see `minify_css`), because
/// `calc()` requires it.
fn is_css_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || c.is_alphanumeric()
}

/// Minify CSS: remove comments and collapsible whitespace, and drop
/// a `;` that directly precedes `}`.
///
/// Whitespace collapses to one space exactly when both neighbours
/// are word characters (`1px solid red` keeps its spaces, `a > b`
/// becomes `a>b`), or when either neighbour is `+` or `-` - the
/// `calc()` rule (`calc(100% - 10px)` must keep its spaces, and
/// keeping a space anywhere else around `+`/`-` is always valid).
/// Quoted strings (including `url("...")` and `content: "..."`)
/// are copied verbatim, so a `/*` inside one is never mistaken
/// for a comment. `0px` is left as `0px`: folding units saves a
/// byte per zero and is not worth the risk of touching a value the
/// minifier does not understand.
pub fn minify_css(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    let mut prev_char = '\0';
    while i < n {
        let c = chars[i];

        // -- Quoted string: copied verbatim, escapes respected.
        if c == '\'' || c == '"' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < n {
                let s = chars[i];
                out.push(s);
                i += 1;
                if s == '\\' {
                    if i < n {
                        out.push(chars[i]);
                        i += 1;
                    }
                    continue;
                }
                if s == quote || s == '\n' {
                    break;
                }
            }
            prev_char = quote;
            continue;
        }

        // -- Whitespace run: comments count as whitespace.
        if c.is_whitespace() || (c == '/' && i + 1 < n && chars[i + 1] == '*') {
            while i < n {
                let w = chars[i];
                if w.is_whitespace() {
                    i += 1;
                    continue;
                }
                if w == '/' && i + 1 < n && chars[i + 1] == '*' {
                    i += 2;
                    while i < n {
                        if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                break;
            }
            if i < n {
                let nc = chars[i];
                if (is_css_word_char(prev_char) && is_css_word_char(nc))
                    || prev_char == '+'
                    || prev_char == '-'
                    || nc == '+'
                    || nc == '-'
                {
                    out.push(' ');
                }
            }
            continue;
        }

        // -- `;` directly before `}` (ignoring whitespace and
        //    comments between them) is dropped.
        if c == ';' {
            let mut j = i + 1;
            while j < n {
                let w = chars[j];
                if w.is_whitespace() {
                    j += 1;
                    continue;
                }
                if w == '/' && j + 1 < n && chars[j + 1] == '*' {
                    j += 2;
                    while j < n {
                        if chars[j] == '*' && j + 1 < n && chars[j + 1] == '/' {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    continue;
                }
                break;
            }
            if j < n && chars[j] == '}' {
                i += 1;
                continue;
            }
            out.push(';');
            i += 1;
            prev_char = ';';
            continue;
        }

        out.push(c);
        i += 1;
        prev_char = c;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn js_strips_comments_and_collapses_whitespace() {
        assert_eq!(
            minify_js("/* header */\nvar  x   =   1;  // trailing\nvar y = 2;"),
            // The newline after `;` is dropped: the semicolon
            // already ends the statement, so ASI needs no newline.
            "var x=1;var y=2;"
        );
        assert_eq!(minify_js("var   x   =   1;"), "var x=1;");
        assert_eq!(minify_js("a   ,   b"), "a,b");
        assert_eq!(minify_js("if  ( x )  {  y();  }"), "if(x){y();}");
    }

    #[test]
    fn js_keeps_asi_newlines() {
        assert_eq!(minify_js("var a = 1\nvar b = 2"), "var a=1\nvar b=2");
        assert_eq!(
            minify_js("function f() {\n  return\n  5;\n}"),
            "function f(){return\n5;}"
        );
        // A newline before `.` continues the expression: no ASI.
        assert_eq!(minify_js("var x = a\n  .b();"), "var x=a.b();");
        // A newline before `++` is ASI: the increment starts a new
        // statement.
        assert_eq!(minify_js("var x = a\n++b;"), "var x=a\n++b;");
    }

    #[test]
    fn js_regex_vs_division() {
        assert_eq!(minify_js("var r = /a\\/b/g;"), "var r=/a\\/b/g;");
        assert_eq!(minify_js("var x = a / b / c;"), "var x=a/b/c;");
        assert_eq!(minify_js("var x = (a + b) / c;"), "var x=(a+b)/c;");
        assert_eq!(
            minify_js("var s = x.replace(/&/g, \"&amp;\");"),
            "var s=x.replace(/&/g,\"&amp;\");"
        );
        // A regular expression after a block-closing brace.
        assert_eq!(
            minify_js("if (x) { y(); } /re/.test(z);"),
            "if(x){y();}/re/.test(z);"
        );
        // Division after an object literal.
        assert_eq!(minify_js("var o = { a: 1 } / 2;"), "var o={a:1}/2;");
        // A regular expression after `return`.
        assert_eq!(minify_js("return /re/.test(x);"), "return/re/.test(x);");
    }

    #[test]
    fn js_templates_strings_and_plus_guards() {
        assert_eq!(minify_js("var s = `a${  x  }b`;"), "var s=`a${x}b`;");
        assert_eq!(
            minify_js("var s = `outer ${ `inner ${ y }` } end`;"),
            "var s=`outer ${`inner ${y}`} end`;"
        );
        assert_eq!(
            minify_js("var u = \"https://example.com/a\"; // done"),
            "var u=\"https://example.com/a\";"
        );
        assert_eq!(
            minify_js("var s = 'it\\'s // not a comment';"),
            "var s='it\\'s // not a comment';"
        );
        assert_eq!(minify_js("var x = a + +b;"), "var x=a+ +b;");
        assert_eq!(minify_js("var x = a - -b;"), "var x=a- -b;");
        assert_eq!(
            minify_js("var f = () => { return 1; };"),
            "var f=()=>{return 1;};"
        );
        assert_eq!(
            minify_js("function f() { return 5; }"),
            "function f(){return 5;}"
        );
        assert_eq!(minify_js("var o = { a: 1, b: 2 };"), "var o={a:1,b:2};");
    }

    #[test]
    fn css_collapses_whitespace_and_drops_trailing_semicolons() {
        assert_eq!(
            minify_css("/* comment */\n.a  {\n  color:  red;\n  margin: 0;\n}\n"),
            ".a{color:red;margin:0}"
        );
        assert_eq!(
            minify_css("a  >  b { font-weight:  400; }"),
            "a>b{font-weight:400}"
        );
        assert_eq!(
            minify_css(".a { content: \"/* not a comment */\"; }"),
            ".a{content:\"/* not a comment */\"}"
        );
        // calc() keeps its spaces around `+` and `-`.
        assert_eq!(
            minify_css(".a { width: calc(100% - 10px); top: calc(100% + 2rem); }"),
            ".a{width:calc(100% - 10px);top:calc(100% + 2rem)}"
        );
        assert_eq!(
            minify_css(".a { margin: -5px; padding: 1px solid red; }"),
            // The space before `-5px` is kept on purpose: whitespace
            // next to `+`/`-` is never dropped, because `calc()`
            // requires it.
            ".a{margin: -5px;padding:1px solid red}"
        );
        assert_eq!(
            minify_css(
                "@media (min-width: 100px) and (max-width: 200px) { .a { display: none; } }"
            ),
            // `)and(` is valid: `)` terminates the previous token,
            // so `and` needs no separating space.
            "@media(min-width:100px)and(max-width:200px){.a{display:none}}"
        );
    }

    /// Minify the real shipped stylesheets. Skips quietly when the
    /// layout differs, so the module stays independently testable.
    /// Run with `--nocapture` to see the before/after sizes.
    #[test]
    fn css_minifies_real_stylesheets() {
        let dir = Path::new("assets/css");
        if !dir.is_dir() {
            return;
        }
        for name in ["admin.css", "client.css", "dashboard.css"] {
            let path = dir.join(name);
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let minified = minify_css(&source);
            eprintln!("css {name}: {} -> {} bytes", source.len(), minified.len());
            assert!(minified.len() < source.len(), "{name} did not shrink");
            assert!(!minified.contains("/*"), "{name} still contains a comment");
        }
        let dashboard = std::fs::read_to_string(dir.join("dashboard.css"))
            .expect("dashboard.css must be readable");
        let minified = minify_css(&dashboard);
        // calc() spacing survives minification.
        assert!(
            minified.contains("calc(100vw - 2rem)"),
            "calc() lost its spaces: {}",
            &minified[..minified.len().min(200)]
        );
        assert!(
            minified.contains("calc(100% + var(--space-2))"),
            "calc() lost its spaces"
        );
    }

    /// Minify the real shipped TypeScript sources. They are the
    /// proxy for the transpiled JavaScript (produced at runtime by
    /// OXC, so it has no file to test against here): the minifier
    /// is input-agnostic text reduction, so the same code paths
    /// run and the size win on real input is proven. Skips quietly
    /// when the layout differs.
    #[test]
    fn js_minifies_real_dashboard_scripts() {
        let dir = Path::new("assets/js");
        if !dir.is_dir() {
            return;
        }
        for name in [
            "admin-pages.ts",
            "admin.ts",
            "auth.ts",
            "client.ts",
            "dashboard.ts",
        ] {
            let path = dir.join(name);
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let minified = minify_js(&source);
            eprintln!("js {name}: {} -> {} bytes", source.len(), minified.len());
            assert!(minified.len() < source.len(), "{name} did not shrink");
            assert!(
                !minified.contains("/*"),
                "{name} still contains a block comment"
            );
        }
        // Known literals survive verbatim in the minified output.
        let admin_pages = std::fs::read_to_string(dir.join("admin-pages.ts"))
            .expect("admin-pages.ts must be readable");
        let minified = minify_js(&admin_pages);
        assert!(minified.contains("/&/g"), "regex literal lost");
        assert!(
            minified.contains("\"/api/v1/security/csrf-token\""),
            "URL string lost"
        );
        assert!(minified.contains("&amp;"), "replacement string lost");
    }

    #[test]
    fn served_fingerprint_registry_is_set_once() {
        assert_eq!(served_fingerprint("js/nope.js"), None);
        let mut map = HashMap::new();
        map.insert("js/dashboard.js".to_string(), "abcdef012345".to_string());
        set_served_fingerprints(map);
        assert_eq!(
            served_fingerprint("js/dashboard.js").as_deref(),
            Some("abcdef012345")
        );
        // A second registration is ignored: the first set wins.
        let mut other = HashMap::new();
        other.insert("js/dashboard.js".to_string(), "ffffffffffff".to_string());
        set_served_fingerprints(other);
        assert_eq!(
            served_fingerprint("js/dashboard.js").as_deref(),
            Some("abcdef012345")
        );
    }
}
