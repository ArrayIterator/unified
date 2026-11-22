# Integration notes — Storage Manager as a product + bring-your-own storage (BYO)

Follow-up round on `src/modules/storage_manager.rs` (storage as a product +
BYO storage API). Migration **000056** (`2026-11-22-000056_storage_product_byo`).
Nothing was pushed. No other module file was touched.

## 1. What changed

| File | Change |
| --- | --- |
| `src/modules/storage_manager.rs` | 4,313 → 8,271 lines (**+3,958**). Product declaration moved into `impl Module`; plans section (13 routes); BYO section (27 routes); new state maps; new events; status counts. |
| `migrations/2026-11-22-000056_storage_product_byo/up.sql` | New, 315 lines, 8 tables. |
| `migrations/2026-11-22-000056_storage_product_byo/down.sql` | New, 17 lines. |
| `INTEGRATION_NOTES_STORAGE_PRODUCT.md` | This file. |

Routes: 41 → **81** (40 new). Handlers added: 40, all capability-checked
(verified: every new handler calls `authorize()`; see §7). No file outside
the three above was edited: `src/modules/mod.rs`, `src/db/schema.rs`,
`src/db/models.rs`, `README.md`, `TODO.md` are untouched by design —
the snippets the integrator needs are in §2–§5 below.

### 1.1 Product declaration (stale comment fixed)

The inherent `available_as_product` / `product_category` block (with the
comment claiming the trait did not declare them yet) is deleted. Both
methods now live in `impl Module for StorageManagerModule`, in the exact
form `meetings.rs` uses:

```rust
/// Storage Manager is sold as a product: a customer buys a
/// module entitlement for it (see `crate::modules::licensing`).
fn available_as_product(&self) -> bool {
    true
}

fn product_category(&self) -> Option<&'static str> {
    Some("infrastructure")
}
```

The status JSON (`available_as_product: true`, `product_category:
"infrastructure"`) is unchanged and consistent. `description()` now also
mentions plans and BYO.

### 1.2 Storage plans

- `StoragePlan`: `id, name, description, quota_bytes` (BIGINT bytes; GB is
  a display-only `quota_gb` in `PlanView`), `max_files` (0 = unlimited),
  `included_provider_kinds` (canonical names: `local`, `s3`, `r2`, `drive`,
  `sftp`, `s3-compatible`), `product_id` (billing reference owned by Product
  Management / billing), `billing_cycle` hint (`monthly`, `yearly`,
  `one_time`, `usage_based`), `is_public`, `sort_order`, `enabled`.
  **There is no money column anywhere** — price lives in billing.
- `PlanAssignment`: one active assignment per buyer (company and/or user),
  keyed in memory by `{company_id}|{user_id}`. `POST /plans/{id}/assign`
  assigns or changes a plan; the change re-checks current usage (recorded
  hub objects + BYO objects) against the new quota **before any mutation**
  and refuses with **409** (`storage.plan.quota_refused` emitted) when
  usage exceeds the new quota. Plan delete is refused while active
  assignments exist.
- Metering: `MeteringReport` per buyer per `YYYY-MM` period;
  `POST /plans/metering/report` stores the report and emits
  `storage.metering.reported` (buyer + plan + bytes + files + period) for
  billing. BYO usage is metered with the **same** event (delta emits on
  BYO upload/delete, source `byo_usage`).
- Entitlement: company-scoped assignment and company-scoped BYO activation
  call `require_storage_module_entitlement`, which delegates to
  `licensing::check_module_entitlement(ctx, company_id, "storage_manager")`
  once the module is registered, and to the same license match via
  `licensing::has_module_entitlement` before registration (the registry
  lookup in `check_module_entitlement` would answer 404 for a module that
  exists but is not registered yet). A plan with a `product_id`
  additionally checks the `product.entitlement.check` hook with the exact
  meetings/mcp contract: first result must carry `{"entitled": true}`;
  no answer fails closed with 503, an error answers 502, anything else 403.
- New permissions: `storage_manager.manage_plans` (admin plan CRUD,
  assign/cancel, metering), `storage_manager.read` (public plans,
  `GET /plans/my-plan` — a buyer sees only their own buyer unless they
  hold `manage_plans`).

### 1.3 Bring-your-own storage (BYO)

