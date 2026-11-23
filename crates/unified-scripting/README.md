# unified-scripting — TypeScript external-module runtime

External modules for Unified are authored in **TypeScript**, not
Rust. This crate is the server side of that contract: it
transpiles `.ts` sources to JavaScript in memory, executes them in
an embedded **QuickJS** runtime, and injects one `Unified` global
object holding the whole SDK — the same shape as Bun's `Bun`
global.

There is deliberately no V8 here: QuickJS is small (~1 MB),
embeddable, and musl-friendly, while V8 would break the static
musl binary and add ~100 MB to the build. No JavaScript engine
other than QuickJS is used.

## Layout

| File | What it does |
| --- | --- |
| `src/lib.rs` | Crate docs + module list. |
| `src/engine.rs` | `ScriptingEngine`: one OS worker thread owns every QuickJS runtime/context; callers talk to it over channels. Resource limits, failure counting, route/hook dispatch. |
| `src/sdk.rs` | `Unified` global: `build_unified_global`, the bootstrap script, route/hook/config/log builders. |
| `src/sdk_db.rs` | `db.query` (tenant-scoped SELECT-only) + the per-module `KvStore`. |
| `src/sdk_http.rs` | `http.fetch` (SSRF-safe blocking fetch). |
| `src/sdk_storage.rs` | `Unified.storage` (scoped file access). |
| `src/loader.rs` | Discovers modules in a directory, reads `module.json`, transpiles `module.ts` (OXC, in memory). |
| `src/host.rs` | Environment settings + `SdkHost` construction from `AppContext` + HookBus forwarders. |
| `src/module.rs` | Server integration: the `scripting` manager module, one `TsModule` per TS module, the dispatcher, admin reload endpoints. |
| `unified.d.ts` | Type declarations for the `Unified` global. |
| `tests/smoke.rs` | End-to-end smoke test (transpile → load → route + hook). |

## How a module is written

Create a directory under `modules/` (default; see Settings):

```
modules/hello/
  module.json   # id (must equal the directory name), name, version, description
  module.ts     # the module source
```

`module.ts`:

```ts
/// <reference path="../../crates/unified-scripting/unified.d.ts" />

Unified.routes.get("/hello", (req) => {
  const name = req.query.name ?? "world";
  return { status: 200, body: { message: `Hello, ${name}!` } };
});

Unified.hooks.on("user.created", (event) => {
  Unified.log.info("a user was created", event.data);
});
```

Type-check it with `tsc --noEmit` (see `modules/example-ts/`
for a working example with a `tsconfig.json`).

Rules:

- The entry file is always `module.ts`. No `import`/`export`:
  the script is evaluated as a plain script in QuickJS, and only
  the `Unified` global exists (no `fs`, `process`, Node, Deno,
  or Bun APIs).
- Handlers must be synchronous. An `async` route handler is
  refused at dispatch time; an `async` hook handler is skipped.
  There is no async executor driving promise jobs.
- Route patterns start with `/`, have 1–10 segments, and support
  `:name` segments (`/users/:id`). A module is served under
  `/api/v1/ts/<module-id>` + the pattern.
- Event names use `[a-z0-9._-]` only, at most 128 characters.

## The `Unified` API

See `unified.d.ts` for the full typed contract. Summary:

| Area | Functions |
| --- | --- |
| `Unified.version` | SDK version string (`"0.1.0"`). |
| `Unified.module` | `{ id, name, version }` of this module. |
| `Unified.log` | `debug/info/warn/error(...args)` → server log (`unified::scripting::<module-id>`). |
| `Unified.config` | `get(key)`, `keys()` — read-only curated settings (never secrets). |
| `Unified.hooks` / `Unified.events` | `on(event, handler)`, `emit(event, data?)` — the real server hook bus (same object, two names). |
| `Unified.routes` | `get/post/put/patch/delete(pattern, handler)` — declare HTTP routes. A handler returns `{ status, body }` or any JSON value (body, status 200). The dispatcher wraps the body in the standard `{"data": ...}` envelope. |
| `Unified.http` | `fetch(url, options?)` → `{ ok, status, headers, body }`. SSRF-safe (details below). |
| `Unified.storage` | `read(path)`, `write(path, data)`, `list(dir?)`, `delete(path)` — scoped to this module's directory. |
| `Unified.db.kv` | `get/set/delete/list` — per-module key/value store, namespaced by tenant + module id. |
| `Unified.db.query` | `query(sql, params?)` — tenant-scoped SELECT only, `$N` params substituted as escaped text literals, 1000-row cap. |

