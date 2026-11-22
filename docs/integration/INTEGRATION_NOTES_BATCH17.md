# INTEGRATION NOTES — Batch 17: Tenant Isolation (schema-per-tenant)

Coordinator-verified by read-back. Nothing pushed. Remote head untouched (`7cf6853c`).
Shared files untouched by this batch: `src/modules/mod.rs`, `src/db/schema.rs`,
`src/db/models.rs`, `README.md`, `TODO.md` contain zero batch-17 references
(verified by grep).

## 1. Files added / changed

| File | Lines | What |
|---|---|---|
| `src/core/tenancy.rs` | 445 (new) | Single source of truth: `schema_name()`, validation, `TenantContext` extractor (fail-closed), `ControlPlaneGuard`, transaction-scoped `SET LOCAL search_path`, `with_tenant_connection`, per-schema migration runner, `drop_tenant_schema`, hook event constants, `TENANT_SCOPE_CONTRACT` |
| `src/core/mod.rs` | 21 (was 20) | Only change: one added line `pub mod tenancy;` |
| `src/db/mod.rs` | 134 (was 48; +86, additive) | `run_migrations_in_schema(pool, schema)`, `schema_exists(pool, schema)`, `drop_tenant_schema(pool, schema)`; `run_migrations` byte-identical |
| `src/modules/tenants.rs` | 2,443 (new) | Tenants module: lifecycle, idempotent provisioning, entitlement-gated module grants, `tenant.lookup` answerer, admin settings, 14 routes |
| `src/modules/tenant_destroy.rs` | 2,386 (new) | Destroy flow: export-first, typed confirmation, second approver, suspension + grace, verified DROP SCHEMA CASCADE, external-cleanup tracking, 13 routes |
| `migrations/2026-11-22-000094_tenant_isolation/` | up.sql 131, down.sql 12 | Control-plane tables in `public` only: `tenants`, `tenant_provisioning_steps`, `tenant_module_grants` (3 CREATE = 3 DROP) |

Totals: **~5,539 lines**, **27 routes** (14 tenants + 13 destroy), **3 tables**,
**1 migration (000094)**. Migration 000093 was taken mid-task by
`000093_product_subdomains` (batch 15); B2 correctly used the next free number
and renumbered nothing. Current duplicate list (unchanged by this batch):
000056, 000070, 000075, 000077, 000080, 000090 (x2 each) + `000100_calculator`
out of sequence. 105 migration dirs at verification.

## 2. The design decision (do not re-litigate)

Schema-per-tenant. Tenant id is a uuidv7 (time-ordered). Schema name is
DERIVED through exactly one helper, `tenancy::schema_name(tenant_id)`:
`tenant_` + the uuidv7's simple hex (32 lowercase hex chars, no hyphens).
Control plane (`tenants`, `subscriptions`, `domains`, `billing`, `licensing`)
stays in `public`; tenant business data lives in the tenant's schema.
Destroy = DROP SCHEMA CASCADE after export + confirmation + grace.
Database-per-tenant was rejected (own pool per DB, N migration runs).

## 3. CRITICAL FIX 1 — B2's seam does NOT match B1's real API (must fix before compile)

B2 wrote `src/modules/tenants.rs` before `src/core/tenancy.rs` existed and
assumed signatures that B1 did not build. All of B2's tenancy calls are
isolated in the seam functions at the bottom of `tenants.rs`
(`tenant_schema_name`, `tenant_context_for`, `tenant_context_schema`,
`ensure_tenant_schema`, `migrate_tenant_schema`, ~lines 2415-2443) plus the
`use` at line 225. Rewrite ONLY the seam:

- `TenantContext::new(tenant_id, schema)` — DOES NOT EXIST. B1's
  `TenantContext` has pub fields `{ tenant_id, schema_name, company_id,
  site_id }` and constructors `from_resolved(&ResolvedTenantContext)` /
  `from_http_request(&HttpRequest)`. Fix: build the struct literal directly
  (`TenantContext { tenant_id, schema_name: schema_name(tenant_id),
  company_id: None, site_id: None }`).
- `context.schema()` — DOES NOT EXIST. The field is `context.schema_name`.
- `create_tenant_schema(pool, schema)` — DOES NOT EXIST. B1 folds schema
  creation into `run_tenant_migrations` (`CREATE SCHEMA IF NOT EXISTS`
  first). Fix: delete `ensure_tenant_schema` and let the migrate step call
  B1's runner (or add a `create_tenant_schema` to `tenancy.rs` — either way,
  exactly one helper owns DDL).
