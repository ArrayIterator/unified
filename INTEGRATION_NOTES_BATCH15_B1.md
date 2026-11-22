# Integration Notes — Batch 15, Builder B1: Per-Product Feature Limiting (Entitlements)

- Module file: `src/modules/entitlements.rs` — **3,635 lines**
- Migration: `migrations/2026-11-22-000092_entitlements/` — `up.sql` 164 lines, `down.sql` 12 lines
- Routes: **20** (all under `/api/v1/entitlements`, all capability-checked)
- Tables: **6** in `up.sql`, **6** `DROP TABLE` in `down.sql` (counts match)
- Capabilities: `entitlements:read`, `entitlements:manage`, `entitlements:grant`
- Module id: `entitlements`, `available_as_product = false` (infrastructure, not sellable)
- Nothing was pushed. No files outside the two paths above were edited.
  `src/modules/mod.rs`, `src/db/schema.rs`, `src/db/models.rs`, `README.md`,
  `TODO.md`, `src/modules/licensing.rs`, `src/modules/extensions.rs`,
  `src/core/modules.rs`, and `LICENSE` were NOT touched (snippets below are
  for the integrator to apply).

## 1. What it does

An entitlement is one row granting one module to one tenant (company)
from one admin-defined source. The effective entitlement for a module is
the UNION of its active sources: any active source grants it. No
entitlement record means not entitled (fail closed).

- Source kinds (closed taxonomy): `license`, `subscription`,
  `saas_package`, `manual_grant`, `owner_entitlement` (implicit owner
  entitlement for a sellable core module — see §7).
- `GET /api/v1/entitlements/my-modules` — every registered module with
  the calling tenant's state (`entitled` / `grace_read_only` / `locked`),
  sources + expiry, and — when locked — the upgrade path resolved from
  product/package data through the hook bus, never a hard-coded map.
  When no provider answers, the upgrade path is null with reason
  `no_upgrade_path_provider`; never a fabricated product.
- `POST /api/v1/entitlements/modules/{module_id}/enable` — server-side
  entitlement check FIRST. Not entitled → 403 with metadata
  `error_code: "entitlement.required"`, the attempt recorded in
  `entitlement_denied_attempts`, and `entitlement.enable_denied` +
  `module.entitlement.denied` emitted. Entitled → activation recorded
  and `module.enabled` fired so the Extensions Manager path does the
  actual activation (this module is the gate, not a second activator).
- `POST /api/v1/entitlements/package-changes/apply` — given a new
  package/product set, computes newly entitled / kept / kept-via-other-
  source / `requires_confirmation`. Newly entitled modules are enabled;
  a module that would be lost is NEVER silently disabled when the tenant
  uses it outside the package (usage is asked through the
  `entitlement.module_usage_requested` hook; no answer = treated as
  used, fail-safe). `POST /package-changes/{id}/confirm` disables only
  the explicitly confirmed modules (or discards the change).
- Downgrade/expiry: a lapsed module first becomes READ-ONLY for a
  configurable grace period (default 14 days; export still allowed,
  writes rejected with `entitlement.grace_read_only`), then DISABLED.
  **Tenant data is NEVER deleted by this flow** — there is no delete
  call anywhere in the lapse/sweep path, and the only DELETE in the
  module is the admin-only manual-grant row delete (history is kept).
- Admin: manual grants CRUD (reason required), list per tenant
  (paginated, filtered by company/module/source/status), history per
  tenant/module, denied attempts, settings (grace days, read-only grace
  on/off, default deny), and a `POST /sweep` lapse/grace sweep.
- AI: `POST /ai/suggest` suggests which package unlocks the modules the
  tenant tried to use most (from the denied-attempt log) — a suggestion
  only, never a purchase. See §8 for the AI-contract gap.

## 2. Tenant-scoped data access (standing rule — implemented)

- Every tenant-scoped endpoint resolves its tenant ONLY through
  `TenantScope::resolve`, backed by the canonical
  `crate::core::tenancy::TenantContext::from_http_request(req)` (see
  §6 for what was verified on disk). `tenant_id` is the uuidv7
  namespace, `schema_name` is the derived name the context already
  carries (never built inline here), `company_id` is the row key (the
  tenants module uses the tenant id itself as the company id).
