# Workspace and External Modules

Design document for the Unified server (repo `ArrayIterator/unified`).

Status: **implemented 2026-11-22**. The workspace move in §5 steps 1–8
landed (see "Implementation notes (2026-11-22)" at the end of §5 and the
§6.2/§6.3 resolutions below). Compilation verification (`cargo check`,
`cargo test`, boot smoke test) is the follow-up gate and was **not** run
as part of the move — no Rust toolchain work was done by the mover; the
cargo-check agent owns it. Migrations live in
`crates/unified-core/migrations/` (§6.2 resolution). The design-stage
text below is kept as the record of what was planned; where the
implementation deviated, the deviation is noted inline.

All file and line references were verified by reading the staging tree
(`~/workspace/unified-push`) on 2026-11-22, during Phase 1. The tree was
changing while this document was written (the Rust file count grew from
762 to 822 between two reads as split builders added files), so every
count below is marked as-read and must be re-verified with the same
`grep` commands in the follow-up step. Nothing in this document was
verified by compiling: there is no Rust toolchain in this environment
and the crate registry is unreachable, so `cargo check` is the first
verification gate of the follow-up step, not of this document.

Language: English only. All code, comments, and documentation in the
workspace follow this rule.

## 1. Grounding: how the server is built today

### 1.1 Current layout

The repository is a single Cargo package. The root `Cargo.toml` has no
`[workspace]` section:

| Path | What it is |
| --- | --- |
| `Cargo.toml` | Single package `unified`, version `0.1.0`, edition `2024`, `rust-version = "1.85"`, `[[bin]] name = "unified"`, `path = "src/main.rs"`, `publish = false` |
| `src/main.rs` | Declares `mod auth; mod cli; mod core; mod db;` (`mod gateway;` behind `#[cfg(feature = "gateway")]`), `mod http; mod modules; mod themes;` — `main()` only calls `crate::cli::run()` |
| `src/core/` | 15 modules: `cache`, `config`, `context`, `error`, `hardening`, `hooks`, `json`, `logging`, `middleware`, `modules`, `pagination`, `reference_id`, `routes`, `storage`, `tenancy` (`src/core/mod.rs`, 22 lines). Note: `src/core/sharing.rs` exists on disk but is **not** declared in `src/core/mod.rs` — see §6.1 |
| `src/db/` | `mod.rs` (pool + migrations), `models.rs` (19,566 lines), `schema.rs` (12,551 lines) |
| `src/cli/` | `commands.rs`, `daemon.rs`, `monitor.rs`, `mod.rs` (`run()` is the binary entry point) |
| `src/http/` | `server.rs`, `tls.rs`, `acme.rs`, `cert_swap.rs`, `websocket.rs`, `mod.rs` |
| `src/modules/` | 137 modules, each exposing `pub fn register(registry: &ModuleRegistry)` and a `*Module` struct implementing `crate::core::modules::Module` |
| `src/auth/`, `src/themes/`, `src/gateway/` | Auth helpers, theme engine, Pingora reverse-proxy gateway (feature-gated) |
| `migrations/` | Diesel migrations `000001`–`000107`, embedded at compile time by `src/db/mod.rs` |
| `config.example.yaml` | Example configuration file |

The `gateway` Cargo feature is `gateway = ["dep:pingora"]`
(`pingora` 0.4, optional). With the feature off, `crate::gateway`
does not exist and the default build links no Pingora code.

### 1.2 How modules are registered today (the single registration point)

Startup order in `src/cli/commands.rs`, function `do_start`
(around lines 381–439):

1. `HookBus::new()` and `ModuleRegistry::new()` are created.
2. `crate::modules::register_all(&modules)` is called. This is the
   **single registration point** for every module in the binary.
   `src/modules/mod.rs::register_all(registry: &ModuleRegistry)`
   calls 137 `<module>::register(registry)` functions in dependency
   order, grouped by the comments in that file (foundation; identity
   and access; infrastructure; content and presentation; education
   and learning; commerce; business operations; governance, risk and
   compliance; workflow and approvals; people and support; platform;
   public API surface). Each module's `register` is one line, for
   example `src/modules/cache/module.rs`:
   `registry.register(Arc::new(CacheModule))`.
3. `modules.register_all_hooks(&hooks)` — every module registers its
   hook handlers on the bus.
4. `AppContext::new(config, hooks, modules)` is built.
5. `crate::db::pool_from_config(&db_config)` builds the pool,
   `crate::db::run_migrations(&pool)` runs the embedded core
   migrations, and `ctx.set_db(pool)` attaches the pool to the
   context.
6. `modules.init_all(&ctx)` runs `Module::init` on every registered
   module, in registration order. Core migrations have already run
   at this point, so a module's `init` may safely use the database.
7. `crate::http::server::run_server(ctx)` builds the Actix server.
   Inside the server factory, `ctx.modules.register_all_middleware`
   collects module middleware and `ctx.modules.configure_all_routes`
   mounts every module's routes
   (`src/http/server.rs::run_server`, around lines 145 and 265).

`build_cli_registry()` in the same file (around line 122) repeats steps
1–2 to collect module-injected CLI commands before argument parsing.

### 1.3 How widely the core is used (as read)

- 741 of 822 Rust files reference `crate::core::` (as read; the
  denominator was still growing under Phase 1).
- 45 files reference `crate::db::`; 41 of them are under
  `src/modules/`.
- Distinct `crate::core::<top-level>` uses, by count:
  `modules` 755, `error` 726, `hooks` 707, `json` 704, `context` 680,
  `pagination` 678, `tenancy` 87, `config` 30, `middleware` 26,
  `reference_id` 24, `storage` 18, `hardening` 14, `routes` 10,
  `sharing` 7, `cache` 3, `logging` 1.
- Distinct `crate::db::<top-level>` uses, by count: `schema` 114,
  `models` 29, `schema_exists` 2, `run_migrations_in_schema` 2,
  `run_migrations` 1, `pool_from_config` 1, `drop_tenant_schema` 1.
- Only 5 files under `src/modules/` reference `crate::http::`
  (`tls` 10 uses, `websocket` 2, `acme` 2, `server` 1). The HTTP
  layer is therefore almost entirely server-private already.
- `src/modules/extensions.rs` (the Modules / Extensions Manager)
  enables and disables already-registered modules. The enabled state
  is kept in memory and is advisory: it gates the state reported by
  the API and the `module.enabled` / `module.disabled` events, but it
  does **not** unregister already-mounted routes. Full dynamic unload
  (route rebuild without a restart) is a follow-up, not part of this
  design. Disabling a core module is refused with 409.

Reproduce the counts in the follow-up with:

```sh
find src -name '*.rs' | wc -l
grep -rl 'crate::core::' src | wc -l
grep -rl 'crate::db::' src | wc -l
grep -rhoE 'crate::core::[a-z_]+' src/ | sort | uniq -c | sort -rn
grep -rhoE 'crate::db::[a-z_]+' src/ | sort | uniq -c | sort -rn
```

## 2. Workspace layout

### 2.1 Members

The workspace has three members:

```toml
[workspace]
members = [".", "crates/unified-core", "crates/example-hello-module", "crates/unified-calculator"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "LicenseRef-Proprietary"

[workspace.dependencies]
# Shared version pins for crates used by more than one member.
# The follow-up step fills this table from the root Cargo.toml
# dependency list (actix-web, serde, serde_json, diesel, ...).
# Members refer to them as `actix-web.workspace = true`.
```

| Member | Kind | Contents after the move |
| --- | --- | --- |
| `.` (root package `unified`) | Binary crate | Everything that stays server-private: `src/main.rs`, `src/cli/`, `src/http/` (except `tls.rs`, which moves), `src/auth/`, `src/themes/`, `src/gateway/`, `src/modules/` (the 137 built-in modules, until they become module crates one bounded context at a time), `migrations/` (see §6.2). Keeps `[[bin]] name = "unified"`, the `gateway` feature, and the release/dev profiles. Depends on `unified-core = { path = "crates/unified-core" }` |
| `crates/unified-core` | Library crate | The whole of `src/core/` (except `logging.rs`, see §3.3), the whole of `src/db/`, and `src/http/tls.rs` as `unified_core::tls`. This is the **only** library crate in the workspace and the **only** crate an external module may depend on from this project |
| `crates/example-hello-module` | Library crate (example) | A complete minimal external module, written against the `unified-core` API in §3. It stays a workspace member so `cargo check --workspace` keeps it compiling, but it is **not** a production module: the follow-up wires it into the server only as a temporary pilot smoke test (§5, step 7), then removes the registration call again |
| `crates/unified-calculator` | Library crate (first real module crate, §5 step 8) | The calculator module, moved out of `src/modules/` in the follow-up. Depends on `unified-core` only; registered from `register_all` in place of the removed in-tree one |

Rules:

- The root package stays the server binary. It is never turned into a
  library crate. A binary crate cannot be depended on by an external
  crate, and that is intentional: it is what forces external modules
  to depend only on `unified-core`.
