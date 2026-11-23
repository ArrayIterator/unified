# unified-ts

TypeScript-to-JavaScript transpiler for the Unified dashboard
frontend. The server reads the TypeScript sources in `assets/js/`
at startup, transpiles them with OXC, and holds the JavaScript in
RAM — no `.js` files exist in the repository.

This is a **transpiler only, not a JavaScript engine**: the server
never executes the scripts; the browser runs the emitted JavaScript.
That is why this crate is small (OXC parser + transformer +
codegen, pure Rust) and has no runtime cost beyond startup. A
V8/deno/boa/quickjs embedding was deliberately not chosen — it
would add a huge native dependency, slow the build, and break the
musl static build, for no benefit: the server only transpiles.

## API

- `transpile_typescript(source, filename) -> Result<String, TranspileError>` — transpile one source. Never type-checks: a type error still transpiles (`tsc --noEmit` owns type errors, run at dev time via `npm run check`).
- `compile_scripts_dir(dir) -> (HashMap<String, String>, Vec<(String, String)>)` — transpile every `.ts` file directly inside a directory, keyed by public path (`js/<stem>.js`); per-file failures are reported, never fatal.

## Why OXC

Checked 2026-11-22 against SWC: OXC 0.159.0 ships parser,
transformer, and code generator as small lockstep-versioned crates
with a plain API (`Parser` -> `Transformer` -> `Codegen`); SWC's
equivalent set is larger, its major versions churn often
(`swc_ecma_parser` is at 48.x), and its compile time is longer.
