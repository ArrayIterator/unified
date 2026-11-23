# example-hello-module

A complete minimal external module for the Unified server. One HTTP
route, one hook listener, no database, no configuration section. Copy
this crate to start your own module.

## What it demonstrates

| Feature | Where in `src/lib.rs` |
| --- | --- |
| Implementing the `Module` trait (`unified_core::modules::Module`) | `impl Module for HelloModule` |
| `is_core()` returning `false` (required for external modules) | `is_core` |
| One route, `GET /api/v1/hello`, returning the standard success envelope | `hello_handler`, `configure_routes` |
| Route metadata for the route catalog | `route_info` |
| One hook listener on the `app.start` event | `register_hooks` |
| Startup hook (no-op here) | `init` |
| The server-side registration function | `register` |

## How a third party writes their own module

1. **Copy this directory** next to your server checkout (or anywhere,
   as long as the `unified-core` path dependency below still resolves)
   and rename the package in `Cargo.toml` (`name`, `description`).
   Keep `edition = "2024"` and `rust-version = "1.85"`.
2. **Depend only on `unified-core`** for Unified APIs:
   `unified-core = { path = "../unified-core" }`. Add your own
   crates.io dependencies as needed. Never depend on the `unified`
   binary crate, on another module crate, or on the server's HTTP,
   CLI, auth, theme, or gateway code — none of those exist as
   libraries, and inter-module communication goes through the hook
   bus only.
3. **Implement `Module`** in `src/lib.rs`: pick a unique, stable,
   lowercase `id()` (it is the key the Extensions Manager, the
   dependency graph, the route catalog, and entitlements use);
   return `false` from `is_core()`; list anything you need in
   `requires()`; register routes under `/api/v1/<your-id>/`;
   describe them in `route_info()`; return the fixed JSON envelope
   from `unified_core::json` in every handler.
4. **Expose `pub fn register(registry: &ModuleRegistry)`** exactly
   as in this crate. That is the one function the server calls.
5. **Wire it into the server** (a server-side change, done by whoever
   builds the server binary):
   - Root `Cargo.toml`: add `<your-crate> = { path = "<path>" }`
     under `[dependencies]` (or a `git` / crates.io version).
   - `src/modules/mod.rs::register_all`: add one line in the
     external-modules section at the end, after all core modules:
     `<your_crate>::register(registry);`
   - Rebuild and restart. There is no runtime install: Rust has no
     stable ABI, so dynamic `.so` plugin loading is intentionally
     not supported (see the design document, section 4.4).
6. **Verify**: `cargo check --workspace`, `cargo test --workspace`,
   then boot the server and check `GET /api/v1/<your-id>/...` and
   the Modules API listing (your module shows `is_core: false`).

## What this crate deliberately does not do

- **No database.** A module that needs its own tables embeds its own
  Diesel migrations in its own crate and runs them from `init()`
  against the pool in `ctx.db` (an `Arc<RwLock<Option<DbPool>>>`).
  It never runs the core migrations and never opens its own pool.
  See the design document, section 4.1.
- **No configuration section.** A module that needs settings reads
  them from the shared `AppConfig` (or from its own settings store,
  via its own routes) — it does not add fields to `unified-core`.
- **No CLI commands or middleware.** Both are available through the
  trait (`register_cli`, `register_middleware`); this example leaves
  them at their defaults to stay minimal.

## Contract

The full contract — workspace layout, the exact `unified-core` API
surface (250 public items), versioning and compatibility rules, and
the step-by-step follow-up plan — is in
`docs/WORKSPACE_AND_EXTERNAL_MODULES.md` in the server repository.
The API names used in `src/lib.rs` match section 3 of that document
exactly; if they ever disagree, the document is authoritative and
this crate must be updated to match it.

## Status

Design stage. The `unified-core` crate does not exist yet — the
workspace follow-up step creates it with exactly the API surface in
the design document — so this crate does not compile on its own yet.
That is expected: the follow-up wires this crate in as a temporary
pilot smoke test, verifies it, and then removes the server-side
registration line again. The crate stays in the workspace so
`cargo check --workspace` keeps it compiling.