- One crate per **bounded context**, never one crate per tiny module.
  A bounded context is a group of modules that change together and
  share one domain language — the groups already named by the comments
  in `src/modules/mod.rs::register_all` are the starting map
  (education and learning; commerce; business operations; content and
  presentation; people and support; platform; and so on). Splitting
  one small module into its own crate buys a second `Cargo.toml` and
  a second version to track for every 200-line module, and it invites
  cross-crate imports between modules that should only talk through
  the hook bus.
- Module crates depend on `unified-core` only (plus their own
  crates.io dependencies). They never depend on the `unified` binary
  crate, never depend on another module crate, and never import
  `crate::http`, `crate::cli`, `crate::auth`, `crate::themes`, or
  `crate::gateway` — those paths do not exist as libraries, and
  inter-module communication goes through the hook bus only.
- The `gateway` feature and the Pingora dependency stay in the root
  package. `unified-core` must never depend on Pingora.

### 2.2 Which modules become crates first, which stay, and why

Order is by dependency weight, lightest first, so each step is
independently verifiable:

1. **Pilot (follow-up step, §5 step 7): `crates/example-hello-module`.**
   It proves the registration mechanism end to end with zero domain
   logic. It is an intentional exception to the bounded-context rule:
   it exists to be copied, not to be shipped.
2. **First real module crate (follow-up step, §5 step 8):
   the calculator module.** Verified as the lightest real candidate:
   `src/modules/calculator/` is 7 files, 2,467 lines, imports only
   `crate::core::{context, error, hooks, json, modules, pagination}`,
   and has **zero** `crate::db::` and **zero** `crate::http::`
   references. It exercises routes, hooks, and pagination against
   `unified-core` without touching the database. It is also an
   intentional single-module-crate exception (the pilot): do not copy
   the exception as the pattern. Its long-term home is a
   bounded-context crate; the follow-up records which one when the
   neighbouring modules are crated.
3. **Then one bounded context at a time**, following the groups in
   `register_all`, starting with the contexts whose modules have the
   fewest `crate::db::models` / `crate::db::schema` imports. The
   follow-up measures each candidate with
   `grep -rl 'crate::db::' src/modules/<context>/` before choosing
   it; no context is chosen by size alone.
4. **Stay in the server crate for now:** `src/cli`, `src/http`
   (minus `tls.rs`), `src/auth`, `src/themes`, `src/gateway`, and all
   137 built-in modules not yet crated. They keep compiling against
   `unified-core` through the path rewrites in §5. `src/db/models.rs`
   and `src/db/schema.rs` move into `unified-core` **as-is, unsplit**:
   the separate schema/models splitting work happens later, and a
   workspace move must not be combined with a 32,000-line refactor.
   Diesel `table!` macros need care, and `models.rs` (`use
   crate::db::schema::*;`) and `schema.rs` must move together.

## 3. The exact `unified-core` API surface

Everything in this section moves from the server crate into
`crates/unified-core`, with its **current source file path** named for
each item. The list was built by reading every file in `src/core/`,
`src/db/mod.rs`, and `src/http/tls.rs`, plus the
`crate::core::` / `crate::db::` grep counts in §1.3.

Counting rule: the headline count counts named public items
(structs, enums, traits, functions, type aliases, constants, and
sub-modules) listed in the tables below. A sub-module such as
`hooks::events` counts as **one** item; its constants are listed
separately and are not added to the headline count. The `db::models`
and `db::schema` modules each count as one item; the structs and
macros inside them are counted separately below the table and are not
added to the headline count.

**Headline count: 250 public items** (19 source files: 15 from
`src/core/`, 1 from `src/http/`, 3 from `src/db/`).

Canonical import paths after the move are
`unified_core::<module>::<Item>` (for example
`unified_core::modules::Module`,
`unified_core::db::models::User`). The crate root additionally
re-exports the five most-used items for ergonomics (§3.2), but the
canonical module paths are the contract — the example module in
`crates/example-hello-module/` uses canonical paths only.

### 3.1 Per-file tables

#### `src/core/modules.rs` — 18 items

The module system. This is the file external modules depend on most.

| Item | Kind | Notes |
| --- | --- | --- |
| `Module` | trait | The contract every module implements. `Send + Sync`, object-safe. Methods: `id()`, `name()`, `version()` (default `"1.0.0"`), `description()`, `is_core()` (default `true`), `dependencies()` (legacy spelling), `requires()`, `requires_libraries()`, `requires_external_libraries()`, `available_as_product()`, `product_category()`, `register_hooks(&HookBus)`, `register_cli(&mut CliRegistry)`, `configure_routes(&mut ServiceConfig)`, `route_info()`, `route_version_support()`, `register_middleware(&mut MiddlewareManager)`, `init(&AppContext) -> UnifiedResult<()>` (default `Ok(())`). Every method added later **must** carry a default, so existing external modules keep compiling |
| `module_requires` | fn | Merged requirements: `requires()` first, then legacy `dependencies()`, de-duplicated, order preserved |
| `ModuleInfo` | struct | Public summary of a module, as returned by the Modules API |
| `ModuleProductOffering` | struct | One sellable module as a product offering (id, name, description, category, version) |
| `module_external_requirements` | fn | `requires_external_libraries()` of one module, as a `Vec` |
| `DependencyIssue` | struct | One dependency-graph or product-declaration problem (`kind`, `module`, `dependency`, `cycle`, `message`) |
| `DependencyReport` | struct | `valid` flag plus every `DependencyIssue`, in registration order |
| `CliCommandDef` | struct | One dynamically injected CLI command (name, about, clap `Command`, handler) |
| `CliRegistry` | struct | `new()`, `register()`, `build_commands()`, `dispatch()`, `names()` |
| `ModuleRegistry` | struct | `new()`, `register(Arc<dyn Module>)`, `get()`, `list()`, `ids()`, `summary()`, `sellable_modules()`, `product_offerings()`, `validate_product_declarations()`, `requires_of()`, `route_version_support_of()`, `dependents_of()`, `validate_dependencies()`, `transitive_requires_ordered()`, `init_all()`, `register_all_hooks()`, `register_all_cli()`, `configure_all_routes()`, `route_catalog()`, `register_all_middleware()` |
| `RouteInfo` | struct | Route metadata for the catalog (`method`, `path`, `owner`, `description`) |
| `RouteVersionSupport` | enum | `All`, `Min`, `Max`, `Range`, `List`; default `All` |
| `ProductCategory` | enum | Closed set: `Productivity`, `Communication`, `Commerce`, `Infrastructure`, `Ai`; `ALL`, `as_str()`, `from_str()`, `is_valid()` |
| `ExternalLibraryProbe` | enum | Closed allowlist of version probes (ImageMagick, Ffmpeg, Git, …) |
| `ExternalLibraryRequirement` | struct | `name`, `minimum_version`, `reason`, `probe`; `new()`, `with_probe()` (both `const`) |
| `MODULE_AUTO_ACTIVATED_EVENT` | const | `"module.auto_activated"` |
| `MODULE_ENTITLEMENT_GRANTED_EVENT` | const | `"module.entitlement.granted"` |
| `MODULE_ENTITLEMENT_DENIED_EVENT` | const | `"module.entitlement.denied"` |

#### `src/core/hooks.rs` — 6 items

| Item | Kind | Notes |
| --- | --- | --- |
| `HookBus` | struct | `new()`, `register()`, `register_with_priority()`, `register_once()`, `register_once_with_priority()`, `unregister()`, `unregister_by_id()`, `emit()`, `emit_if_registered()`, `has()`, `events()`, `registrations()`, `registrations_detailed()`. Handlers run by priority, highest first; same priority runs in registration order. One-shot handlers run at most once. A handler re-entered on the same thread is skipped (recursion guard) |
| `HookContext` | struct | Per-emit context: `event`, `data` (`serde_json::Value`), `results`; `new()`, `stop()`, `is_stopped()`, `set()`, `get()`, `add_result()` |
| `HookHandler` | type | `Arc<dyn Fn(&mut HookContext) + Send + Sync>` |
| `HookRegistration` | struct | One registered handler as reported by `registrations_detailed` (`id`, `event`, `owner`, `priority`, `once`) |
| `DEFAULT_PRIORITY` | const | `0` (`isize`) |
| `events` | module | 26 well-known event-name constants: `APP_START` (`"app.start"`), `APP_STOP`, `APP_RELOAD`, `APP_PAUSE`, `APP_RESUME`, `APP_MAINTENANCE`, `CONFIG_VALIDATE`, `CONFIG_RELOAD`, `REQUEST_BEFORE`, `REQUEST_AFTER`, `AUTH_LOGIN`, `AUTH_LOGOUT`, `AUTH_FAILED`, `USER_CREATED`, `USER_UPDATED`, `ROLE_CREATED`, `MODULE_ENABLED`, `MODULE_DISABLED`, `SSL_RENEW`, `SSL_SWAP`, `GEO_LOOKUP`, `LICENSE_ISSUED`, `LICENSE_REVOKED`, `DOWNLOAD_STARTED`, `DOWNLOAD_FINISHED`, `SYNC_CONNECTED` |

#### `src/core/config.rs` — 17 items