- **Fail closed:** no tenant context → **403 `tenant.required`**
  (metadata `error_code: "tenant.required"`), no data — never a silent
  fall back to `public`, never another tenant's modules. A requested
  `company_id` differing from the context's company is also 403. A
  resolved context with no company id is refused (rows are keyed by
  company). Note: the tenancy extractor itself returns 401 for a
  missing context; this module maps that to 403 per the standing rule
  for this endpoint family.
- No tenant id, schema name, or package id is hard-coded anywhere in
  the module (verified by grep; the only fixed strings are the
  `tenant_` prefix usage inside `tenancy::schema_name`, which this
  module does not duplicate).
- Control-plane sources (licenses, subscriptions, SaaS packages) are
  read ONLY through their modules' events/APIs
  (`licensing::has_module_entitlement`,
  `licensing::check_module_entitlement` as composed in `effective()`,
  `license.revoked`, `subscription.cancelled` / `.expired`,
  `saas_packages.cancelled`, `tenant.module.granted` / `.revoked`,
  `entitlements.grant_requested`). This module references NO
  control-plane table: no `schema::licenses`, `schema::subscriptions`,
  `schema::store*`, `schema::companies`, no `sql_query`, no raw SQL
  (verified by grep: 0 matches).
- Per-tenant activation state IS tenant business data: every
  entitlement / history / denied-attempt / package-change / activation
  row carries `tenant_id` (uuidv7) alongside `company_id`, is written
  only under a resolved tenant scope, and its durable tables are
  **tenant-schema tables** — created per tenant schema by the migration
  runner with the search_path set to that tenant's schema, never
  `public` tables. See the migration header comment and §4.
- Admin endpoints (`GET /entitlements`, grants, history,
  denied-attempts, settings, sweep) run on the control plane under
  `entitlements:manage` / `entitlements:grant`; they take no tenant
  scope from a request and name the tenant explicitly in the filter or
  body. They are the control-plane admin surface for this module's own
  data, not a tenant-data read path.

## 3. `src/modules/mod.rs` — exact snippets for the integrator

Insert the module declaration in alphabetical order (after line 37):

```rust
pub mod email_templates;
pub mod entitlements;
pub mod extensions;
```

Insert the registration after `licensing::register(registry);`
(line 142, commerce section — `roles`, `extensions`, and `licensing`
all register before it, satisfying `requires`):

```rust
    licensing::register(registry);
    entitlements::register(registry);
    software::register(registry);
```

`requires()` is `&["licensing", "extensions", "roles"]` — all three are
registered in `mod.rs` today. `saas_packages` and `tenants` are
deliberately NOT in `requires` (neither is registered in `mod.rs`;
all wiring to them is events-only).

## 4. `src/db/schema.rs` / `src/db/models.rs` — snippets (tenant-schema tables)

Placement: these tables live in the TENANT schema, not `public`.
They are applied per tenant schema automatically: migrations are
embedded with `embed_migrations!("migrations")` in `src/db/mod.rs`,
and `tenancy::run_tenant_migrations` runs the same embedded set
inside each tenant schema. No per-module registration is needed for
the migration itself. The Diesel wiring below is for the durable
read/write path that replaces the in-memory stores at integration
time (see §9). No foreign keys: `company_id` / `tenant_id` /
`source_id` / `module_id` are plain text on purpose (tenants,
companies, and modules live in other modules; ids are validated
against the live registry at write time, never by the DB) — the
school-fees precedent.

### schema.rs — append these `table!` blocks (and add the table names to the `allow_tables_to_appear_in_same_query!` list; no `joinable!` entries — no FKs)