- `ByoConnection`: owner (`owner_user`, optional `company_id`), provider
  fields mirroring a bucket, `s3_compatible` flag (`s3-compatible` =
  S3 kind with a custom endpoint, which is required for it),
  `path_prefix` (**never empty**; defaults to `byo/{owner_user}`),
  status `draft` → `validated` → `active` / `suspended` / `revoked`,
  operation flags (`allow_read/write/delete`), quotas (`max_bytes` /
  `max_files`, 409 on exceed), `encryption_required` (same honest
  record-not-enforce matrix as buckets), `allowed_extensions` /
  `allowed_mime_types` allowlists, terms acceptance, validation and
  health outcome fields. Credentials follow the bucket pattern exactly:
  write-only, encrypted with the same versioned `v1.` HMAC-keystream
  construction, never returned/logged/emitted; views expose only
  `credentials_configured` + fingerprint.
- Allowlist: `ByoSettings` (`allowed_provider_kinds`, `terms_required`,
  `max_connections_per_user`; defaults: all six kinds, terms required,
  no per-user limit), admin-managed via `GET/PATCH /byo/settings`.
  Connecting a non-allowlisted kind is rejected with 403; activation
  re-checks the allowlist.
- Validation: `POST /byo/connections/{id}/validate` runs
  `run_byo_validation` — `test_connection`, list of the test prefix,
  write + read + delete of a probe object under the prefix, then removal
  (same probe shape as the bucket health check). Failure blocks
  activation and records the reason; success moves `draft` → `validated`.
  Credential replacement resets an active/validated connection to
  `draft` for re-validation.
- Terms: `ByoTerms` versions (`version` unique, `title`, `body`,
  `is_current`, `reacceptance_deadline`, `enabled`); one current version
  at a time. Acceptance is recorded per (user, version) with timestamp
  (`POST /byo/terms/accept`, optionally bound to a connection).
  Activation requires acceptance of the current version when terms are
  required. Changing the current version requires re-acceptance:
  `POST /byo/terms/enforce-reacceptance` suspends active/validated
  connections whose owner has not accepted the current version **after**
  its deadline passes (before the deadline, connections keep working;
  with no deadline set, the run reports 0 and does nothing).
- Rules on every object operation (`GET/POST/DELETE
  /byo/connections/{id}/objects`, `GET .../objects/download`): status
  must be `active`, operation flag, key must be under the prefix
  (`validate_key` rejects `..`, absolute paths, backslashes), extension
  and MIME allowlists, quotas (409). Prefix scoping is server-side on
  every operation through the connection.
- Ownership: `load_byo_connection_for` answers **404** (never 403) for a
  non-owner, non-admin caller — the per-user 404 pattern from the todos
  module — so cross-owner access never leaks existence. List endpoints
  are owner-scoped; admins (holding `storage_manager.manage_byo`) see all.
- Health: per-connection probe (`POST /byo/connections/{id}/health`,
  same write/read/delete probe as buckets), history endpoints, and
  `POST /byo/health/run` (sync run for all active connections, optionally
  also enqueues `storage.byo.health_check`; see §4). Hook handler
  `on_byo_health_check_requested` answers the scheduler event.
- New permissions: `storage_manager.manage_byo` (admin: settings, terms
  CRUD, enforce, see-all), `storage_manager.use_byo` (owner self-service),
  `storage_manager.health` (BYO health runs/history, as for buckets).

## 2. Registration (`src/modules/mod.rs`) — integrator

`pub mod storage_manager;` is **not** in `mod.rs` yet (verified: the grep
finds no such line). Add it in alphabetical order with the other `pub mod`
lines, and in `register_all` after `storage::register(registry);` (the
placement batch 2 already agreed):

```rust
pub mod storage_manager;
```

```rust
storage_manager::register(registry);
```

`register()` itself already exists at the end of `storage_manager.rs`
(`registry.register(Arc::new(StorageManagerModule::new()))`).

## 3. Schema (`src/db/schema.rs`) — integrator, paste after the 000033 blocks