| Item | Kind | Notes |
| --- | --- | --- |
| `AppConfig` | struct | Whole configuration. `default_path()`, `load()`, `load_default()`, `validate() -> Vec<String>`, `test()`, `is_debug()`, `database_url()` |
| `AppSection` | struct | `mode`, `dashboard_path`, `session_name`, `csrf_duration`, `secret_key`, `salt_key`, `user`, `tcp`, `ssl`, `workers`, timeouts, connection limits, memory cleanup |
| `SslSection` | struct | `auto_create`, `listen`, `cert`, `key`, `ssl_session_cache`, `domains` |
| `DomainSsl` | struct | Per-domain `cert` / `key` |
| `CacheSection` | struct | `dir`, `default_ttl_secs`, `max_entries`, `failover_enabled` |
| `RedisSection` | struct | `enabled`, `unix_socket`, `host`, `port`, credentials, pool sizes, timeouts |
| `DatabaseSection` | struct | `driver` (`postgresql` / `mysql` / `sqlite`), socket, host, port, credentials, pool sizes, timeouts, log settings, `collation` |
| `LicenseSection` | struct | MaxMind license key |
| `GeoSection` | struct | `database_path`, `download_url`, `auto_update`, `update_interval_hours`, `settings_override` |
| `AcmeSection` | struct | `enabled`, `directory_url`, `contacts` |
| `StoreSection` | struct | `download_signing_secret` |
| `ApiSection` | struct | `prefix`, `migrate_internal`, `default_version`, `default_rate_limit_per_minute`, `default_auth_required` |
| `LoadBalancerSection` | struct | Health-check interval/timeout/path, `default_strategy`, `max_backends_per_balancer` |
| `GatewaySection` | struct | `enabled`, `listen`, `tls_listen`, `default_upstream`, `request_log_capacity`, `health_check_interval_secs`, `connect_timeout_secs`; `validate()` |
| `RecordsSection` | struct | `retention_days` |
| `CONFIG_ENV` | const | `"UNIFIED_CONFIG_FILE"` |
| `DEFAULT_CONFIG_FILE` | const | `"config.yaml"` |

#### `src/core/error.rs` — 2 items

| Item | Kind | Notes |
| --- | --- | --- |
| `UnifiedError` | struct | Every error in the application. Fields `message`, `code`, `metadata`, `trace`. Constructors `new()`, `bad_request()` (400), `unauthorized()` (401), `forbidden()` (403), `not_found()` (404), `conflict()` (409), `unprocessable()` (422), `internal()` (500); `with_metadata()`, `with_trace()`, `to_json(debug)` (error envelope: `message`, `code`, `metadata`, plus `trace` in debug mode only). `Display` and `std::error::Error`. `From` for `std::io::Error`, `serde_yaml::Error`, `serde_json::Error` (classified into `metadata.kind`: `io` / `syntax` / `data` / `eof`), and `diesel::result::Error` (message sanitised, kind in `metadata.kind`, full error in `trace`) |
| `UnifiedResult<T>` | type | `Result<T, UnifiedError>` |

#### `src/core/context.rs` — 2 items

| Item | Kind | Notes |
| --- | --- | --- |
| `AppContext` | struct | Shared context built once at startup. Public fields: `config: Arc<RwLock<AppConfig>>`, `hooks: Arc<HookBus>`, `modules: Arc<ModuleRegistry>`, `cache: Arc<Cache>`, `geo: Arc<RwLock<Option<GeoInfo>>>`, `db: Arc<RwLock<Option<DbPool>>>`, `storage_registry: Arc<StorageRegistry>`, `routes: Arc<RouteRegistry>`, `tls: Arc<RwLock<Option<Arc<SslStorage>>>>`, `start_time`, `paused`, `maintenance`. Methods: `new()`, `is_debug()`, `uptime_seconds()`, `set_geo()`, `geo()`, `set_db()`, `set_tls()`, `tls()`, `reload_config()` (validates first; a failing config never replaces the running one; emits `config.reload`), `is_paused()`, `set_paused()`, `is_maintenance()`, `set_maintenance()`. **Dependency note:** this file imports `crate::db::DbPool` and `crate::http::tls::SslStorage`, which is why `src/db/` and `src/http/tls.rs` must move together with the core (see §3.3) |
| `GeoInfo` | struct | Geo data for one IP (`ip`, `country_code`, `country_name`, `city`, `region`, `asn`, `asn_organization`, `latitude`, `longitude`, `source`) |

#### `src/core/cache.rs` — 16 items

| Item | Kind | Notes |
| --- | --- | --- |
| `Cache` | struct | Shared cache: `new()`, `adapter_kind()`, `failover_active()`, `default_ttl()` (plus get/set/delete operations defined on the struct in the file) |
| `CacheAdapter` | trait | `Send + Sync + Debug`; implemented by `FilesystemCache`, `RedisCache`, `FailoverCache` |
| `CacheEntry` | struct | `new()`, `is_expired()`, `ttl_remaining()` |
| `CacheValue` | enum | Text / number / boolean / bytes / object, with `is_*` and `as_*` accessors and `from_json` / `as_json` |
| `FilesystemCache` | struct | `new()`, `with_max_entries()`, `dir()`, `max_entries()` |
| `RedisCache` | struct | `new(&RedisSection)` |
| `FailoverCache` | struct | `new(primary, fallback)` |
| `build_cache` | fn | Builds the `Arc<Cache>` from `AppConfig` (Redis when `redis.enabled`, else filesystem) |
| `validate_key` | fn | Key validation (`MAX_KEY_LEN`) |
| `validate_tenant_id` | fn | Tenant id validation (`MAX_TENANT_ID_LEN`) |
| `tenant_key` | fn | `tenant:{id}:{key}` namespacing |
| `MAX_KEY_LEN` | const | `512` |
| `REDIS_KEY_PREFIX` | const | `"unified:cache:"` |
| `TENANT_KEY_PREFIX` | const | `"tenant:"` |
| `MAX_TENANT_ID_LEN` | const | `128` |
| `DEFAULT_MAX_FS_ENTRIES` | const | `10_000` |

#### `src/core/json.rs` — 5 items

The fixed API envelope: success is exactly one top-level key `data`;
error is exactly one top-level key `error`.

| Item | Kind | Notes |
| --- | --- | --- |
| `success` | fn | `{"data": ...}` |
| `error_json` | fn | Error envelope from a `UnifiedError` |
| `success_response` | fn | `HttpResponse` carrying a success envelope with a status code |
| `error_response` | fn | `HttpResponse` carrying an error envelope |
| `respond` | fn | `Result<T, UnifiedError>` into a success or error `HttpResponse` |

#### `src/core/pagination.rs` — 7 items

| Item | Kind | Notes |
| --- | --- | --- |
| `PaginationParams` | struct | Raw `page` / `per_page` / `sort` request parameters |
| `Pagination` | struct | Normalised pagination: `normalized()`, `offset()`, `limit()`, `allowed_sort()` |
| `PageResult<T>` | struct | One page of serialisable items plus total and pagination |
| `PageInfo` | struct | `new()`, `to_json()` |
| `DEFAULT_PAGE` | const | `1` |
| `DEFAULT_PER_PAGE` | const | `20` |
| `MAX_PER_PAGE` | const | `100` |

#### `src/core/reference_id.rs` — 24 items

| Item | Kind | Notes |
| --- | --- | --- |
| `generate` | fn | Reference id with the default prefix |
| `generate_with_prefix` | fn | Reference id with an explicit prefix |
| `generate_unique` | fn | Generated id, retried while an `exists` callback reports a collision |
| `generate_unique_with_prefix` | fn | Same, with an explicit prefix |
| `generate_for` | fn | Reference id for a product key, honouring `ReferenceIdSettings` |
| `is_valid` | fn | Shape validation of a reference id |
| `parse` | fn | `Option<ReferenceOrderId>` |
| `get_timestamp` | fn | `Option<u64>` timestamp embedded in an id |
| `get_time_range_pattern` | fn | `TimeRangePattern` for a prefix and a millisecond range |
| `format_prefix` | fn | Prefix normalisation |
| `timestamp_ms_now` | fn | Current time in milliseconds |
| `timestamp_hex` | fn | 12-hex-digit timestamp encoding |
| `emit_generated` | fn | Emits `reference.generated` on the bus |
| `ReferenceOrderId` | struct | Parsed reference id |
| `TimeRangePattern` | struct | `from_bound()`, `to_bound()` |
| `ReferenceIdSettings` | struct | `new()`, `with_default_prefix()`, `prefix_for_product()`, `set_product_prefix()`, `remove_product_prefix()`, `generate()`, `generate_with_prefix()`, `validate()`, `sanitized()` |
| `DEFAULT_PREFIX` | const | `"ORDR"` |
| `TOTAL_LENGTH` | const | `30` |
| `TIMESTAMP_HEX_LENGTH` | const | `12` |
| `RANDOM_HEX_LENGTH` | const | `12` |
| `TIMESTAMP_MASK` | const | `0xFFFF_FFFF_FFFF` |
| `EXAMPLE_REFERENCE_ID` | const | `"ORDR-019d43d20eb8-6a5a7925dfb5"` |
| `EVENT_REFERENCE_GENERATED` | const | `"reference.generated"` |
| `EVENT_REFERENCE_SETTINGS_CHANGED` | const | `"reference.settings_changed"` |