```rust
diesel::table! {
    entitlements (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        source_kind -> Varchar,
        source_id -> Varchar,
        source_ref -> Nullable<Varchar>,
        module_id -> Varchar,
        status -> Varchar,
        granted_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        granted_by -> Nullable<Varchar>,
        reason -> Nullable<Varchar>,
        grace_started_at -> Nullable<Timestamptz>,
        grace_ends_at -> Nullable<Timestamptz>,
        lapsed_at -> Nullable<Timestamptz>,
        lapse_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_history (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        entitlement_id -> Nullable<Uuid>,
        company_id -> Varchar,
        module_id -> Varchar,
        action -> Varchar,
        actor_id -> Nullable<Varchar>,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_denied_attempts (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        module_id -> Varchar,
        actor_id -> Nullable<Varchar>,
        reason -> Varchar,
        error_code -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_package_changes (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        source_kind -> Varchar,
        source_id -> Varchar,
        new_module_ids -> Jsonb,
        newly_entitled -> Jsonb,
        kept -> Jsonb,
        kept_via_other_source -> Jsonb,
        requires_confirmation -> Jsonb,
        disabled_module_ids -> Jsonb,
        status -> Varchar,
        requested_by -> Nullable<Varchar>,
        decided_by -> Nullable<Varchar>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_settings (id) {
        id -> Varchar,
        grace_period_days -> Int8,
        read_only_grace_enabled -> Bool,
        default_deny -> Bool,
        updated_by -> Nullable<Varchar>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_module_activations (company_id, module_id) {
        tenant_id -> Varchar,
        company_id -> Varchar,
        module_id -> Varchar,
        enabled -> Bool,
        enabled_at -> Timestamptz,
        enabled_by -> Nullable<Varchar>,
        source_kind -> Nullable<Varchar>,
        source_id -> Nullable<Varchar>,
        updated_at -> Timestamptz,
    }
}
```

### models.rs — append (field-for-field mirrors of the structs in `entitlements.rs`)

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlements)]
pub struct EntitlementRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub reason: Option<String>,
    pub grace_started_at: Option<DateTime<Utc>>,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub lapsed_at: Option<DateTime<Utc>>,
    pub lapse_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlements)]
pub struct NewEntitlementRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub reason: Option<String>,
}