```rust
diesel::table! {
    storage_plans (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Varchar,
        quota_bytes -> Int8,
        max_files -> Int8,
        included_provider_kinds -> Array<Text>,
        product_id -> Nullable<Varchar>,
        billing_cycle -> Nullable<Varchar>,
        is_public -> Bool,
        sort_order -> Int8,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_plan_assignments (id) {
        id -> Uuid,
        plan_id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        status -> Varchar,
        assigned_by -> Varchar,
        assigned_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_usage_metering (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        plan_id -> Nullable<Uuid>,
        period -> Varchar,
        bytes_stored -> Int8,
        files_stored -> Int8,
        reported_by -> Varchar,
        reported_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_byo_connections (id) {
        id -> Uuid,
        name -> Varchar,
        provider_kind -> Varchar,
        s3_compatible -> Bool,
        endpoint -> Varchar,
        region -> Varchar,
        container -> Varchar,
        path -> Varchar,
        sftp_host -> Varchar,
        sftp_port -> Int4,
        sftp_username -> Varchar,
        sftp_auth_kind -> Varchar,
        sftp_keyfile -> Varchar,
        allowed_hosts -> Array<Text>,
        credentials_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        path_prefix -> Varchar,
        owner_user -> Varchar,
        company_id -> Nullable<Varchar>,
        status -> Varchar,
        status_reason -> Nullable<Varchar>,
        allow_read -> Bool,
        allow_write -> Bool,
        allow_delete -> Bool,
        max_bytes -> Nullable<Int8>,
        max_files -> Nullable<Int8>,
        used_bytes -> Int8,
        used_files -> Int8,
        encryption_required -> Bool,
        allowed_extensions -> Array<Text>,
        allowed_mime_types -> Array<Text>,
        terms_version -> Nullable<Varchar>,
        terms_accepted_at -> Nullable<Timestamptz>,
        last_validated_at -> Nullable<Timestamptz>,
        last_validation_ok -> Nullable<Bool>,
        last_validation_error -> Nullable<Varchar>,
        last_health_at -> Nullable<Timestamptz>,
        last_health_ok -> Nullable<Bool>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_byo_terms (id) {
        id -> Uuid,
        version -> Varchar,
        title -> Varchar,
        body -> Text,
        is_current -> Bool,
        reacceptance_deadline -> Nullable<Timestamptz>,
        enabled -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_byo_terms_acceptances (id) {
        id -> Uuid,
        terms_id -> Uuid,
        terms_version -> Varchar,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        connection_id -> Nullable<Uuid>,
        accepted_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_byo_objects (connection_id, key) {
        connection_id -> Uuid,
        key -> Varchar,
        size_bytes -> Int8,
        mime -> Varchar,
        owner_user -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    storage_byo_health_checks (id) {
        id -> Uuid,
        connection_id -> Uuid,
        ok -> Bool,
        latency_ms -> Int8,
        detail -> Varchar,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

No `joinable!` lines are needed: the module deliberately declares no
foreign keys between these tables (audit rows must survive the removal
of the plan / terms / connection they reference), following migration
000033. No money columns: quotas and metering are BIGINT bytes/counts,
so the NUMERIC(19,4) rule does not apply.

## 4. Models (`src/db/models.rs`) — integrator

Structs mirroring the module structs field-for-field (`StoragePlan`,
`PlanAssignment`, `MeteringReport`, `ByoConnection`, `ByoTerms`,
`ByoTermsAcceptance`, `ByoObject`, `ByoHealthCheck` in
`src/modules/storage_manager.rs`; copy the derive /
`#[diesel(table_name = …)]` block from the existing storage structs,
e.g. the 000033 ones). Type mapping, per struct:

- `StoragePlan`: `id: Uuid`, `name: String`, `description: String`,
  `quota_bytes: i64`, `max_files: i64`,
  `included_provider_kinds: Vec<String>`, `product_id: Option<String>`,
  `billing_cycle: Option<String>`, `is_public: bool`, `sort_order: i64`,
  `enabled: bool`, `created_at: DateTime<Utc>`, `updated_at: DateTime<Utc>`.
- `PlanAssignment`: `id: Uuid`, `plan_id: Uuid`,
  `company_id: Option<String>`, `user_id: String`, `status: String`,
  `assigned_by: String`, `assigned_at: DateTime<Utc>`,
  `expires_at: Option<DateTime<Utc>>`,
  `cancelled_at: Option<DateTime<Utc>>`, `cancel_reason: Option<String>`,
  `created_at: DateTime<Utc>`, `updated_at: DateTime<Utc>`.
- `MeteringReport`: `id: Uuid`, `company_id: Option<String>`,
  `user_id: String`, `plan_id: Option<Uuid>`, `period: String`,
  `bytes_stored: i64`, `files_stored: i64`, `reported_by: String`,
  `reported_at: DateTime<Utc>`.