- `run_tenant_migrations(pool, schema: &str)` — B1's signature is
  `run_tenant_migrations(pool: &DbPool, tenant_id: Uuid)`. Fix the call to
  pass the uuidv7, not the schema string.
- Line 225 import must drop `create_tenant_schema` (or keep it only if you
  add it to `tenancy.rs`).

## 4. CRITICAL FIX 2 — migration CHECK rejects B1's real schema names

`migrations/2026-11-22-000094_tenant_isolation/up.sql` line 91:

```sql
CHECK (schema_name ~ '^tenant_[0-9a-f_]{36}$')
```

This assumes the `tenant_<uuidv7-underscored>` format (36 chars after the
prefix). B1's actual format is `tenant_` + 32 simple-hex chars, so EVERY
real tenant row would FAIL this CHECK. Fix before running the migration:

```sql
CHECK (schema_name ~ '^tenant_[0-9a-f]{32}$')
```

(must match `tenancy::is_valid_schema_name`, `^tenant_[0-9a-f]{32}$`).

## 5. Registration (integrator)

```rust
// src/modules/mod.rs — add (alphabetical position per existing order):
pub mod tenant_destroy;
pub mod tenants;
```

```rust
// Module registration — the modules' own register() fns exist in each file:
//   tenants::register(registry)        // 14 routes, available_as_product = false
//   tenant_destroy::register(registry) // 13 routes under the same /api/v1/tenants prefix
```

`src/core/mod.rs` already contains `pub mod tenancy;` (added by B1).

## 6. schema.rs / models.rs

B2's Diesel `table!` snippets for `tenants`, `tenant_provisioning_steps`,
`tenant_module_grants` are in the header doc comment of
`src/modules/tenants.rs` — copy them from there (coordinator did not
re-verify the snippet text; check it against `up.sql` when wiring).
B3's `tenant_destroy_requests` + `destroyed_tenants` table DDL is in B3's
report §OPEN-8 — add those two tables to B2's migration (this module
creates no second migration) with matching `table!` entries. In-memory
DashMap stores are runtime-authoritative this round (per the
licensing/packages precedent); Diesel persistence wiring is integrator work.

## 7. Hook contracts (wire strings — verified present)

Emitted by tenants module: `tenant.created`, `tenant.provisioned`,
`tenant.provision_failed`, `tenant.suspended`, `tenant.resumed`,
`tenant.module.granted`, `tenant.module.skipped`, `tenant.module.revoked`,
`tenant.deprovisioning_started`, `tenant.deleted`, `tenant.settings.updated`.
Emitted by destroy flow: `tenant.destroy_requested` (via request),
`tenant.suspended`, `tenant.resumed`, `tenant.destroyed`,
`tenant.destroy.failed`, `tenant.storage_cleanup_requested`,
`tenant.backup_cleanup_requested`, `tenant.<area>_cleanup.completed`
(area = storage/backups/search_index).
Answered: `tenant.lookup` (tenants module — B3 resolves tenants ONLY through
it); `tenant.resolve_requested` is NOT answered here — saas_domains already
owns it (`resolve_host`); do not register a second answerer.
Scheduler kind: `tenant.destroy_due` — arm a dispatch calling
`tenant_destroy::process_due_destroys(ctx)` (the hook handler only lists due
requests; hook handlers have no AppContext and never drop anything).
Add to `AUDITED_EVENTS` in `audit.rs`: `tenant.created`,
`tenant.provisioned`, `tenant.provision_failed`, `tenant.suspended`,
`tenant.resumed`, `tenant.destroy.*`, `tenant.destroyed`,
`tenant.module.granted`, `tenant.module.skipped`, `tenant.module.revoked`.

## 8. Convergence gaps the integrator must close (all fail closed today)

1. **Subdomain claim declined today.** B2 emits `site.provision_requested`
   with honest `source: "tenants"` and no product_id; saas_domains' handler
   only trusts `source == "saas_packages"` and only claims *product*
   subdomains (requires product_id). The step records the reason as
   `skipped` and the tenant proceeds with `subdomain_pending: true`
   (retryable). Durable fix: saas_domains must accept a tenant-level claim
   from source `tenants` (generic allocation, not a product subdomain), or
   wire tenant claims through the saas_domains allocation flow.