#### `src/core/routes.rs` — 13 items

| Item | Kind | Notes |
| --- | --- | --- |
| `RouteRegistry` | struct | Named page routes: `new()`, `from_config_section()`, `apply_config_map()`, `apply_legacy_dashboard_path()`, `resolve()`, `route_url()`, `route_url_or()`, `default_path()`, `override_source()`, `list()`, `entry()`, `set_override()`, `clear_override()`, `resolve_redirect()`, `list_redirects()`, `validate_override()` |
| `RoutesSection` | struct | Config section: `get()`, `validate()` |
| `RouteEntry` | struct | One named route (name, path, source) |
| `RedirectEntry` | struct | One redirect (from, to) |
| `RouteChange` | struct | One route change event payload |
| `DefaultRoute` | struct | One built-in default route |
| `DEFAULT_ROUTES` | const | The built-in route table |
| `OverrideSource` | enum | Where an override came from; `as_str()`, `from_str()` |
| `ROUTE_CHANGED_EVENT` | const | `"route.changed"` |
| `RESERVED_PREFIXES` | const | `["/api/", "/.well-known/", "/assets/", "/theme/"]` |
| `paths_conflict` | fn | True when two paths collide |
| `normalize_path` | fn | Path normalisation |
| `validate_path_shape` | fn | Path shape validation |

#### `src/core/storage.rs` — 27 items

| Item | Kind | Notes |
| --- | --- | --- |
| `StorageKind` | enum | Storage backend kind; `as_str()`, `all()` |
| `StorageProvider` | trait | `Send + Sync` object store interface |
| `StorageRegistry` | struct | `new()`, `register()`, `get()`, `list()`, `remove()` |
| `LocalProvider` | struct | Filesystem provider: `new()`, `base_dir()`, `checksum()`, `recorded_checksum()` |
| `S3Config` | struct | S3 target configuration |
| `S3Provider` | struct | `new()` |
| `R2Config` | struct | R2 target configuration |
| `R2Provider` | struct | `new()` |
| `DriveConfig` | struct | Google Drive target configuration |
| `GoogleDriveProvider` | struct | `new()` |
| `SftpConfig` | struct | SFTP target configuration; `effective_port()` |
| `SftpProvider` | struct | `new()` |
| `StorageTargetConfig` | struct | One configured target; `validate()`, `masked_config()` |
| `StorageSection` | struct | Config section (`targets`, `default_target`) |
| `provider_from_config` | fn | Builds a provider from a target config |
| `build_storage_registry` | fn | Builds the `Arc<StorageRegistry>` from a `StorageSection` |
| `validate_key` | fn | Object-key validation (`MAX_KEY_LEN`) |
| `validate_target_name` | fn | Target-name validation |
| `sha256_hex` | fn | Hex SHA-256 of bytes |
| `mask_secret` | fn | Secret masking for display |
| `SECRET_CONFIG_FIELDS` | const | 6 field names treated as secrets |
| `is_allowed_storage_host` | fn | Host allowlist check |
| `validate_endpoint_url` | fn | Endpoint URL validation against the allowlist |
| `is_allowed_sftp_host` | fn | SFTP host allowlist check |
| `validate_sftp_host` | fn | SFTP host validation |
| `MAX_KEY_LEN` | const | `1024` (different from `cache::MAX_KEY_LEN` = 512; same name, different module — no conflict) |
| `events` | module | 5 constants: `TARGET_CREATED` (`"storage.target.created"`), `TARGET_DELETED`, `OBJECT_UPLOADED`, `OBJECT_DELETED`, `OBJECT_CHECKSUM_MISMATCH` |

#### `src/core/tenancy.rs` — 19 items

| Item | Kind | Notes |
| --- | --- | --- |
| `TenantContext` | struct | Tenant scope resolved for a request: `from_resolved()`, `from_http_request()`. **Not** the same type as `middleware::TenantContext` (§3.4) |
| `ControlPlaneGuard` | struct | `is_control_plane_request()` |
| `schema_name` | fn | `tenant_<uuid>` schema name for a tenant id |
| `is_valid_schema_name` | fn | Schema-name validation (allowlist shape only) |
| `schema_name_for_str` | fn | Validated schema name from a raw string |
| `set_tenant_search_path` | fn | `SET LOCAL search_path` on one connection |
| `with_tenant_connection` | fn | Runs a closure with a tenant-scoped connection |
| `run_tenant_migrations` | fn | Runs the embedded core migrations inside one tenant schema |
| `schema_exists` | fn | Takes a tenant id (`Uuid`). **Not** the same function as `db::schema_exists`, which takes a schema name (`&str`) (§3.4) |
| `drop_tenant_schema` | fn | Takes a tenant id (`Uuid`). **Not** the same function as `db::drop_tenant_schema`, which takes a schema name (`&str`) (§3.4) |
| `SCHEMA_PREFIX` | const | `"tenant_"` |
| `CONTROL_PLANE_SCHEMA` | const | `"public"` |
| `TENANT_SCOPE_CONTRACT` | const | The tenant-scope contract text every data access must honour |
| `EVENT_TENANT_PROVISIONED` | const | `"tenant.provisioned"` |
| `EVENT_TENANT_SUSPENDED` | const | `"tenant.suspended"` |
| `EVENT_TENANT_DESTROY_REQUESTED` | const | `"tenant.destroy_requested"` |
| `EVENT_TENANT_DESTROYED` | const | `"tenant.destroyed"` |
| `EVENT_TENANT_SCHEMA_CREATED` | const | `"tenant.schema_created"` |
| `EVENT_TENANT_SCHEMA_DROPPED` | const | `"tenant.schema_dropped"` |

#### `src/core/hardening.rs` — 37 items

| Item | Kind | Notes |
| --- | --- | --- |
| `SecuritySettings` | struct | Brute-force, rate-limit, flood, IP-list, header, body-size settings; `from_app_config()`, `validate()` |
| `SecuritySettingsUpdate` | struct | Partial settings update |
| `HeadersUpdate` | struct | Response-header update |
| `CidrRange` | struct | Parsed CIDR range; `contains()` |
| `parse_cidr` | fn | Parses one CIDR string |
| `IpListEntry` | struct | One allowlist/blocklist entry |
| `LockoutStatus` | struct | One brute-force lockout |
| `RateLimitEntry` | struct | One rate-limit bucket |
| `ReputationEntry` | struct | One IP reputation entry |
| `FailedAuthCluster` | struct | One cluster of failed authentications |
| `RateDecision` | enum | Allow / deny decision of a rate check |
| `HardeningState` | struct | Live hardening state: `new()`, `settings()`, `update_settings()`, `update_headers()`, `is_allowlisted()`, `is_blocklisted()`, `ip_list_entries()`, `add_ip_entry()`, `remove_ip_entry()`, `record_login_failure()`, `record_login_success()`, `is_locked()`, `lockout_statuses()`, `unlock()`, `failed_auth_clusters()`, `check_rate_limits()`, `rate_limit_entries()` |
| `shared_state` | fn | The installed `Arc<HardeningState>`, or a lazily created default |
| `install_shared_state` | fn | Installs the shared `HardeningState` |
| `client_ip` | fn | Client IP extraction for a request |
| `rate_limited_response` | fn | 429 response for a rate-limited request |
| `security_events` | module | 7 constants: `BRUTEFORCE_LOCKED` (`"security.bruteforce.locked"`), `BRUTEFORCE_UNLOCKED`, `AUTH_FAILED` (`"security.auth.failed"`), `IP_FLAGGED`, `IP_UNFLAGGED`, `SETTINGS_UPDATED`, `RATE_LIMITED` |
| 20 `DEFAULT_*` consts | consts | `DEFAULT_BRUTE_FORCE_MAX_FAILURES` (5), `DEFAULT_BRUTE_FORCE_WINDOW_SECS` (900), `DEFAULT_BRUTE_FORCE_BASE_LOCKOUT_SECS` (300), `DEFAULT_BRUTE_FORCE_LOCKOUT_MULTIPLIER` (2), `DEFAULT_BRUTE_FORCE_MAX_LOCKOUT_SECS` (3600), `DEFAULT_RATE_LIMIT_PER_IP_REQUESTS` (300), `DEFAULT_RATE_LIMIT_PER_IP_WINDOW_SECS` (60), `DEFAULT_RATE_LIMIT_PER_USER_REQUESTS` (600), `DEFAULT_RATE_LIMIT_PER_USER_WINDOW_SECS` (60), `DEFAULT_RATE_LIMIT_PER_ENDPOINT_REQUESTS` (120), `DEFAULT_RATE_LIMIT_PER_ENDPOINT_WINDOW_SECS` (60), `DEFAULT_FLOOD_BURST_THRESHOLD` (100), `DEFAULT_FLOOD_BURST_WINDOW_SECS` (10), `DEFAULT_MAX_CONCURRENT_PER_IP` (50), `DEFAULT_MAX_BODY_BYTES` (1,048,576), `DEFAULT_MAX_BODY_BYTES_UPLOAD` (104,857,600), `DEFAULT_FAILED_AUTH_ALERT_THRESHOLD` (5), `DEFAULT_FAILED_AUTH_ALERT_WINDOW_SECS` (900), `DEFAULT_IP_REPUTATION_FLAG_THRESHOLD` (100), `DEFAULT_IP_REPUTATION_BURST_PENALTY` (25) |