- `ByoConnection`: `id: Uuid`, `name: String`,
  `provider_kind: String` (the stored `StorageKind` name; map through
  `StorageKind::from_str` on read), `s3_compatible: bool`,
  `endpoint / region / container / path / sftp_host: String`,
  `sftp_port: i32`, `sftp_username / sftp_auth_kind / sftp_keyfile: String`,
  `allowed_hosts: Vec<String>`,
  `credentials_encrypted: Option<String>` (mark
  `#[serde(skip_serializing)]`), `key_fingerprint: Option<String>`,
  `path_prefix: String`, `owner_user: String`,
  `company_id: Option<String>`, `status: String`,
  `status_reason: Option<String>`, `allow_read / allow_write /
  allow_delete: bool`, `max_bytes / max_files: Option<i64>`,
  `used_bytes / used_files: i64`, `encryption_required: bool`,
  `allowed_extensions / allowed_mime_types: Vec<String>`,
  `terms_version: Option<String>`,
  `terms_accepted_at / last_validated_at: Option<DateTime<Utc>>`,
  `last_validation_ok: Option<bool>`,
  `last_validation_error: Option<String>`,
  `last_health_at: Option<DateTime<Utc>>`,
  `last_health_ok: Option<bool>`, `created_at / updated_at: DateTime<Utc>`.
- `ByoTerms`: `id: Uuid`, `version: String`, `title: String`,
  `body: String`, `is_current: bool`,
  `reacceptance_deadline: Option<DateTime<Utc>>`, `enabled: bool`,
  `created_by: String`, `created_at / updated_at: DateTime<Utc>`.
- `ByoTermsAcceptance`: `id: Uuid`, `terms_id: Uuid`,
  `terms_version: String`, `user_id: String`,
  `company_id: Option<String>`, `connection_id: Option<Uuid>`,
  `accepted_at: DateTime<Utc>`.
- `ByoObject`: `connection_id: Uuid`, `key: String`, `size_bytes: i64`,
  `mime: String`, `owner_user: String`, `company_id: Option<String>`,
  `created_at: DateTime<Utc>`, `updated_at: DateTime<Utc>`.
  (Composite primary key `(connection_id, key)` — no single `id` column.)
- `ByoHealthCheck`: `id: Uuid`, `connection_id: Uuid`, `ok: bool`,
  `latency_ms: i64`, `detail: String`, `checked_at: DateTime<Utc>`.

The in-memory DashMap stores remain authoritative; these tables are the
persistence targets, exactly as for migration 000033.

## 5. Events, audit, scheduler, README / TODO

### New hook events (all emitted on the hook bus, never direct calls)

- Plans: `storage.plan.created`, `storage.plan.updated`,
  `storage.plan.deleted`, `storage.plan.assigned`,
  `storage.plan.changed`, `storage.plan.cancelled`,
  `storage.plan.quota_refused`.
- Metering: `storage.metering.reported` (plan reports + BYO usage deltas;
  the billing handoff — billing listens, this module never calls it).
- BYO: `storage.byo.created`, `storage.byo.updated`,
  `storage.byo.validated`, `storage.byo.validation_failed`,
  `storage.byo.activated`, `storage.byo.suspended`,
  `storage.byo.revoked`, `storage.byo.terms_accepted`,
  `storage.byo.terms_changed`, `storage.byo.health_ok`,
  `storage.byo.health_failed`, `storage.byo.quota_exceeded`.
- Listened to: `storage.byo.health_check_requested` (answered by
  `on_byo_health_check_requested`, spawning checks in the background).
- Emitted to the scheduler: `scheduler.enqueue` with kind
  `storage.byo.health_check` from `POST /byo/health/run` with
  `enqueue: true`.
- This module also emits `storage.object.stored` /
  `storage.object.deleted` for BYO object operations (same events as
  bucket objects).

### AUDITED_EVENTS additions (`src/modules/audit.rs`)

Add the event names the audit trail should record (same string values
as the `events` constants in `storage_manager.rs`):

```rust
"storage.plan.created",
"storage.plan.updated",
"storage.plan.deleted",
"storage.plan.assigned",
"storage.plan.changed",
"storage.plan.cancelled",
"storage.plan.quota_refused",
"storage.metering.reported",
"storage.byo.created",
"storage.byo.updated",
"storage.byo.validated",
"storage.byo.validation_failed",
"storage.byo.activated",
"storage.byo.suspended",
"storage.byo.revoked",
"storage.byo.terms_accepted",
"storage.byo.terms_changed",
"storage.byo.health_ok",
"storage.byo.health_failed",
"storage.byo.quota_exceeded",
```