// Same shape for the other five tables, following the migration
// columns exactly:
//   EntitlementHistoryRow / NewEntitlementHistoryRow
//     (entitlement_history; `detail` is serde_json::Value)
//   EntitlementDeniedAttemptRow / NewEntitlementDeniedAttemptRow
//     (entitlement_denied_attempts)
//   EntitlementPackageChangeRow / NewEntitlementPackageChangeRow
//     (entitlement_package_changes; the six module-list columns
//     are serde_json::Value arrays)
//   EntitlementSettingsRow (entitlement_settings; single-row table,
//     `id` is the constant settings key)
//   EntitlementModuleActivationRow (entitlement_module_activations;
//     composite key (company_id, module_id))
```

Durable-access rule for the integrator: every Diesel read/write of
these tables goes through `tenancy::with_tenant_connection` (checkout,
transaction, `SET LOCAL` search_path, closure, commit) with the
tenant id from the request's `TenantContext` — never a plain pooled
connection, never `public`. The in-memory DashMap stores in the module
are authoritative this round (licensing / SaaS-packages precedent)
and are replaced by that wiring; a restart currently loses grants
made since the last durable write.

## 5. Hook contracts

Emitted (all on the hook bus; payloads in `src/modules/entitlements.rs`):

| Event | Payload |
|---|---|
| `entitlement.granted` | `company_id, module_id, source_kind, source_id, entitlement_id, expires_at` |
| `entitlement.revoked` | `company_id, module_id, source_kind, source_id, entitlement_id, reason` |
| `entitlement.grace_started` | `company_id, module_id, entitlement_id, grace_ends_at, reason` |
| `entitlement.disabled` | `company_id, module_id, entitlement_id, reason` |
| `entitlement.enable_denied` | `company_id, module_id, actor_id, reason, error_code` (`entitlement.required`) |
| `module.enabled` / `module.disabled` (core events) | `id, company_id, reason` — fired when this module enables/disables a module for a tenant |
| `entitlement.upgrade_path_requested` (query) | `{company_id, module_id, upgrade_paths: []}` — a handler sets `upgrade_paths` to `[{kind, id, name}]` (`kind` is `package`, `product`, or `license`) |
| `entitlement.module_usage_requested` (query) | `{company_id, module_id, used_outside_package: false}` — a handler sets the flag; silence = treated as used |
| `entitlements.ai.suggest_requested` (query) | `{company_id, tenant_id, denied_modules: [{module_id, denied_attempts}]}` — the AI module may set `ai_note` |

Listened to (all defensive — missing field / unknown record is skipped, never an error):

| Event | Behaviour |
|---|---|
| `license.revoked` (`{license_id, ...}`, no company) | Stored license-sourced rows lapse; the licensing leg already reflects the revocation on the next effective read |
| `license.expiring` | Warning only — intentionally a no-op; the sweep lapses on actual expiry |
| `subscription.cancelled` / `subscription.expired` (`{subscription_id, subscription: {...}, ...}`) | Stored subscription-sourced rows lapse |
| `saas_packages.cancelled` (`{purchase_id, package_id, company_id, ...}`) | Rows sourced from either id lapse |
| `tenant.module.granted` (`{tenant_id, company_id, module_id, granted_via, actor_id}`) | Mirrored into a stored row; `granted_via` maps `license→license`, `subscription→subscription`, `package→saas_package`, `manual_grant→manual_grant`, `owner_entitlement→owner_entitlement`; `preset`/`builtin` map onto nothing (see §7) |
| `tenant.module.revoked` (`{tenant_id, company_id, module_id, reason, actor_id}`) | The module's non-manual rows lapse; manual grants survive (only an admin revoke here removes them) |
| `entitlements.check_requested` and `saas_packages.entitlement.check_requested` (query `{company_id, module_id, allowed: false}`) | Answered from stored rows only (no AppContext in hook handlers): sets `allowed`, `state`, `reason` |
| `entitlements.write.check_requested` (query `{company_id, module_id, write_allowed: false}`) | Answered `false` while locked or in read-only grace, with `error_code` `entitlement.required` / `entitlement.grace_read_only` |
| `entitlements.grant_requested` (command `{company_id, module_id, source_kind, source_id, source_ref?, expires_at?, actor_id?, tenant_id?}`) | Records a grant; answers `granted: true/false` + `entitlement_id` / `reason` |
| `entitlements.ai.suggest_requested` | Answered from the denied-attempt log (counts only, never buyer personal data) |

Also emitted by the licensing check this module composes with (not
re-emitted here): `module.entitlement.granted` /
`module.entitlement.denied` (core constants).

## 6. Convergence — batch 17 tenancy plumbing (verified on disk)

The plumbing EXISTS (it landed while this module was being written;
the module was re-based onto it — the earlier "not on disk" draft
was replaced, not shipped):

- `src/core/tenancy.rs` (`pub mod tenancy` in `src/core/mod.rs`):
  `TenantContext { tenant_id: Uuid, schema_name: String,
  company_id: Option<String>, site_id: Option<String> }`,
  `TenantContext::from_http_request(req)` (fail-closed extractor),
  `schema_name(Uuid)` (the ONLY place a schema name is built:
  `tenant_` + 32 lowercase hex), `is_valid_schema_name`,
  `schema_name_for_str`, `with_tenant_connection` (the ONLY sanctioned
  Diesel path into a tenant schema), `set_tenant_search_path`,
  `run_tenant_migrations`, `schema_exists`, `drop_tenant_schema`,
  `ControlPlaneGuard`, `TENANT_SCOPE_CONTRACT`.
- Underneath: `TenantResolutionMiddleware` (priority 950, wired in
  `src/http/server.rs`) resolves the Host header through
  `tenant.resolve_requested` and attaches the resolved context
  (`middleware::TenantContext`) the tenancy extractor reads.
- Migration `2026-11-22-000094_tenant_isolation` (batch 17): control-
  plane tables `tenants`, `tenant_provisioning_steps`,
  `tenant_module_grants` (the DURABLE grant record: which modules a
  tenant was granted and via what, soft-delete revocation, partial
  unique index = at most one live grant per tenant+module).
- `tenant_module_grants` vs this module's tables: the former is the
  control-plane grant record; the latter are the tenant-schema
  runtime gate record (grace, diff, history, denied attempts). They
  agree through `tenant.module.granted` / `.revoked` (see §5). The
  tenants module gates its own grant step on
  `licensing::check_module_entitlement`.
- Remaining gap (integrator): the in-memory stores are process-global
  keyed by company id, not per-schema Diesel access through
  `with_tenant_connection` (§4). Also the admin grant endpoint
  defaults `tenant_id` to `company_id` when the administrator does
  not name it; in practice the tenants module uses the tenant id
  itself as the company id, so the default is exact for
  tenant-originated grants.

## 7. Convergence — batch 14 SaaS packages, tenants module, licensing

- `src/modules/saas_packages.rs` EXISTS (1,042 lines) but is NOT
  registered in `src/modules/mod.rs`; neither is `src/modules/
  tenants.rs` (2,414 lines). All wiring to both is events-only; the
  event names are plain string constants in this file (not imports)
  because their items are not importable until they register.
- Migration number: **000092, not 000091**. 000091 is claimed in the
  `saas_packages.rs` header as its persistence target, and a
  `2026-11-22-000091_library` directory has since appeared on disk.
  Verified at write time: no 000092 directory existed. Do not renumber
  anyone else's migration. (Neighbouring duplicates seen while
  working, for the integrator's list, not mine to fix: 000056, 000070,
  000075, 000077, 000080, 000090 ×2 each, 000093 ×2, 000100 out of
  sequence.)
- Upgrade paths: resolved through the
  `entitlement.upgrade_path_requested` hook query, because
  `saas_packages` exposes no public accessor for its published
  packages. Durable fix: a SaaS-packages listener for that event (or
  a public `published_packages` accessor), after which locked modules
  resolve to the real packages that bundle them.
- Package purchases grant entitlements through
  `entitlements.grant_requested` and cancel through
  `saas_packages.cancelled` (listened to). Until the provisioning step
  emits the grant command, a purchase enables modules without an
  entitlement row here.
- `tenant_module_grants.granted_via` values are `license`,
  `subscription`, `package`, `preset`, `owner_entitlement`,
  `builtin`, `manual_grant`. `preset` (a settings bundle, not a
  purchase source) and `builtin` (non-sellable modules need no
  entitlement) intentionally create no row here.
- Owner-entitlement consequence (important for the product model):
  the licensing rule this module composes with grants every sellable
  CORE module to every tenant implicitly, so per-product limiting
  bites hardest on sellable NON-core (extension) modules. If the
  product model ever intends core modules to be individually sold,
  the owner-entitlement rule in
  `licensing::check_module_entitlement` must change first; this
  module cannot override it from here. The effective check mirrors
  that rule exactly (`is_core` + `available_as_product` →
  `owner_entitlement` source; otherwise the license leg via
  `licensing::has_module_entitlement`).

## 8. AI wiring

- The AI contract (`AiCapability`, `register_ai_capabilities`,
  `run_ai_capability` in `crate::modules::ai`) does NOT exist in
  `ai.rs` (verified: no definition matches, although `exams.rs`,
  `library.rs`, and `saas_packages.rs` import those names). This file
  deliberately does NOT import them.
- Instead: the AI surface is the `entitlements.ai.suggest_requested`
  hook (answered from the denied-attempt log; the endpoint emits it so
  the AI module can ground/enrich the suggestion, and reports
  `ai_grounded: false` when nothing answers) plus `GET
  /ai/capabilities`, which returns the capability as data:
  `{id: "suggest_requested", module_id: "entitlements",
  use_case: "entitlements.ai.suggest_requested", allows_pii: false,
  input_fields: ["company_id"]}`.
- When the AI-contract builder lands those items, register the
  `suggest_requested` capability there and have the AI endpoint
  delegate through `run_ai_capability`. The suggestion never
  purchases or provisions anything (`purchases_nothing: true` in the
  response).

## 9. Capability seeds

Capabilities are checked through the `role.check` hook (fail closed
on silence), following the licensing module's
`require_licensing_capability` pattern. There is no code seed for
capabilities in this repo (verified: no seed table for capability
names exists); grant these to the admin role through the Roles
module's own data, and `entitlements:read` to tenant roles that may
use the dashboard:

| Capability | Held by | Used by |
|---|---|---|
| `entitlements:read` | tenant dashboard roles + admin | `my-modules`, `modules/{id}/access`, `enable`, `disable`, `package-changes/*`, `ai/suggest`, `ai/capabilities`, `settings` (GET) |
| `entitlements:manage` | admin only | `GET /entitlements`, `GET/PATCH/DELETE /entitlements/{id}`, `history`, `denied-attempts`, `settings` (PUT), `sweep` |
| `entitlements:grant` | admin only | `POST /grants` |

## 10. ApiAccessRule rows (API Management)

Register all 20 routes in the versioned public API registry
(`api_manager` collects them from `route_info()`; version support
is `All`). Seed one `ApiAccessRule` per route, all with
`allowed = true`, `public_access = false` (every route requires a
capability and, for tenant routes, a tenant context; none is public),
`version = "v1"`:

| Method | Path |
|---|---|
| GET | `/api/v1/entitlements/my-modules` |
| GET | `/api/v1/entitlements/modules/{module_id}/access` |
| POST | `/api/v1/entitlements/modules/{module_id}/enable` |
| POST | `/api/v1/entitlements/modules/{module_id}/disable` |
| POST | `/api/v1/entitlements/package-changes/apply` |
| GET | `/api/v1/entitlements/package-changes` |
| GET | `/api/v1/entitlements/package-changes/{id}` |
| POST | `/api/v1/entitlements/package-changes/{id}/confirm` |
| POST | `/api/v1/entitlements/ai/suggest` |
| GET | `/api/v1/entitlements/ai/capabilities` |
| GET | `/api/v1/entitlements/history` |
| GET | `/api/v1/entitlements/denied-attempts` |
| GET | `/api/v1/entitlements/settings` |
| PUT | `/api/v1/entitlements/settings` |
| POST | `/api/v1/entitlements/sweep` |
| POST | `/api/v1/entitlements/grants` |
| GET | `/api/v1/entitlements` |
| GET | `/api/v1/entitlements/{id}` |
| PATCH | `/api/v1/entitlements/{id}` |
| DELETE | `/api/v1/entitlements/{id}` |

## 11. README.md / TODO.md lines (for the integrator to add — not edited here)

README.md, "Core modules" list — append as the next numbered entry
after the last item:

```markdown
N. **Per-Product Entitlements** — entitlement-scoped module activation
   per tenant (license, subscription, SaaS package, manual grant, or
   owner entitlement), with a tenant dashboard listing, read-only
   grace on lapse, package-change diffs that never silently disable a
   used module, and an AI package suggestion grounded in denied
   attempts. Tenant data is resolved only through the tenant context
   and never falls back to the public schema.
```

TODO.md — append under the commerce / SaaS area (or as a new
batch-15 section):

```markdown
- [x] Per-product feature limiting (batch 15, builder B1):
      `src/modules/entitlements.rs` + migration
      `2026-11-22-000092_entitlements` (6 tables, tenant-schema):
      entitlement union (license / subscription / SaaS package /
      manual grant / owner entitlement), tenant dashboard
      (`my-modules` with upgrade paths), entitlement-gated enable
      (403 `entitlement.required`, audited), package-change diff
      with explicit confirmation, read-only grace then disable
      (tenant data never deleted), admin grants/history/settings/
      sweep, AI suggestion from denied attempts.
- [ ] Wire the Diesel persistence for entitlements (schema/models
      snippets in INTEGRATION_NOTES_BATCH15_B1.md §4) through
      `tenancy::with_tenant_connection`; replace the in-memory
      stores.
- [ ] SaaS-packages listener for `entitlement.upgrade_path_requested`
      so locked modules resolve to real packages.
- [ ] Register `saas_packages` and `tenants` in `src/modules/mod.rs`
      so their events fire (grant mirroring, provisioning).
```

## 12. Verification performed (by read-back — no compiler available)

- Line counts: `entitlements.rs` 3,635; migration up 164 / down 12.
- Routes: 20 `RouteInfo` entries = 20 `.route(...)` registrations =
  20 handler functions; every handler calls `require_capability`
  (verified per-handler by script: 20/20).
- Braces/parens/brackets balance (comment- and string-stripped):
  `{}` 464/464, `()` 1727/1727, `[]` 115/115; no duplicate function
  names (44 functions).
- `CREATE TABLE` = 6, `DROP TABLE` = 6; migration 000092 unique
  (no other 000092 directory on disk at write time).
- Greps on the new files, all 0: the `todo` / `unimplemented`
  stub macros,
  the forbidden reference-project name, Indonesian words
  (word-boundary list incl. `yang/dengan/untuk/adalah/tidak/dari/
  pengguna/modul/perusahaan/langganan/paket/kedaluwarsa/izin/
  pengaturan/riwayat/ditolak`), direct control-plane table references
  (`schema::licenses`, `schema::subscriptions`, `schema::store*`,
  `schema::companies`, `sql_query`, `diesel::sql`), and
  `X-Company-Id` as an identity source (one remaining mention is a
  doc comment stating it is NOT used).
- Every `record_history` / `create_entitlement` call site checked
  for arity after the `tenant_id` retrofit (one real bug found and
  fixed: the package-change discard call was missing its `action`
  argument; one real bug found and fixed in the lapse path: a
  module with another active source no longer enters grace).
- DashMap guard discipline: no map iteration runs while a row write
  guard is held (the `apply_source_lapse` coverage check was
  restructured for this — the shard lock is not re-entrant).
- **NOT verified: compilation.** `cargo check` has NEVER run in this
  VM (no Rust toolchain, crates.io blocked). Do not claim this
  compiles. The riskiest compile surface is the `TenantContext`
  import path (`crate::core::tenancy::TenantContext`, verified
  present in `src/core/tenancy.rs` with `from_http_request`), the
  `HookContext::set/get/add_result` API (verified in
  `src/core/hooks.rs`), and the `web::Data<Arc<...>>` deref chains
  in handlers (same pattern as `licensing.rs` / `saved_views.rs`).

## 13. OPEN list

- Compilation is NOT verified (see §12). The repo needs a round
  WITH a compiler; until then this is code-reviewed, not proven.
- The Extensions Manager has no hook subscriber for
  `module.enabled` today (verified: its enable path is the HTTP
  endpoint `POST /api/v1/extensions/{id}/enable` only), so firing
  the event is a handoff, not an activation: a dashboard that
  enables through this module should also call the Extensions
  enable endpoint (or the Extensions Manager gains a listener).
  The entitlement gate itself is enforced here regardless.
- The AI contract does not exist in `ai.rs` yet (§8); the AI
  surface here is hook-based and compiles without it.
- `saas_packages` and `tenants` are not registered in
  `src/modules/mod.rs`, so their events do not fire yet
  (upgrade-path resolution, grant mirroring, provisioning grants).
  Until the provisioning step emits `entitlements.grant_requested`,
  a package purchase enables modules without an entitlement row
  here.
- Lapse payloads are defensive (verified shapes in §5); a lapse
  whose payload names no matching source id is skipped, and the
  sweep is the backstop.
- `tenant.module.revoked` names no source, so the mirror lapses
  all non-manual rows for that tenant+module; manual grants survive
  by design.
- `preset` grants from the tenants module create no row here (§7).
- Fail-closed is an invariant, not a setting: `default_deny`
  records the admin's chosen policy, but no entitlement record
  always means not entitled, whatever the setting says.
- In-memory stores are authoritative this round; a restart loses
  grants made since the last durable write (same limitation as
  licensing / SaaS-packages until the §4 wiring lands).
- The in-memory stores are process-global keyed by company id, not
  per-schema Diesel access — the §4 wiring replaces them.
- Cross-module defect found during this work (batch 17's, not mine
  to fix, already flagged by its coordinator as must-fix): the
  `tenants` CHECK constraint in migration
  `2026-11-22-000094_tenant_isolation` (`^tenant_[0-9a-f_]{36}$`)
  does NOT match the real schema names `tenancy::schema_name`
  builds (`tenant_` + 32 lowercase hex, no underscores) — every
  real tenant row would fail the CHECK. It must become
  `^tenant_[0-9a-f]{32}$`. This module builds no schema names
  itself, so it is unaffected except through tenant provisioning.
- Naming collision to be aware of (batch 17's own warning): two
  types are named `TenantContext` (`core::middleware` vs
  `core::tenancy`). This module imports the tenancy one only
  (`crate::core::tenancy::TenantContext`) and never the middleware
  one.
- Migration duplicates elsewhere in the tree (not mine to fix):
  000056, 000070, 000075, 000077, 000080, 000090, 000093 (×2 each)
  + 000100 out of sequence (observed during this work; the
  coordinator's list is authoritative).