#### `src/core/middleware.rs` — 19 items

| Item | Kind | Notes |
| --- | --- | --- |
| `Middleware` | trait | `Send + Sync + Debug + 'static`; one middleware unit |
| `MiddlewareManager` | struct | `new()`, `register()`, `register_by_type()`, `register_function()`, `len()`, `is_empty()`, `clear()`, `create_dispatcher()` |
| `MiddlewareResult` | type | `BoxFuture<Result<ServiceResponse<BoxBody>, actix_web::Error>>` |
| `NextFn` | type | Next-middleware invocation |
| `Dispatcher` | struct | Ordered middleware pipeline; `new()` |
| `DispatcherMiddleware<S>` | struct | Actix middleware adapter around the `Dispatcher` |
| `DispatcherFuture<F>` | struct | In-flight dispatcher future |
| `TenantContext` | struct | Middleware tenant context, resolved from the request: `from_request()`, `from_service_request()`, `is_tenant_request()`. **Not** the same type as `tenancy::TenantContext` (§3.4) |
| `TenantResolutionConfig` | struct | `normalized()`, `with_main_host()`, `with_blocked_paths()`, `with_exempt_paths()`, `with_tenant_header_name()`, `with_unknown_host_policy()`, `with_missing_host_policy()`, `from_environment()` |
| `TenantResolutionMiddleware` | struct | `new()`, `with_defaults()`, `config()` |
| `UnresolvedHostPolicy` | enum | Policy for an unresolvable host; `parse()` |
| `TenantKind` | enum | Kind of tenant for a request |
| `normalize_host` | fn | Host normalisation |
| `normalize_path_list` | fn | Path-list normalisation |
| `EVENT_TENANT_RESOLVE_REQUESTED` | const | `"tenant.resolve_requested"` |
| `EVENT_TENANT_CONTROL_PLANE_BLOCKED` | const | `"tenant.control_plane_blocked"` |
| `DEFAULT_TENANT_HEADER` | const | `"X-Unified-Tenant"` |
| `DEFAULT_BLOCKED_PATHS` | const | Control-plane paths blocked on tenant hosts |
| `DEFAULT_EXEMPT_PATHS` | const | `["/health", "/.well-known/acme-challenge/"]` |

#### `src/core/sharing.rs` — 28 items

| Item | Kind | Notes |
| --- | --- | --- |
| `SharingSettings` | struct | Public-link and auditor settings; `validate()` |
| `ShareScope` | struct | One share scope: `new()`, `parse()`, `to_scope_string()`, `matches()` |
| `ShareableKind` | struct | One shareable resource kind |
| `ShareRegistry` | struct | `new()`, `register()`, `is_known_kind()`, `is_known_view()`, `kinds()` |
| `PublicLink` | struct | One public link; `is_expired()`, `is_revoked()` |
| `LinkDenial` | enum | Denial reason; `as_str()` |
| `PublicLinkAccessAttempt` | struct | One recorded access attempt |
| `AuditorScopeEntry` | struct | One auditor scope entry |
| `AuditorScope` | struct | Auditor allow/deny scope: `allows()`, `allows_kind()`, `allows_export()`, `covered_record_ids()`, `covered_company_ids()` |
| `check_allow_public_link` | fn | Asks the bus whether a public link is allowed |
| `sha256_hex` | fn | Hex SHA-256 of a string (same name as `storage::sha256_hex`, which takes bytes — different module, no conflict) |
| `sha256_hex_bytes` | fn | Hex SHA-256 of bytes |
| `constant_time_eq` | fn | Constant-time byte comparison |
| `generate_link_token` | fn | Generates a link token (value + hash) |
| `hash_link_token` | fn | Hashes a presented link token |
| `generate_auditor_password` | fn | Generates an auditor password |
| `sign_link_url` | fn | Signs a link URL (HMAC) |
| `verify_signed_link_url` | fn | Verifies a signed link URL |
| `validate_public_link` | fn | Validates a public link against its scope and expiry |
| `access_attempt` | fn | Builds a `PublicLinkAccessAttempt` |
| `record_failed_link_validation` | fn | Records a failed validation |
| `record_successful_link_validation` | fn | Records a successful validation |
| `sharing_events` | module | 14 constants: `LINK_CREATED` (`"sharing.link.created"`), `LINK_REVOKED`, `LINK_ACCESSED`, `LINK_ACCESS_DENIED`, `PUBLIC_ACCESS`, `ALLOW_CHECK`, `AUDITOR_CREATED`, `AUDITOR_REVOKED`, `AUDITOR_EXPIRED`, `AUDITOR_ACTION`, `AUDITOR_SESSION_STARTED`, `AUDITOR_SESSION_REVOKED`, `AUDITOR_EXPIRE_REQUESTED`, `AUDITOR_SCOPE_CHECK` |
| `MAX_LINK_TTL_DAYS` | const | `30` |
| `MAX_AUDITOR_TTL_HOURS` | const | `720` |
| `MIN_AUDITOR_TTL_HOURS` | const | `1` |
| `LINK_TOKEN_BYTES` | const | `32` |
| `AUDITOR_PASSWORD_BYTES` | const | `24` |

#### `src/http/tls.rs` — 2 items (moves to `unified_core::tls`)

| Item | Kind | Notes |
| --- | --- | --- |
| `SslStorage` | struct | Live TLS certificate storage, shared by the HTTPS listener, the load balancer TLS endpoint, the certificates module, and the gateway. `from_config(&SslSection)`, `lookup()`, `get()`, `cert_expiry()`, `certificate_names()`. Its imports are only `core::config`, `core::error`, `chrono`, `dashmap`, `rcgen`, `rustls`, `rustls-pki-types`, `serde`, `sha2`, `x509-certificate` — nothing from `src/http/` itself, which is why it can move cleanly |
| `to_ascii_name` | fn | Name normalisation for certificate lookup |

#### `src/db/mod.rs` — 6 items (become `unified_core::db`)

| Item | Kind | Notes |
| --- | --- | --- |
| `DbPool` | type | `Pool<ConnectionManager<PgConnection>>` — the shared Diesel PostgreSQL pool |
| `pool_from_config` | fn | Builds the pool from `AppConfig` |
| `run_migrations` | fn | Runs the embedded Diesel migrations against the pool |
| `run_migrations_in_schema` | fn | Runs the same embedded migrations inside one validated tenant schema (`SET LOCAL` search path, schema-local `__diesel_schema_migrations` table) |
| `schema_exists` | fn | Takes a schema name (`&str`). **Not** `tenancy::schema_exists`, which takes a tenant id (`Uuid`) (§3.4) |
| `drop_tenant_schema` | fn | Takes a schema name (`&str`). Irreversible; `DROP SCHEMA "<name>" CASCADE`. **Not** `tenancy::drop_tenant_schema`, which takes a tenant id (`Uuid`) (§3.4) |

#### `src/db/models.rs` and `src/db/schema.rs` — 2 items

| Item | Kind | Notes |
| --- | --- | --- |
| `db::models` | module | Diesel models for every table in the schema (migrations `000001`–`000107`). **1,344 `pub struct` items as read.** `use crate::db::schema::*;` at the top. Money fields use `diesel::data_types::PgNumeric`; secret columns carry `#[serde(skip_serializing)]`. Moves as-is, unsplit |
| `db::schema` | module | Diesel `table!` definitions for every table. **678 `table!` macros as read.** Moves as-is, unsplit, together with `models.rs` |

### 3.2 Crate root of `unified-core`

`crates/unified-core/src/lib.rs` (created in the follow-up) declares:

```rust
pub mod cache;
pub mod config;
pub mod context;
pub mod db;
pub mod error;
pub mod hardening;
pub mod hooks;
pub mod json;
pub mod middleware;
pub mod modules;
pub mod pagination;
pub mod reference_id;
pub mod routes;
pub mod sharing;
pub mod storage;
pub mod tenancy;
pub mod tls;
```

plus root re-exports for the five most-used items only:

```rust
pub use config::AppConfig;
pub use context::AppContext;
pub use error::{UnifiedError, UnifiedResult};
pub use hooks::HookBus;
pub use modules::{Module, ModuleRegistry};
```

The canonical paths stay `unified_core::<module>::<Item>`. The root
re-exports are a convenience, not a second contract: documentation
and the example module always show the canonical path.

### 3.3 What does NOT move into `unified-core`

| Item | Why it stays |
| --- | --- |
| `src/core/logging.rs` (`init_logging`) | It references `crate::modules::logs::LogsLayer`. A library crate must not depend on the server crate's modules; the dependency would be circular. `init_logging` stays in the server crate. `unified-core` does not provide logging setup; modules log with `log` / `tracing` directly |
| `src/cli/` | Binary entry point, daemon control, monitor UI. Server-private |
| `src/http/` except `tls.rs` | Server, ACME client, certificate swap, websocket handling. Server-private. Only `SslStorage` moves, because `AppContext` holds it (§3.1, `context.rs`) |
| `src/auth/`, `src/themes/`, `src/gateway/` | Auth helpers, theme engine, Pingora gateway. Server-private |
| `src/modules/` (137 modules) | They compile against `unified-core` after the path rewrites; they become crates one bounded context at a time (§2.2) |
| The `gateway` feature / Pingora | Feature-gated in the root package only. `unified-core` has no features that pull a proxy stack |