### Scheduler kind (`src/modules/scheduler.rs`)

Add `"storage.byo.health_check"` to `KNOWN_KINDS` and arm it, mirroring
the lifecycle kind:

```rust
"storage.byo.health_check" => {
    ctx.hooks.emit("storage.byo.health_check_requested", payload);
}
```

Until then the enqueue in `POST /byo/health/run` answers
`{"queued": false, "error": "Unknown job kind: ..."}` and the run
falls back to executing synchronously in-process — the response
reports `scheduler_queued: false` honestly, never pretending a job
was scheduled. The same note already applies to
`storage.lifecycle.run`. There is no scheduler job for the
re-acceptance enforcement: it is an on-demand admin endpoint
(`POST /byo/terms/enforce-reacceptance`); a scheduler arm for it is
an integrator choice, not a hidden dependency.

### README / TODO lines (integrator, do not edit here)

- README module table: Storage Manager row — routes 41 → 81; add
  `000056_storage_product_byo` to its migration cell.
- TODO.md: mark the batch-2 item "move inherent product methods into
  `impl Module`" done for storage_manager (the other three modules
  are other builders' files); add the open items from §6 as follow-ups.

## 6. New routes (40)

Plans (13), all under `/api/v1/storage-manager`:

| Method | Path | Capability |
| --- | --- | --- |
| GET | `/plans` | `storage_manager.manage_plans` |
| POST | `/plans` | `storage_manager.manage_plans` |
| GET | `/plans/public` | `storage_manager.read` |
| GET | `/plans/my-plan` | `storage_manager.read` |
| GET | `/plans/assignments` | `storage_manager.manage_plans` |
| GET | `/plans/assignments/{id}` | `storage_manager.manage_plans` |
| POST | `/plans/assignments/{id}/cancel` | `storage_manager.manage_plans` |
| POST | `/plans/metering/report` | `storage_manager.manage_plans` |
| GET | `/plans/metering` | `storage_manager.manage_plans` |
| GET | `/plans/{id}` | `storage_manager.read` |
| PATCH | `/plans/{id}` | `storage_manager.manage_plans` |
| DELETE | `/plans/{id}` | `storage_manager.manage_plans` |
| POST | `/plans/{id}/assign` | `storage_manager.manage_plans` |

BYO (27), all under `/api/v1/storage-manager`:

| Method | Path | Capability |
| --- | --- | --- |
| GET | `/byo/settings` | `storage_manager.read` |
| PATCH | `/byo/settings` | `storage_manager.manage_byo` |
| GET | `/byo/terms` | `storage_manager.read` |
| POST | `/byo/terms` | `storage_manager.manage_byo` |
| GET | `/byo/terms/current` | `storage_manager.read` |
| GET | `/byo/terms/{id}` | `storage_manager.read` |
| PATCH | `/byo/terms/{id}` | `storage_manager.manage_byo` |
| DELETE | `/byo/terms/{id}` | `storage_manager.manage_byo` |
| POST | `/byo/terms/accept` | `storage_manager.use_byo` |
| POST | `/byo/terms/enforce-reacceptance` | `storage_manager.manage_byo` |
| GET | `/byo/connections` | `storage_manager.use_byo` |
| POST | `/byo/connections` | `storage_manager.use_byo` |
| GET | `/byo/connections/{id}` | `storage_manager.use_byo` |
| PATCH | `/byo/connections/{id}` | `storage_manager.use_byo` |
| DELETE | `/byo/connections/{id}` | `storage_manager.use_byo` |
| POST | `/byo/connections/{id}/validate` | `storage_manager.use_byo` |
| POST | `/byo/connections/{id}/activate` | `storage_manager.use_byo` |
| POST | `/byo/connections/{id}/suspend` | `storage_manager.use_byo` |
| POST | `/byo/connections/{id}/revoke` | `storage_manager.use_byo` |
| POST | `/byo/connections/{id}/health` | `storage_manager.health` |
| GET | `/byo/connections/{id}/health` | `storage_manager.health` |
| GET | `/byo/connections/{id}/objects` | `storage_manager.use_byo` |
| POST | `/byo/connections/{id}/objects` | `storage_manager.use_byo` |
| GET | `/byo/connections/{id}/objects/download` | `storage_manager.use_byo` |
| DELETE | `/byo/connections/{id}/objects` | `storage_manager.use_byo` |
| GET | `/byo/health` | `storage_manager.health` |
| POST | `/byo/health/run` | `storage_manager.health` |