Published types: module authors depend on `@unified/sdk-types`
(`packages/unified-sdk-types`), whose `index.d.ts` is generated
from `unified.d.ts` by `npm run sync` (verified by `npm run check`).
`unified.d.ts` stays the single source of truth — never edit the
generated copy by hand.

## Sandbox limits

Every module runs in its own isolated QuickJS context with
hard limits (defaults; see `ScriptLimits` in `engine.rs`):

- **Memory:** 64 MiB per module context.
- **Stack:** 512 KiB.
- **Execution timeout:** 5 s per call (route, hook, or top-level
  script), enforced by an interrupt handler.
- **Module source:** 5 MiB (`MAX_SCRIPT_SIZE`, shared with the
  dashboard transpiler).
- **Consecutive failures:** at 10 the module status flips to
  `"error"` (visible in `GET /api/v1/scripting/modules`).

What a module cannot do unless an admin grants it:

- No filesystem access outside its own storage directory
  (absolute paths, `..` escapes, and symlink escapes are
  rejected; writes are atomic; 10 MiB file cap).
- No raw network: `http.fetch` is http(s) only, no credentials
  in the URL, every resolved address must be public (loopback
  only with the dev exception), redirects never followed,
  10 s default / 30 s max timeout, 5 MiB body cap, 2048-char URL
  cap. An admin allowlist of exact hosts skips the address
  check.
- `db.query` is SELECT only — WITH is refused because a CTE can
  carry a write (comments skipped, `;` rejected), runs only in the
  current tenant's schema, and fails closed without a tenant
  context or a configured database. It never touches the
  control-plane database.

## Settings

Environment variables (see `host.rs`):

| Variable | Default | Meaning |
| --- | --- | --- |
| `UNIFIED_SCRIPTING_MODULES_DIR` | `modules` | Directory scanned for modules at startup. |
| `UNIFIED_SCRIPTING_STORAGE_DIR` | `data/scripting-storage` | Root of per-module storage directories. |
| `UNIFIED_SCRIPTING_HTTP_ALLOWLIST` | (empty) | Comma-separated hosts allowed to skip the public-IP check. |
| `UNIFIED_SCRIPTING_ALLOW_LOOPBACK` | `false` | Development exception: allow loopback fetch targets (`1`/`true`). |

## Loading and reloading

- **Startup:** the `scripting` manager module loads every
  directory under the modules directory that contains a
  `module.ts`, transpiles it (OXC, in memory — the compiled JS
  is cached in RAM), runs it, and registers one `TsModule` in
  the `ModuleRegistry` plus HookBus forwarders for its events.
  A module that fails to load is logged and skipped; it never
  takes the server down.
- **Reload (no restart):** `POST /api/v1/scripting/modules/{id}/reload`
  re-reads the manifest, re-transpiles, and re-runs the module
  in a fresh context. Hook forwarders are diffed (removed
  events unregistered, new events registered); the dispatcher
  routes resolve by module id at request time, so they need no
  change. `POST /api/v1/scripting/modules/reload` reloads all
  modules. Both require the `scripting:manage` capability
  (checked in the handler; they fail closed while the global
  auth middleware is still landing).
- **List:** `GET /api/v1/scripting/modules` returns the loaded
  modules with their routes, hooks, status, and failure counts.

## Architecture notes

- **One worker thread, many contexts.** A single dedicated OS
  thread owns every QuickJS runtime/context (QuickJS contexts
  are not `Send`). Callers hold `Arc<ScriptingEngine>` and send
  requests over `std::sync::mpsc`; synchronous calls block
  with a timeout, hook emissions are fire-and-forget.
- **Handlers live in JavaScript.** The bootstrap script keeps
  route/hook tables in JS (`__unified`); Rust keeps only
  metadata (method/path/event). No JS function values are ever
  stored in Rust, so there are no self-referential lifetimes.
- **Tenant scoping.** The tenant id is carried per call
  (thread-local on the worker, set from the HTTP request's
  tenant or the event payload's `tenant_id`). `db.query` fails
  closed without one.
- **The dispatcher never blocks a tokio worker.** Route
  dispatch runs on the Actix blocking pool (`web::block`).

## Verification

```sh
cargo test -p unified-scripting   # smoke test + unit tests
cargo fmt -p unified-scripting -- --check
```

`tsc --noEmit` on `unified.d.ts` + `modules/example-ts/` is
checked separately (TypeScript via bun/npm). A full
`cargo check` of the whole workspace is OOM-blocked in the
dev VM; the final proof is the Colab run after the push.