### 3.4 Name collisions the move must preserve (not "fix")

These pairs have the same short name but are different types in
different modules. The follow-up must keep both and must not merge
them:

- `tenancy::TenantContext` vs `middleware::TenantContext` — the
  tenancy one is built from a resolved tenant (`from_resolved`,
  `from_http_request`); the middleware one is resolved from the live
  request (`from_request`, `from_service_request`). Callers use the
  full path.
- `tenancy::schema_exists` / `tenancy::drop_tenant_schema` (take a
  tenant id, `Uuid`) vs `db::schema_exists` /
  `db::drop_tenant_schema` (take a schema name, `&str`).
- `cache::MAX_KEY_LEN` (512) vs `storage::MAX_KEY_LEN` (1024);
  `cache::validate_key` vs `storage::validate_key`;
  `sharing::sha256_hex` (takes `&str`) vs `storage::sha256_hex`
  (takes bytes).
- `config::AppConfig::database_url()` builds a Diesel URL; it is not
  a pool builder — `db::pool_from_config` is.

### 3.5 Dependencies of `unified-core`

Derived from the top-level `use` statements of the moved files plus
the fully-qualified paths inside them (`redis::`, `tokio::`,
`url::` in `cache.rs` / `storage.rs`) and `src/http/tls.rs`. The
follow-up confirms the list with `cargo check`
and adds anything the compiler demands; nothing is added speculatively:

`actix-web`, `actix-service`, `async-trait`, `base64`, `chrono`,
`clap`, `dashmap`, `diesel`, `diesel_migrations`, `futures-util`,
`hmac`, `rand`, `rcgen`, `redis`, `regex`, `reqwest`, `rustls`
(with the `aws-lc-rs` feature, matching the root `Cargo.toml`),
`rustls-pki-types`, `serde`, `serde_json`, `serde_yaml`, `sha2`,
`tokio`, `url`, `uuid`, `x509-certificate`. Versions are pinned in
`[workspace.dependencies]` (§2.1) and inherited with
`dependency.workspace = true`. `unified-core` has no dependency on
`pingora`, `ratatui`, `crossterm`, `handlebars`, `lettre`,
`maxminddb`, or any other server-only crate.

## 4. External-module registration mechanism

### 4.1 What a third-party crate does

An external module is an ordinary Rust library crate. It implements
the `Module` trait from `unified_core::modules` and nothing else from
this project:

```rust
use std::sync::Arc;
use unified_core::modules::{Module, ModuleRegistry};

pub struct HelloModule;

impl Module for HelloModule {
    fn id(&self) -> &'static str { "hello" }
    fn name(&self) -> &'static str { "Hello" }
    fn description(&self) -> &'static str { "Example external module." }
    fn is_core(&self) -> bool { false }
    // ... routes, hooks, init — see crates/example-hello-module/src/lib.rs
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` (for example `src/modules/cache/module.rs`).
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HelloModule));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(HelloModule)
}
```

Rules for the implementation:

- `id()` must be unique across the whole registry and stable across
  versions: it is the key the Extensions Manager, the dependency
  graph, the route catalog, and entitlements use. Use lowercase
  letters, digits, and `-` only. The registry today does **not**
  reject a duplicate id (`ModuleRegistry::register` pushes; `get`
  returns the first match), so uniqueness is a rule enforced by
  review until the duplicate-id check in §6.3 lands.
- `is_core()` **must** return `false`. The default is `true`, and a
  module that leaves the default is treated as a core module: the
  Extensions Manager refuses to disable it (409) and it is reported
  as core in the Modules API.
- `version()` is the module's own version (default `"1.0.0"`).
  It is unrelated to the `unified-core` version (§4.3).
- `requires()` lists the ids of modules this module needs. The
  registry validates the graph (`validate_dependencies`: missing
  dependencies and cycles are both reported) and the Extensions
  Manager auto-activates required modules transitively, in
  dependency order, when this module is enabled. A module that needs
  another module's routes or hooks must declare it here; silently
  relying on registration order is a bug.
- Route paths live under `/api/v1/<module-id>/` unless the module
  documents a reason not to. `route_info()` should describe every
  route `configure_routes` registers, so the route catalog
  (`ModuleRegistry::route_catalog`) lists the module.
- Handlers return the fixed envelope from `unified_core::json`
  (`success` / `respond` / `error_response`); no handler returns any
  other envelope.
- `init(&AppContext)` runs after the core migrations (§1.2, step 6).
  A module that needs its own tables embeds its own Diesel
  migrations in its own crate and runs them from `init()` against
  the pool in `ctx.db` (an `Arc<RwLock<Option<DbPool>>>`; read the
  lock, clone the `Some` pool, run the module's embedded migrations).
  It must never run the core migrations and must never create tables
  outside its own migration set.
- Inter-module communication goes through the hook bus only. No
  module calls another module's code directly (this is the standing
  rule in `src/modules/mod.rs`: "Interconnection between modules
  happens through the hook/event bus — no module calls another
  module directly").
- Security rules from the project apply unchanged: nonce +
  capability checks on every route that needs them (the `role.check`
  hook pattern used by the built-in modules), sanitise input, escape
  output, no raw SQL outside Diesel, only allowlisted fetch hosts.

### 4.2 How the server discovers and registers it

There is **no runtime discovery, no config-file module list, and no
plugin directory scan**. Registration is compile-time, through one
documented call site:

**Registration point: `src/modules/mod.rs::register_all(registry:
&ModuleRegistry)`.** The follow-up adds a clearly marked section at
the end of that function, after all core modules (so external modules
can `require` core ids and dependency validation sees the whole
graph):

```rust
// External modules (see docs/WORKSPACE_AND_EXTERNAL_MODULES.md §4).
// Each line is one external crate's `register` function. An external
// module is compiled into the binary; adding one is a code change,
// a rebuild, and a restart — never a runtime install.
example_hello_module::register(registry);
```

Two more one-line changes complete the wiring:

1. Root `Cargo.toml`: `example-hello-module = { path =
   "crates/example-hello-module" }` under `[dependencies]`.
   Third-party crates outside this repo are added the same way, as a
   `path` dependency (crate kept in this repo) or a `git` /
   crates.io version dependency (crate kept elsewhere).
2. Nothing else. No `pub mod` line is needed in `src/modules/mod.rs`:
   an external crate is an external crate, not a module of the server
   crate. No CLI, HTTP, or config change is needed: routes, hooks,
   CLI commands, and middleware all flow through the `Module` trait
   methods the registry already calls (`configure_all_routes`,
   `register_all_hooks`, `register_all_cli`,
   `register_all_middleware`, `init_all`).

Why this shape and not discovery: the mechanism reuses the exact
path every one of the 137 built-in modules already takes, so the
server code that consumes modules (`do_start`, `run_server`,
`build_cli_registry`) needs zero changes, and the Extensions Manager,
dependency validation, product declarations, and route catalog all
treat an external module exactly like a built-in one — except
`is_core() == false`, which is what makes it disableable and
correctly reported.

### 4.3 Versioning and compatibility rules

- `unified-core` and the server share one workspace version
  (`version.workspace = true`, currently `0.1.0`). They cannot drift:
  the server always builds against the `unified-core` in the same
  workspace.
- While the major version is `0`, any minor bump of `unified-core`
  may break the API. External crates pin an exact compatible
  requirement (for example `unified-core = { version = "=0.1.0",
  path = "../unified-core" }` for a crate vendored next to the
  workspace, or the matching `version` requirement for a registry /
  git dependency). After `1.0`, normal semver applies: additive
  changes are minor, breaking changes are major.
- The `Module` trait is the compatibility surface. New trait methods
  **must** ship with a default implementation (the established
  pattern: `version`, `is_core`, `requires`, `requires_libraries`,
  `requires_external_libraries`, `available_as_product`,
  `product_category`, `register_hooks`, `register_cli`,
  `configure_routes`, `route_info`, `route_version_support`,
  `register_middleware`, `init` all have defaults today). A trait
  method without a default is a breaking change and requires a major
  bump. The trait must stay object-safe (`dyn Module`): no generic
  methods, no `Self` return type.
- An external crate may depend on: `unified-core`, any crates.io
  crate, and its own dependencies. It may **not** depend on: the
  `unified` binary crate (a binary crate cannot be depended on, and
  the dependency would be circular); another module crate's
  internals; `crate::http`, `crate::cli`, `crate::auth`,
  `crate::themes`, `crate::gateway` (none of these exist as
  libraries); or Pingora (the gateway stays server-only and
  feature-gated).
- `unified-core` itself never gains a dependency on the server crate,
  on any module crate, or on Pingora. If a future core addition would
  need one of those, the addition does not belong in `unified-core`.
- A module id, once released, is never reused for a different module,
  and a published module's route paths under `/api/v1/<id>/` are
  never silently moved. Renaming an id is a breaking change for
  entitlements, bookmarks, and API clients.

### 4.4 What is intentionally NOT supported

- **No dynamic `.so` / plugin loading.** Rust has no stable ABI:
  a shared library compiled with a different rustc version, different
  optimisation flags, or a different version of any shared dependency
  (including `std` internals such as `Arc` layout or the allocator)
  cannot be safely loaded into the running server. There is no
  `dlopen`-based module loading, no plugin directory, and no
  runtime install. Adding a module is a compile-time change: add the
  dependency, add one line to `register_all`, rebuild, restart.
  This is stated plainly because it is the decision most often
  re-litigated; the answer does not change without a stable Rust ABI.
- **No runtime route mounting.** Registering a module after startup
  cannot mount its routes: Actix routes are fixed when the server
  factory is built. The Extensions Manager's enable/disable state is
  advisory in memory (§1.2) and does not mount or unmount routes.
  Full dynamic unload (route rebuild without a restart) is a
  separate follow-up and is not a precondition for external modules.
- **No database access outside `unified_core::db`.** An external
  module must not open its own database pool, must not depend on the
  server crate's connection settings directly, and must not run raw
  SQL outside Diesel. It uses the pool from `AppContext.db`.
- **No direct calls into other modules.** See §4.1: the hook bus is
  the only inter-module channel.
- **No access to server internals.** `src/cli`, `src/http` (except
  through `unified_core::tls::SslStorage`, which is a shared handle,
  not server logic), `src/auth`, `src/themes`, `src/gateway`, and the
  monitor/daemon control paths are not available to external modules.

## 5. Implementation plan for the follow-up step

Preconditions: Phase 1 module splits have landed; the tree compiles
at the starting commit (`cargo check` passes before any move — if it
does not, fix that first and do not start the move on a red tree);
§6.1 (`sharing` not declared) is resolved. Work on a branch; no push
until the program's push-approval gate clears (the standing rule for
this repo).

### Step 0 — Baseline

```sh
cargo check --workspace   # must pass before the move starts
grep -rl 'crate::core::' src | wc -l
grep -rl 'crate::db::' src | wc -l
```

Record both counts. The follow-up re-runs them after the move: the
first must be `0` outside `crates/unified-core` (nothing in `src/`
may reference `crate::core::` any more), and the second must be `0`
in `src/` as well (all `crate::db::` uses become
`unified_core::db::`).

### Step 1 — Create the workspace

1. Edit root `Cargo.toml`: add the `[workspace]` table from §2.1
   (`members = [".", "crates/unified-core",
   "crates/example-hello-module"]`, `resolver = "2"`),
   `[workspace.package]`, and `[workspace.dependencies]` with the
   shared pins (same versions as the current `[dependencies]`).
2. Change the root `[package]` to `version.workspace = true`,
   `edition.workspace = true`, `rust-version.workspace = true`,
   `license.workspace = true`.
3. Add `[dependencies] unified-core = { path =
   "crates/unified-core" }` to the root package.
4. Verify: `cargo check -p unified` still passes (the workspace now
   exists; no code has moved yet).

### Step 2 — Create `crates/unified-core` (skeleton first)

1. Create `crates/unified-core/Cargo.toml`:
   `[package] name = "unified-core"`, `version.workspace = true`,
   `edition.workspace = true`, `rust-version.workspace = true`,
   `license.workspace = true`, `publish = false`, plus the
   dependencies from §3.5 inherited from the workspace
   (`actix-web.workspace = true`, and so on).
2. Create `crates/unified-core/src/lib.rs` with the module
   declarations and root re-exports from §3.2 — and no moved files
   yet. `cargo check -p unified-core` fails at this point (modules
   missing); that is expected and is fixed by step 3. Do not commit
   a red skeleton as a stopping point: steps 2–3 land together.

### Step 3 — Move the core, file by file

Move (with `git mv`, so history follows the files) in this order,
running `cargo check -p unified-core` after each group:

1. `src/core/error.rs` → `crates/unified-core/src/error.rs`.
   Rewrites inside the moved file: none (it imports only `serde_json`
   and `diesel`).
2. `src/core/hooks.rs` → `hooks.rs`; `src/core/json.rs` → `json.rs`;
   `src/core/pagination.rs` → `pagination.rs`;
   `src/core/reference_id.rs` → `reference_id.rs`. Rewrites: none
   except `crate::core::error` → `crate::error` (and `crate::core::hooks`
   → `crate::hooks` where present).
3. `src/core/config.rs` → `config.rs`. Rewrites:
   `crate::core::error` → `crate::error`,
   `crate::core::hardening` → `crate::hardening`,
   `crate::core::routes` → `crate::routes`,
   `crate::core::storage` → `crate::storage`. (In general, inside
   moved files: `crate::core::X` → `crate::X`.)
4. `src/core/cache.rs`, `storage.rs`, `routes.rs`, `tenancy.rs`,
   `hardening.rs`, `middleware.rs`, `sharing.rs` → same names.
   Rewrites: `crate::core::X` → `crate::X` throughout, and
   `crate::db::DbPool` → `crate::db::DbPool` (unchanged text: `db`
   is still `crate::db` inside `unified-core`). `sharing.rs` is
   declared in `lib.rs` from the start (§6.1).
5. `src/core/context.rs` → `context.rs`. Rewrites:
   `crate::core::cache` → `crate::cache`, `crate::core::config` →
   `crate::config`, `crate::core::error` → `crate::error`,
   `crate::core::hooks` → `crate::hooks`,
   `crate::core::modules` → `crate::modules`,
   `crate::core::routes` → `crate::routes`,
   `crate::core::storage` → `crate::storage`,
   `crate::db::DbPool` → unchanged,
   `crate::http::tls::SslStorage` → `crate::tls::SslStorage`.
6. `src/core/modules.rs` → `modules.rs`. Rewrites:
   `crate::core::context` → `crate::context`,
   `crate::core::error` → `crate::error`,
   `crate::core::hooks` → `crate::hooks`,
   `crate::core::middleware` → `crate::middleware`.
7. Delete `src/core/` and remove `mod core;` from `src/main.rs`.

### Step 4 — Move the database

Move `src/db/mod.rs`, `src/db/models.rs`, `src/db/schema.rs`
together into `crates/unified-core/src/db/` (same file names).
Rewrites inside the moved files:

- `crate::core::error` → `crate::error`,
  `crate::core::tenancy` → `crate::tenancy` (in `db/mod.rs`).
- `use crate::db::schema::*;` in `models.rs` stays **unchanged**:
  inside `unified-core`, `crate::db::schema` still resolves (`db/` is
  a module of this crate). The earlier draft of this document said to
  rewrite it to `use crate::schema::*;` — that was wrong, there is no
  top-level `schema` module in `unified-core`, and the mover caught it
  during implementation (2026-11-22).
- **Migrations path (resolved 2026-11-22, do not re-litigate):**
  `db/mod.rs` embeds with
  `embed_migrations!("migrations")`. The macro resolves the path
  relative to `CARGO_MANIFEST_DIR` — verified in the vendored
  `migrations_macros-2.2.0` source (`src/migrations.rs`:
  `env::var("CARGO_MANIFEST_DIR")` joined with the argument), i.e.
  relative to the manifest directory of the crate being compiled.
  The embedding crate is now `unified-core`, so the directory moved
  with the code: `migrations/` (108 migrations) now lives at
  `crates/unified-core/migrations/` and the macro argument stays
  `"migrations"` unchanged. The repository root has no `migrations/`
  directory any more. `db/mod.rs`'s header comment was updated to
  say so.
- Delete `src/db/` and remove `mod db;` from `src/main.rs`.

### Step 5 — Move the TLS storage

Move `src/http/tls.rs` → `crates/unified-core/src/tls.rs`. Rewrites:
`crate::core::config` → `crate::config`, `crate::core::error` →
`crate::error`. Remove the `tls` declaration from `src/http/mod.rs`
(verify its exact form in the follow-up; it was not read for this
document). Server-side uses of `crate::http::tls::SslStorage` (in
`src/http/server.rs`, `src/http/cert_swap.rs`, and the modules that
reference it — 10 uses as read) become
`unified_core::tls::SslStorage`.

### Step 6 — Rewire the server crate

Mechanical path rewrites across `src/` (everything except the moved
directories), then compile:

| Before | After |
| --- | --- |
| `crate::core::X` | `unified_core::X` |
| `crate::core::modules::...` | `unified_core::modules::...` |
| `crate::core::hooks::events::...` | `unified_core::hooks::events::...` |
| `crate::db::Y` | `unified_core::db::Y` |
| `crate::db::models::...` | `unified_core::db::models::...` |
| `crate::db::schema::...::dsl` | `unified_core::db::schema::...::dsl` |
| `crate::http::tls::SslStorage` | `unified_core::tls::SslStorage` |
| `crate::core::logging::init_logging` | stays `crate::...` — `logging.rs` stays in the server crate (§3.3). Move the file to `src/logging.rs` (do not keep a `src/core/` shim), update its one call site (`src/cli/commands.rs:341`), and leave its internal `crate::modules::logs::LogsLayer` path unchanged |

Suggested method: one `sed` pass per rewrite over `src/` (excluding
nothing — the moved files are already gone), then `cargo check -p
unified`, then fix the stragglers the compiler reports. Do not hand-edit
741 files one by one. Verify the rewrite is complete with:

```sh
grep -rn 'crate::core::' src/ | wc -l   # must be 0
grep -rn 'crate::db::' src/ | wc -l     # must be 0
grep -rn 'crate::http::tls' src/ | wc -l # must be 0
```

### Step 7 — Pilot: wire the example module (temporary)

1. Add `example-hello-module = { path =
   "crates/example-hello-module" }` to the root `[dependencies]`.
2. Add one line to `src/modules/mod.rs::register_all`, in the
   external-modules section at the end (§4.2):
   `example_hello_module::register(registry);`
3. `cargo check --workspace` and `cargo test --workspace`.
4. Boot smoke test: `unified -t` (config test), start the server,
   `GET /api/v1/hello` returns the success envelope, and the Modules
   API lists `hello` with `is_core: false`.
5. **Remove the `register_all` line again.** The example crate stays
   a workspace member (compiled by `cargo check --workspace`, so it
   cannot rot unnoticed) but is not a registered production module.
   Record the removal in the commit message so nobody re-adds it by
   accident.

### Step 8 — First real module crate (pilot exception)

Move the calculator module (`src/modules/calculator/` + the
`calculator` declaration in `src/modules/mod.rs`) into its own crate
(for example `crates/unified-calculator`), depending on
`unified-core` only, keeping its `pub fn register` + `CalculatorModule`
shape, and call its `register` from `register_all` in place of the
removed in-tree one. `cargo check --workspace`, `cargo test
--workspace`, boot smoke test (calculator routes answer). This is
the deliberate single-module-crate exception from §2.2; the next
module crates follow the bounded-context rule.

### Step 9 — Verification gate (all of it, in order)

```sh
cargo fmt --check
cargo check --workspace
cargo check -p unified-core
cargo check -p example-hello-module
cargo check --workspace --features gateway   # Pingora build still resolves
cargo test --workspace
```

`scripts/check.sh` exists (verified: the program's project-checking task landed it; run `bash scripts/check.sh` as the one-command gate, plus `bash scripts/check.sh --features gateway` for the Pingora build). The `cargo` commands above are the same gate in explicit form. Also run the boot smoke test from step 7
(`unified -t`, start, `GET /api/v1/hello` before its removal) and
confirm `grep -rn 'crate::core::' src/` and `grep -rn 'crate::db::'
src/` are both empty. Only when every gate passes is the work ready
to report for push approval — no push happens inside this plan.

### Implementation notes (2026-11-22)

What the mover did, and where it deviated from the plan above:

- **Baseline (pre-move):** 6,062 `crate::core::` uses in 1,306 files,
  249 `crate::db::` uses in 78 files, 36 `crate::http::tls` uses,
  1,621 `.rs` files under `src/`. After the move: `grep -rn
  'crate::core::' src/`, `grep -rn 'crate::db::' src/`, and `grep -rn
  'crate::http::tls' src/` are all **0**. 1,294 files under `src/`
  were rewritten by scripted passes; no file was hand-edited for the
  path rewrites. A pre-move backup tarball is at
  `/tmp/unified-pre-workspace-backup.tar.gz` (ephemeral; not in the
  repo).
- **No `git mv`:** the staging tree is not a git repository, so plain
  `mv` was used; history following is unavailable here. No push was
  made.
- **No `cargo check` / `cargo test` was run by the mover** (per the
  task: a separate agent owns compilation verification). Verification
  was by read-back and the grep counts above. `cargo check
  --workspace`, `cargo test --workspace`, and the boot smoke test are
  still open and belong to the compilation-verification agent.
- **Step 1:** `[workspace.dependencies]` copies every pin from the
  root `[dependencies]` verbatim (including optional `pingora`);
  `x509-certificate = "0.25"` and reqwest feature
  `"rustls-tls-webpki-roots"` preserved. The root `[dependencies]`
  were left as direct version pins (not converted to
  `.workspace = true`); only the new members inherit from the
  workspace table.
- **Step 3:** `sharing` was already declared in `src/core/mod.rs`
  (§6.1 was fixed before this move), and is declared in
  `crates/unified-core/src/lib.rs` from the start.
- **Step 4:** migrations moved to `crates/unified-core/migrations/`
  (§6.2). The doc's `use crate::schema::*;` rewrite for `models.rs`
  was **not** applied — it was wrong (no top-level `schema` module
  exists in `unified-core`); `use crate::db::schema::*;` stays.
- **Step 5:** `tls.rs` exports more than the §3.1 table lists
  (`server_config`, `validate_pem_pair` in addition to `SslStorage`
  and `to_ascii_name`); the whole file moved, so all of them moved.
  Server-side uses in `src/http/acme.rs`, `cert_swap.rs`, and
  `server.rs` were rewritten by the step 6 passes.
- **Step 6:** `crate::core::logging::init_logging` was rewritten to
  `crate::logging::init_logging` (not `unified_core::logging`) before
  the general pass. Stale file-path mentions in comments
  (`src/core/...`, `src/db/...`) were updated to
  `crates/unified-core/src/...`; doc comments inside moved files that
  named server-side paths (`crate::modules::licensing`, `crate::http::server`,
  `crate::http::cert_swap`, ...) were reworded to plain prose so no
  dangling intra-doc link remains in the new crate.
- **`unified-core` dependencies:** §3.5 plus `log` and `walkdir`
  (both actually used by the moved files: `log::warn!` in
  `cache.rs`/`context.rs`/`hardening.rs`, `walkdir::WalkDir` in
  `cache.rs`/`storage.rs`). No `tracing`, `parking_lot`, `semver`,
  `bytes`, `mime`, `nix`, `zip`, or `tar` dependency: the only
  `tracing` use was `logging.rs`, which stays server-side.
- **Step 7 pilot:** wired (`example-hello-module` dep +
  `example_hello_module::register(registry);`), verified by
  read-back, then removed again. The root dependency was removed
  together with the register line — with no registration call it
  would be an unused dependency of the binary; workspace membership
  is what keeps the example compiling. The external-modules section
  comment stays in `register_all` as the landing place. No boot
  smoke test was run (no toolchain work by the mover).
- **Step 8:** calculator crate is `crates/unified-calculator`
  (`lib.rs` + 7 files, `unified-core` + actix-web/chrono/dashmap/
  serde/serde_json/uuid). `register_all` calls
  `unified_calculator::register(registry);` in the same
  business-operations position. Its long-term bounded-context home
  is still undecided (per §2.2 it is the deliberate
  single-module-crate exception).
- **§6.3:** duplicate-id panic in `ModuleRegistry::register` (see
  §6.3 resolution).
- **Counts as-read (§6.5):** the §1.3/§3 counts were not re-measured
  file by file; the baseline greps above are the re-measured anchor.

## 6. Open issues and risks (for the follow-up, not this document)

### 6.1 `src/core/sharing.rs` is not declared

`src/core/mod.rs` declared 15 modules; `sharing` was not among them,
yet `src/modules/auditors.rs:156` (`use crate::core::sharing::{...}`)
and `src/modules/network_tools/tools_tls.rs:6` (`use
crate::core::sharing;`) import it — the tree could not compile with
those imports unresolvable. Verified by the coordinator after this
document was written, and fixed with one line (`pub mod sharing;`
added to `src/core/mod.rs`, in alphabetical position). This document
counts `sharing.rs` as part of the `unified-core` surface either way,
and `lib.rs` in §3.2 declares it.

### 6.2 Migrations directory placement

Resolved 2026-11-22 (see step 4): `embed_migrations!` resolves
relative to `CARGO_MANIFEST_DIR`, so `migrations/` moved into
`crates/unified-core/migrations/` with the macro argument unchanged.

### 6.3 Duplicate module ids are not rejected today

`ModuleRegistry::register` pushes unconditionally and `get` returns
the first match. A duplicate id (built-in vs external, or external
vs external) silently shadows. The follow-up should add a
duplicate-id check — in `register` (fail startup loudly) or in
`validate_dependencies` (report it like a missing dependency) —
before the first real external module ships. This is hardening, not
part of the move, and must not block steps 1–7.

**Resolved 2026-11-22:** the check landed in `register`
(`crates/unified-core/src/modules.rs`): registering a module whose
id is already present panics, naming the duplicate id and both
module names. `validate_dependencies` was **not** chosen: it returns
a report that nothing at startup calls today, so a report-based
check could be silently ignored, while the panic fires during
`register_all`, before any route, hook, or migration runs.

### 6.4 `logging.rs` stays behind — deliberately

`init_logging` references `crate::modules::logs::LogsLayer`, so it
cannot move into `unified-core` without creating a crate cycle. It
stays in the server crate (§3.3). If a future core change needs a
logging hook in `unified-core`, it must be a trait or callback
supplied by the server, not a reference to a module.

### 6.5 Counts are as-read

All counts in §1.3 and §3 (741 files, 45 files, 137 modules, 250
items, 1,344 structs, 678 macros) were read on 2026-11-22 while
Phase 1 builders were actively adding files. Re-run the greps in
step 0; if a count moved, update this document in the same commit
as the move.