All 40 are registered in both `configure_routes` and `route_info`
(verified 1:1, no duplicates, no missing handler). New capabilities
to register in the roles module: `storage_manager.manage_plans`,
`storage_manager.manage_byo`, `storage_manager.use_byo` (the other
capabilities already existed).

## 7. What was verified in this round

- Read `src/modules/storage_manager.rs` in full (4,313 lines) before
  changing anything; read `src/core/modules.rs` trait methods,
  `meetings.rs` trait-override form and `product.entitlement.check`
  contract, `licensing.rs` entitlement functions, and the 000033
  migration for style.
- Migration number: re-checked the migrations directory at write time;
  **000056 was free** (000057/000058 taken by batch 4, 000059 workflows,
  000060+ by batches 5/6, 000100 calculator). Used
  `2026-11-22-000056_storage_product_byo`.
- Delimiter balance (comment/string/raw-string-aware check):
  `{` 1173/1173, `(` 4626/4626, `[` 219/219 on the final file.
- Routes: 81 configured = 81 `RouteInfo` entries, 0 without a match
  either way, 0 duplicates, 0 routes with a missing handler.
- All 40 new handlers defined and all 40 call `authorize()` (0 without).
- Forbidden-name hits: 0 (no reference-project name anywhere in any
  file written). Indonesian hits: 0 (word-boundary grep over the
  module). Stub macros: 0 (`todo!` / `unimplemented!` /
  `unreachable!`). No binary files. No push. No edits to `mod.rs`,
  `schema.rs`, `models.rs`, `README.md`, `TODO.md`, `MEMORY.md`, the
  tracker, `LICENSE`, or any other batch's files.
- **Not verified: compilation.** `cargo check` has never run in this VM
  (no toolchain, crates.io blocked), so compilation is explicitly NOT
  claimed. A borrow-ordering hazard found by review (map guard held
  while calling back into state) was fixed by reordering
  check-then-mutate in `assign_plan` and by a two-pass collect-then-
  suspend in `enforce_byo_reacceptance`; that fix is review-only,
  not compiler-proven.

## 8. OPEN list (for the integrator / later rounds)

1. **Register the module** (§2): `pub mod storage_manager;` +
   `storage_manager::register(registry);` after
   `storage::register(registry);`. Until then the entitlement gate
   uses the `has_module_entitlement` fallback (§1.2), and the
   `storage.byo.health_check` / `storage.lifecycle.run` scheduler
   kinds answer "unknown kind" with an in-process fallback.
2. **Run `cargo check` / `cargo test`** — never run in this VM.
   Highest-risk spots for the first compile: the new structs/handlers
   in `storage_manager.rs` (§1.2–1.3) and the `licensing` import.
3. **Run migration 000056 against a scratch PostgreSQL database**
   before applying it anywhere real (same standing warning as 000033).
   Then paste the §3 schema blocks and §4 models.
4. **Register the three new capabilities** in the roles module
   (`manage_plans`, `manage_byo`, `use_byo`) and add the §5
   `AUDITED_EVENTS` entries; add `storage.byo.health_check` to the
   scheduler's `KNOWN_KINDS` (§5).
5. **Wire billing to `storage.metering.reported`** (plan reports and
   BYO deltas) and a handler for `product.entitlement.check` for
   plan `product_id`s — until a handler answers, plan assignment
   with a `product_id` fails closed with 503, by design.
6. **Wire the BYO allowlist and terms to the admin UI**, and decide
   whether the re-acceptance enforcement gets a scheduler arm or
   stays an on-demand admin run.
7. **Decide the plan-quota enforcement scope**: quotas are enforced
   at plan *change* (409) and reported in `GET /plans/my-plan`
   (`over_quota` flag); uploads are still gated by the existing
   per-scope quotas, not by the plan quota. If plan quotas must also
   reject uploads, that is a follow-up touching the upload path.
8. **Persistence**: the new DashMap stores are authoritative, as
   before; nothing in this round writes the new tables (same as
   000033). A persistence writer is integrator work.
9. **Live provider verification**: BYO validation/health probes were
   written against the existing provider contract (local, S3, R2,
   Drive, SFTP) but never run against a live provider here.