2. **Preset module lists:** presets exposes no `modules_requested` event, so
   a `preset_id` alone grants no modules (recorded as a skip reason).
   Durable fix: presets answers `presets.preset.modules_requested`, or
   preset application runs before provisioning.
3. **B3 <-> B2 single source of truth.** B3 does NOT call B2's public
   helpers `begin_deprovision` / `set_export_reference` /
   `finish_deprovision_as_deleted` and keeps its own `DestroyedTenant`
   store. Decide one source of truth for the `deprovisioning -> deleted`
   transition (B2's helpers move the tenant record; B3 must call them or
   the record and B3's store drift) and share one `TenantsState` via
   `TenantsModule::with_state` / `new_state()`.
4. **B2 needs from B3/B1:** B3's destroy flow needs B2 to answer
   `tenant.lookup` with `{id, slug, status, created_at (required),
   data_size_bytes (optional), destroyed_at, export_reference}` (NOW
   ANSWERED — verified present in tenants.rs) and to listen to
   `tenant.suspended` (flip to `suspended`), `tenant.resumed`, and
   `tenant.soft_delete_requested` (keep row with `destroyed_at` + export
   reference, status `deleted`). Verify these listeners exist in B2's
   `register_hooks` — B3's report lists them as required; the coordinator
   did not re-verify each listener by name.
5. **Import/export:** no listener turns `export.requested` (scope
   `full_tenant_export`) into a job, and `export.lookup` has no answerer.
   Working path until then: admin creates the export via
   `POST /api/v1/import-export/exports` and attaches the job id via the
   export-proof route; completion observed via `export.completed`.
6. **saas_domains:** no listener for
   `saas_domains.subdomain.release_requested` (its release flow is
   HTTP-driven). Revoke is recorded; an admin releases subdomains
   manually. Not a destroy blocker (serving already stopped at
   suspension).
7. **Storage/backup/search:** no listeners for the three
   `tenant.<area>_cleanup_requested` events and no `.completed` emitters —
   destroyed tenants wait in `destroyed_pending_external_cleanup` until an
   admin confirms each item or those modules add listeners. Still needing a
   separate job: stored files/objects, backups, search-index entries,
   cached keys (cache invalidation is not emitted by this flow).
8. **Entitlement source of truth:** B1 notes no entitlement store/API
   exists yet in this codebase — the check is specified as resolved at
   request time against the subscription/package record via the
   subscriptions/tenants module or its hook events. Until wired, gated
   features fail closed per the contract. B2's provisioning calls
   `licensing::check_module_entitlement` (exists, verified) — that is the
   current entitlement path.
9. **Doc-note tension (flagged, not hidden):** B1 documents the drop helper
   as "never called from a request handler". B3 routes the manual `execute`
   endpoint through the same audited `run_final_destroy` core as the
   scheduler sweep, with identical gates. If strict compliance is wanted,
   the manual endpoint should only enqueue `tenant.destroy_due`.

## 9. The tenant-scoped data access contract (standing rule — THE convergence contract)

Stated verbatim in `src/core/tenancy.rs` as `TENANT_SCOPE_CONTRACT` and in
the module header: every module resolves tenant business data through the
tenant context (uuidv7 tenant id, derived tenant schema, package
entitlements) via `TenantContext` + `with_tenant_connection` + an
entitlement check. No module queries the control-plane database directly
for tenant business data. No tenant id, schema name, or package id is
hard-coded; all are resolved at request time from the tenant context. No
tenant context, no entitlement answer, or no tenant schema means the
request is rejected: fail closed, never fall back to the `public` schema.

Convergence checklist for other batches (until they adopt it: fail closed):
1. Extract `tenancy::TenantContext` in every tenant handler (never read the
   middleware context directly for DB work, never default a tenant).
2. Route ALL tenant reads/writes through `with_tenant_connection` (never a
   raw pooled connection for tenant data, never `SET search_path` by hand).
3. Check package entitlements at request time before serving module data;
   no entitlement answer → reject.
4. Control-plane handlers extract `ControlPlaneGuard`, never
   `TenantContext`; tenant handlers never touch `public` tables except
   qualified global lookups.
5. Record any unwired point in that module's integration notes — the request
   still fails closed meanwhile. No other module is claimed to follow this
   contract yet.

## 10. Public-vs-tenant table split (rule; integrator enforces the list)

- `public` (control plane): tenants, subscriptions, domains, billing,
  licensing records. Accessed only through their own modules/events.
- Per-tenant schema: all tenant business data (every module's tables).
- Shared global lookups that must stay global (provider registries, country
  lists) are read from `public` via qualified names — the table list is
  integrator work; B1 states the rule only.

## 11. Naming collision warning (integrator)

Two types named `TenantContext` exist: `middleware::TenantContext` (string
ids, opaque) and `tenancy::TenantContext` (parsed uuidv7 + schema). B1
aliases the middleware one as `ResolvedTenantContext` inside tenancy.rs.
Import both with explicit aliases everywhere; never `use` both unaliased in
one file. Also: `middleware::TenantContext.tenant_id` is only validated as
alphanumeric/`-`/`_`; any non-UUID tenant id the domains module ever emits
will be rejected by the tenancy extractor (403) by design — domains/tenants
modules must only ever issue uuidv7 ids.

## 12. What the coordinator verified (read-back, not trusting reports)

- Line counts match every builder report exactly (tenancy 445, tenants
  2,443, tenant_destroy 2,386, db/mod.rs 134, up.sql 131).
- Greps over all batch-17 files: 0 forbidden reference-project name hits,
  0 `todo!`/`unimplemented!`/`unreachable!`, 0 Indonesian word-boundary
  hits (B1/B2/B3 scans; coordinator re-ran stub + forbidden-name greps).
- `schema_name()` is the only place a `tenant_...` name is built
  (SCHEMA_PREFIX const + the one `format!` in `schema_name()`); every DDL
  `format!` interpolates a name already validated by
  `is_valid_schema_name()`; `schema_exists` uses a bound parameter.
- 0 DDL strings (`CREATE/DROP SCHEMA`, `CREATE TABLE`) in tenants.rs and
  tenant_destroy.rs — all DDL goes through tenancy helpers.
- 14 `.route(` + 14 `require_permission` (+1 def) in tenants.rs; 13
  `.route(` + 13 `require_permission` (+1 def) in tenant_destroy.rs.
- Migration: 3 CREATE TABLE = 3 DROP TABLE (the 4th "DROP TABLE" grep hit
  is a comment line in down.sql). 000094 is single; 000093 is
  `000093_product_subdomains` (batch 15), B2's empty 000093 dir is gone.
- B3 uses B1's REAL API (`schema_name`, `schema_exists`,
  `drop_tenant_schema`, `EVENT_TENANT_*` constants) — verified present.
- B2's seam mismatch (§3) and the migration CHECK bug (§4) were found by
  the coordinator's read-back, not by the builders — both are integrator
  MUST-FIX items before compile/migration.

## 13. OPEN list (explicit)

1. `cargo check` / `cargo build` NEVER ran (no toolchain, crates.io
   blocked). Highest-risk compile points: Diesel `transaction()` inference
   with `UnifiedError` in `with_tenant_connection` /
   `run_migrations_in_schema`; `run_pending_migrations` inside an outer
   transaction (relies on Diesel nesting migrations as savepoints so
   `SET LOCAL` survives); B2's seam fixes (§3).
2. No migration has ever run against live PostgreSQL. Live proof needed:
   provision a tenant schema, run `run_tenant_migrations`, confirm
   `__diesel_schema_migrations` lands in the tenant schema (not `public`),
   confirm a returned pool connection has the default search path again,
   confirm `schema_exists` / `drop_tenant_schema` behave, and confirm the
   corrected CHECK (§4) accepts B1-generated names.
3. §3 + §4 MUST-FIX before anything compiles or migrates.
4. §8 convergence gaps (subdomain claim, preset lists, B3↔B2 helpers,
   export listener, saas_domains release, storage/backup/search cleanup
   listeners, entitlement source of truth).
5. In-memory DashMap stores are runtime-authoritative in tenants.rs and
   tenant_destroy.rs; Diesel persistence wiring is integrator work.
6. Tenant create endpoint is synchronous this round (returns the tenant as
   `active` or `provision_failed`); `GET /{id}/provision` is the polling
   contract for async runtimes.
7. No live HTTP test of any endpoint, gate, or migration.
8. Remaining hard-coded spots (honest): status vocabularies, capability
   names, event names, `SCHEMA_PREFIX`/`CONTROL_PLANE_SCHEMA`/validation
   regex (standard constants), configurable defaults in
   `TenantsSettings`/`TenantDestroySettings` (reserved slugs, slug bounds,
   grace defaults, second-approval thresholds — editable via settings,
   not fixed rules).
