# C11 DB Read Path — Integration Notes (task 271)

Priority slice of `docs/integration/C11_DB_READ_PATH_PROGRAM.md`.
Delivered 2026-11-23. All code and docs in English only.

## 0. What this slice is (and is not)

The program brief counted 784 DashMap store declarations across 129
files; most business state is in-memory-only (lost on restart,
invisible cross-instance). This slice wires the **priority 1–2**
modules to the database tables migrations `000001`–`000123` already
defined as DDL "persistence targets" (675 tables). No new tables
were needed. The work is the read path: hydrate-on-startup,
write-through, or direct Diesel queries — one choice per module.

**Deliberately stays in memory (not migrated):** lock / dedup /
rate-limit maps, caches with a database behind them, anything
documented as intentionally ephemeral (webhook-event dedup,
pending checkouts, cached PayPal tokens — see §3).

**Verification limits, stated plainly:** no live PostgreSQL exists
on this VM, so restart survival below (§5) is code-level analysis
(seed → restart → read back), not an observed run. No `cargo
check` / `cargo test` was run by this worker — several workers
share the VM and the coordinator runs lib-crate checks centrally.
`rustfmt --edition 2024 --check` results are in §7.

**Tracker blocker (read this first):** the tracker tool
(`artifact.invoke_action` / `updatetask` / `createtask`) is NOT
available in this subagent session — loading the artifact
namespace returns only restore/share/unshare, and no usable CLI
or doc exists in the workspace. So task 271 was **not** moved to
"In progress", was **not** closed, and **no follow-up tasks were
created**. The follow-up definitions in §6 are tracker-ready
(title + description + project id 1 + normal priority); the
coordinator must create them and move/close 271.

## 1. Per-module choice

| Module | Choice | Status | Files |
|---|---|---|---|
| documents / finalize_jobs | **Write-through** (DB first, map second) | Wired; pool attach is a one-line follow-up (§4) | `src/modules/documents/state/finalize.rs` |
| billing | **Hydrate-on-startup** | `hydrate_from_db` written; init call is a follow-up (§4) | `src/modules/billing/state.rs` |
| store | **Hydrate-on-startup** | `hydrate_from_db` written; init call is a follow-up (§4) | `src/modules/store/state.rs` |
| payments | **Hydrate-on-startup** | `hydrate_from_db` written; init call is a follow-up (§4) | `src/modules/payments/records.rs` |
| esg | **Hydrate-on-startup** | `hydrate_from_db` written; init call is a follow-up (§4) | `src/modules/esg/state.rs` |
| forums | **Hydrate-on-startup** | `hydrate_from_db` written; init call is a follow-up (§4) | `src/modules/forums/state.rs` |
| licensing | **Direct Diesel queries** | Already wired; verified, no code change | (see §2.7) |

Each `hydrate_from_db` clears and replaces its maps (a restart
never merges stale in-memory rows with database rows) and returns
the number of rows loaded. A load error aborts the hydrate with
`UnifiedError::internal(...)` — the init call sites in §4
propagate it, so a live database that fails to answer fails
startup loudly instead of serving empty maps. When no pool is
attached at all, the hydrate is never called and the module runs
in-memory-only, exactly as before.

## 2. What each module wires (and what it skips)

### 2.1 documents — finalize_jobs write-through

`DocumentsState` has no pool field and `state.rs` is owned by the
documents-metadata worker, so the pool attach seam lives entirely
inside `finalize.rs`:

- `static FINALIZE_DB_POOL: OnceLock<DbPool>` + `pub(crate) fn
  attach_finalize_pool(pool)` (first attach wins; a double `init`
  can never swap the pool under a running worker) + `fn
  finalize_pool() -> Option<DbPool>`.
- Pure helpers: `parse_job_uuid`, `job_insert_row` (`None` on a
  non-UUID id/session; an unparseable document id degrades to
  `NULL`), `db_job_to_runtime` (attempts clamped ≥ 0).
- DB helpers: `db_session_exists`, `db_document_exists` (FK
  guards), `db_insert_job` (`.on_conflict(id).do_nothing()`),
  `db_try_claim_job`, `db_reap_stale_claims`, `db_finish_job`,
  `db_load_open_jobs` (status IN `pending`/`finalizing`),
  `db_load_finalize_jobs_by_id` (empty input returns empty —
  an empty `IN ()` is never sent).
- `pub(crate) fn db_sync_finalize_jobs(&self) -> usize`:
  hydrates open rows whose session is in the map; adopts a
  DB-`finalizing` row into a map-`pending` row (never re-claims
  another instance's work); marks map rows terminal when their
  DB row is `verified`/`failed` (one by-id query over known
  pending/finalizing ids); never downgrades a map-`finalizing`
  row; rows whose session is not in the map stay in the DB.
  No-op without a pool or on DB error.

Wiring inside `finalize.rs` (no signature changes — callers in
`module.rs` and `handlers_sessions.rs` are out of bounds):

- `start_finalize_worker` runs `db_sync_finalize_jobs()` before
  `rescan_finalize_jobs()`.
- `create_finalize_job` writes the DB row first (best-effort,
  `let _ =`); the map insert still runs when the row cannot be
  persisted, so the worker keeps working in-memory.
- `claim_finalize_job` claims in-memory, drops the map lock,
  then runs the DB claim as the cross-instance arbiter. Only a
  lost claim (`Ok(Some(false))`) rolls the map row back to
  `pending` and continues to the next job; no-row / DB-error
  keeps the in-memory claim.
- `reap_stale_finalize_claims` runs the DB reap first and
  mirrors reaped rows into the map via by-id reload (prevents a
  double attempts bump), then the existing in-memory pass (which
  then only reaps jobs with no DB row).
- `finalize_tick` calls `db_sync_finalize_jobs()` first.
- `run_finalize_job` calls `db_finish_job` (`verified` / `failed`)
  after the map update on both paths.

**FK rule (the important caveat):** `db_insert_job` pre-checks
that the session row (and document row, when present) exists in
the database and inserts nothing when it does not. Sessions and
documents are still in-memory-only — the documents-metadata
worker owns their persistence. Until that lands, the FK guard
will skip the insert and jobs stay in-memory-only in practice:
the write-through is wired, but it has nothing durable to
reference yet. That is by design, not a bug: a job row must
never reference a session that does not exist.

**Degraded-mode contract:** every DB call is best-effort at the
call site (errors swallowed, in-memory map stays authoritative
for that tick) — the same contract licensing uses (DB reads fall
back to the map when no pool is attached). No `log::` usage
exists in the documents module; DB errors are swallowed with
comments, not logged.

**New unit tests** (pure, no pool attached): `db_job_insert_row_
maps_ids_and_tenant`, `db_job_insert_row_rejects_non_uuid_ids`,
`db_job_round_trips_through_runtime` (negative attempts → 0),
`db_sync_is_noop_without_pool` (sync returns 0, create+claim
still work in-memory). The existing 5 tests are untouched. A
dead-URL pool test was deliberately NOT written (`pool.get()`
would block/retry up to the connection timeout).

### 2.2 billing — hydrate-on-startup

`BillingState::hydrate_from_db(&self, pool: &DbPool) ->
UnifiedResult<usize>` (`src/modules/billing/state.rs:188`).
Loads invoices + items, invoice payments, credit notes, and
invoice sequences.

- Money is exact: `PgNumeric` → minor units via
  `unified_core::db::money::pg_numeric_to_minor` with the row's
  own currency; a row that does not convert is skipped, never
  approximated. `InvoicePayment` / `CreditNote` have no currency
  column — they use their parent invoice's currency, and an
  orphan (invoice missing) is skipped.
- `Invoice.subscription_id` is `None`: the DB model has no
  `subscription_id` column. `payment_terms_days` `i32` → `u32`
  (≥ 0 only); `reminders_sent` Jsonb → `Vec<i32>`.
- Sequences reseed from the `invoice_sequences` table
  (`invoice` / `credit_note` rows, `next_value BIGINT`); when
  the table has no row, the seed is max parsed number + 1
  (numbers `{prefix}-{seq:06}`, credit notes `CN-{seq:06}`).

**Skipped:** subscription-lifecycle mirrors and dunning state
(no DB columns — runtime mirrors of store/billing events);
school-fees maps (`fee_types`, `fee_charges`, …) — no
persistence target was wired in this slice. Both are
follow-ups (§6).

### 2.3 store — hydrate-on-startup

`StoreState::hydrate_from_db` (`src/modules/store/state.rs:153`).
Loads products, pricing tiers (priced in the parent product
currency; features Jsonb → `Vec<String>`), orders + items
(items priced in the parent order currency; an item with NULL
`product_id` is skipped), entitlements (NULL `order_id` /
`product_id` rows skipped), downloads (NULL `license_id` /
`release_id` skipped), catalog, issued licenses.

- Runtime-only fields take defaults via the `pub(crate)`
  helpers in `types.rs` (`default_proration_policy()`,
  `default_dunning_max_retries()`,
  `default_dunning_retry_interval_days()`); grace 0;
  `module_id` / `discount_evaluation` / `payment_reference` /
  `course_id` / `exam_id` `None`; an unconvertible renewal
  price → `None`.

**Skipped:** coupons — the runtime `Coupon` splits one DB
`discount_value` into `discount_percent_bp` vs
`discount_fixed_minor`, and no writer records which encoding a
stored value uses (`discount_value` appears nowhere in store
runtime code except a doc comment), so the mapping is
ambiguous. Carts and subscriptions — no `store_carts` /
`store_subscriptions` tables exist. `digital_*` tables exist
(000078) but are not hydrated in this slice. All follow-ups
(§6); coupons need a discount-encoding decision first.

### 2.4 payments — hydrate-on-startup

Free function `hydrate_from_db(pool: &DbPool) ->
UnifiedResult<usize>` (`src/modules/payments/records.rs:385`,
after `paypal_tokens_store()`). Loads connections, intents,
transactions, methods, gateway subscriptions, split bills +
parts (grouped by `split_id`, sorted by index).

- Connections: `kind` / `mode` via `PaymentProviderKind::
  from_str` / `GatewayMode::from_str`; credentials via
  `serde_json::from_value::<PaymentCredentials>` (derives
  `Deserialize` + `Default`); `CustomGatewayConfig` via
  `from_value`. Unparseable rows are skipped.
- Intents: transient `payment_method_token` /
  `payment_method_id` → `None`.
- Transactions: rebuilds the idempotency index
  `(connection_id, idempotency_key) → transaction id`;
  `original_transaction_id` / `operator_id` → `None`.
- Methods: expiry `i32` → `u32`. Gateway subscriptions:
  dunning fields → `0` / `false` / `None` (the DB stores no
  dunning state).

**Skipped (by design, ephemeral):** webhook-event dedup
(time-bounded), pending checkouts (in-flight), cached PayPal
tokens (expiring cache). Bank-transfer verifications have no
runtime map at all. Runtime payment records carry no
`tenant_id` field — the 000126 columns are DDL + DB models
only in this slice; runtime stamping is a follow-up (§6).

### 2.5 esg — hydrate-on-startup

`EsgState::hydrate_from_db` (`src/modules/esg/state.rs:217`).
Loads all 7 tables: `esg_frameworks`, `esg_metrics`,
`esg_periods`, `esg_entries`, `esg_entry_history`,
`esg_targets`, `esg_audit`. Enum values parse via
`Category` / `MetricDataType` / `PeriodStatus` /
`TargetDirection::from_str` (all `pub(crate)` in `types.rs`);
an unparseable row is skipped. `framework_ids` /
`document_ids` Jsonb → `Vec<String>`. `tenant_id` and
`scope_type` / `scope_id` are carried as stored — the tables
already had `tenant_id TEXT NOT NULL` (000119), so no DDL was
needed for ESG.

**Skipped:** nothing at table level. (New Diesel schema/models
for all 7 tables were created in this slice — they never had
any.)

### 2.6 forums — hydrate-on-startup

`ForumsState::hydrate_from_db`
(`src/modules/forums/state.rs:139`). Loads all 13 content
tables + settings: `forums`, `forum_categories`,
`forum_threads`, `forum_posts`, `forum_post_edits`,
`forum_votes`, `forum_reports`, `forum_moderation_actions`,
`forum_subscriptions`, `forum_polls`, `forum_poll_options`,
`forum_poll_votes`, `forum_reputation`, `forum_settings`.

- Visibility is matched inline (`ForumVisibility` has only
  `as_str`, no `from_str`); an unknown value skips the row.
  Post `depth` SMALLINT → clamped 0–2 → `i8`. Vote `value`
  must be ±1 or the row is skipped. `tags` / `attachment_ids`
  / `mentions` are `TEXT[]` → `Vec<String>`.
- Settings are applied ONLY when exactly one
  `forum_settings` row exists (runtime settings are
  installation-wide, not per-tenant).
- `tenant_id` is carried as stored — the tables already had
  `tenant_id TEXT NOT NULL` (000110), so no DDL was needed.

**Skipped:** per-tenant settings (needs a design decision —
settings are installation-wide today). New Diesel
schema/models for all 14 tables were created in this slice —
they never had any.

### 2.7 licensing — direct Diesel queries (already wired, verified)

No code change in this slice. Verified from source:

- Reads prefer the DB with map fallback, e.g.
  `handlers.rs:47`: `match db_pool(&ctx) { Some(pool) =>
  db_load_licenses(&pool)?, None => licenses_store()... }`.
- Creates insert via Diesel (`hooks.rs:73` `NewLicense`,
  `validation.rs:164` `NewLicenseActivation`).
- DB helpers in `store.rs`: `db_pool(ctx)`,
  `db_load_licenses`, `db_load_activations`,
  `db_find_license_by_key` / `db_find_license_by_id`.

**Not yet DB-backed (follow-up):** license products,
releases, distributions, events, offline tokens, activation
history. Their tables exist (000069) and their Diesel models
exist (`crates/unified-db-core/src/models/licensing.rs`), but
no `insert_into` / `*_dsl` read exists in the licensing
handlers — products are created/read through
`license_products_store()` only. The money-critical records
(licenses, activations) are already durable; the auxiliary
maps are metadata, not money movement.

## 3. Migration 000126 — the only new migration

`crates/unified-core/migrations/2026-11-23-000126_c11_db_read_path/{up,down}.sql`.
No new tables. All statements idempotent (`IF NOT EXISTS`).
No existing migration was edited. (Other workers' `000125_data_tools`
and `000127_c12_registry_tables` were not touched.)

`up.sql` adds, with one index each:

- `finalize_jobs.tenant_id UUID` (nullable, **NO foreign
  key** — tenant ids resolve via the tenancy middleware, and
  the tenants table may not be visible in a tenant schema's
  search path; same convention as `documents.company_id` and
  the ESG/forum reference ids) + `idx_finalize_jobs_tenant_id`.
- `payment_intents` / `payment_transactions` /
  `payment_methods` / `gateway_subscriptions` `.tenant_id TEXT`
  (nullable; NULL = written before C11 stamping) +
  `idx_<table>_tenant_id` each.

`down.sql` drops the 5 columns + indexes in reverse order.

ESG (000119) and forums (000110) already carry `tenant_id
TEXT NOT NULL` on every row — nothing to add for them.

### Diesel schema/models changes

- `crates/unified-db-platform/src/schema/documents.rs` +
  `models/documents.rs`: `tenant_id -> Nullable<Uuid>` /
  `pub tenant_id: Option<Uuid>` on the `finalize_jobs` table
  and the `FinalizeJob` / `NewFinalizeJob` models.
- `crates/unified-db-commerce/src/schema/payments.rs` +
  `models/payments.rs`: `tenant_id -> Nullable<Varchar>` /
  `pub tenant_id: Option<String>` on `payment_intents`,
  `payment_transactions`, `payment_methods`,
  `gateway_subscriptions` (Query + New structs).
- NEW `crates/unified-db-platform/src/schema/esg.rs` +
  `models/esg.rs`: all 7 ESG tables with exact DDL columns;
  models `EsgFramework` / `EsgMetric` / `EsgPeriod` /
  `EsgEntry` / `EsgEntryHistory` / `EsgTarget` / `EsgAudit`
  + `New*` variants (prefixed — bare `Metric` / `Period` /
  `Entry` / `Target` are too generic for the shared
  namespace). No `joinable!` / `allow_tables_to_appear_in_same_query!`
  (no FKs; hydrate loads each table alone).
- NEW `crates/unified-db-platform/src/schema/forums.rs` +
  `models/forums.rs`: all 14 forum tables with exact DDL
  columns (`Array<Text>`, `Int2`, `Int8`, `Date`); models
  named naturally (`Forum`, `ForumThread`, … + `New*`).
- Registered in `crates/unified-db-platform/src/schema.rs`
  (`pub mod esg;`, `#[path = "schema/forums.rs"] pub mod
  forums_tables;` + `pub use` lines, alphabetical — the
  `forums_tables` alias is required because `schema/forums.rs`
  defines a table named `forums`) and `models.rs`
  (`pub mod esg; pub mod forums;` + `pub use` lines).
- All DB model names were verified unique across the
  `unified-db-*` crates (no glob re-export ambiguity). The
  `invoices` collision is handled per the NOTE in
  `models/invoices.rs`: `crate::schema::invoices` is the FILE
  module, the table is `crate::schema::invoices::invoices`.

## 4. Init call sites (follow-up wiring — one line per module)

All `init` functions receive `ctx: &AppContext`; the pool source
is always the same (`AppContext.db` is
`Arc<RwLock<Option<DbPool>>>`, `crates/unified-core/src/context.rs:42`):

```rust
if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
    /* ... */
}
```

Recommended error policy: propagate with `?` (fail init loudly
on a query error against a live DB; skip silently when the pool
is `None`). A failed hydrate that silently leaves empty maps
behind a live database is worse than a failed startup.

- **documents** — `src/modules/documents/module.rs`,
  `DocumentsModule::init` (line 278), immediately before
  `self.state.start_finalize_worker();` (line 295):
  ```rust
  if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
      crate::modules::documents::state::finalize::attach_finalize_pool(pool);
  }
  ```
  (Also documented in the comment block above
  `attach_finalize_pool` in `finalize.rs`.)
- **billing** — `src/modules/billing/module.rs:56`,
  `BillingModule::init`:
  ```rust
  if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
      self.state.hydrate_from_db(&pool)?;
  }
  ```
- **store** — `src/modules/store/routes.rs:77`,
  `StoreModule::init`: same one-liner with `self.state`.
- **payments** — `src/modules/payments/module.rs:217`,
  `PaymentsModule::init` (`records::*` is already glob-imported
  at line 167, so the free function resolves directly):
  ```rust
  if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
      hydrate_from_db(&pool)?;
  }
  ```
- **esg** — `src/modules/esg/module.rs:93`, `EsgModule::init`:
  same one-liner with `self.state`.
- **forums** — `src/modules/forums/module.rs:94`,
  `ForumsModule::init`: same one-liner with `self.state`.

Ordering: store declares dependencies on licensing + billing,
so module init order already runs billing before store. No
hydrate reads another module's tables, so no further ordering
constraint exists.

## 5. Restart-survival reasoning (code-level analysis, no live PostgreSQL)

Seed → restart → read back, per module. "Survives" means: after
a process restart with the §4 call wired, the map is rebuilt
from the same rows.

- **finalize_jobs — survives only after the documents-metadata
  follow-up.** The write-through, claim arbitration, reaper,
  and re-sync are all wired (§2.1), but `db_insert_job`
  refuses to insert a job whose session/document rows do not
  exist in the DB, and sessions/documents are still
  in-memory-only. Until they are persisted, a restarted worker
  finds no job rows and re-scans from received sessions exactly
  as before (no regression, no durability yet). After the
  follow-up: seed a job → restart → `start_finalize_worker`
  runs `db_sync_finalize_jobs()` before `rescan_finalize_jobs()`
  → the open row is adopted (or its terminal state honoured),
  so a `verified` job is never re-hashed and a `finalizing`
  job claimed by another instance is never double-claimed.
- **billing — survives (after §4).** Invoices, items,
  payments, credit notes, and sequence seeds are rebuilt from
  the tables. Does NOT survive: subscription-lifecycle mirrors
  and dunning state (rebuilt from events at runtime, not from
  rows) and school-fees maps.
- **store — survives (after §4).** Products, tiers, orders +
  items, entitlements, downloads, catalog, issued licenses are
  rebuilt. Does NOT survive: coupons, carts, subscriptions,
  digital products/files (no hydrate in this slice).
- **payments — survives (after §4).** Connections, intents,
  transactions (plus the idempotency index), methods, gateway
  subscriptions, split bills + parts are rebuilt. Does NOT
  survive, by design: webhook-event dedup (TTL-bounded),
  pending checkouts (in-flight), cached PayPal tokens
  (expiring cache) — these are re-created by live traffic.
- **esg — survives (after §4).** All 7 tables are rebuilt;
  nothing at table level is left behind.
- **forums — survives (after §4).** All 13 content tables +
  settings are rebuilt; unknown-visibility / bad-vote rows are
  skipped (they were never valid runtime rows).
- **licensing — already survives for licenses + activations**
  (direct Diesel reads with map fallback; creates insert via
  Diesel). Products / releases / distributions / events /
  offline tokens / activation history do NOT survive (map-only).

## 6. What remains — follow-up task definitions

Tracker-ready (project id 1, normal priority). To be created by
the coordinator — see the tracker blocker in §0. Task 271
closes for the delivered slice only; these carry the rest.

1. **Documents session/document persistence + finalize pool attach (REQUIRED for finalize durability).**
   Persist upload sessions and documents through the DB pool
   (tables exist: `document_upload_sessions`, `documents`),
   then add the one-line `attach_finalize_pool` call in
   `DocumentsModule::init` (§4). Until this lands, the
   finalize write-through's FK guard keeps jobs
   in-memory-only. Owner: documents-metadata worker.
2. **Hydrate call wiring (one line per module init).**
   Add the §4 calls in billing, store, payments, esg, and
   forums `init`. Verify each module's restart survival with a
   live database (seed → restart → read back).
3. **Payments write-through + runtime `tenant_id` stamping.**
   Add `tenant_id` to the runtime payment structs (needs
   struct-literal changes across `handlers_*.rs`), write
   mutations through Diesel, and stamp the 000126 columns on
   insert. Includes split-bill and bank-transfer-verification
   tenant stamping.
4. **Billing write-through.** Write invoice / payment / credit
   note mutations through Diesel (currently hydrate-only, so a
   crash between mutation and the next restart loses the
   change); persist school-fees maps and the
   subscription-lifecycle / dunning mirrors (or document them
   as intentionally rebuildable from events).
5. **Store write-through + coupons + carts/subscriptions/digital.**
   Write store mutations through Diesel; decide the coupon
   `discount_value` encoding (percent-bp vs fixed-minor) and
   record it in the writer before hydrating coupons; add
   `store_carts` / `store_subscriptions` tables (new
   migration) or document carts/subscriptions as ephemeral;
   hydrate `digital_*`.
6. **Licensing auxiliary maps DB backing.** Hydrate/write-through
   (or direct Diesel) for license products, releases,
   distributions, events, offline tokens, activation history.
   Tables + models already exist (000069); only the read/write
   path is missing.
7. **Forums per-tenant settings.** Decide whether
   `forum_settings` becomes per-tenant (today it is
   installation-wide and hydrate applies it only when exactly
   one row exists).
8. **Remaining 120+ files/modules of the program.** Module by
   module, same pattern as this slice: check the DDL target
   exists, pick hydrate / write-through / direct Diesel, wire
   it, record the choice here. Lock / dedup / rate-limit maps
   and caches with a DB behind them stay in memory.

## 7. Verification

- `rustfmt --edition 2024 --check` (direct rustfmt binary,
  1.11.0-stable) on every changed `.rs` file. Fully clean
  (exit 0, zero diffs): `src/modules/store/state.rs`,
  `src/modules/esg/state.rs`, `src/modules/forums/state.rs`,
  and all 10 changed `unified-db-*` schema/model files
  (platform `schema/{documents,esg,forums}.rs`,
  `models/{documents,esg,forums}.rs`, `schema.rs`,
  `models.rs`; commerce `schema/payments.rs`,
  `models/payments.rs`).
- Remaining diffs are **pre-existing fmt debt only** — every
  hunk was checked against the author's own lines and left
  untouched per the "do NOT reformat the tree" rule:
  - `finalize.rs`: 4 hunks @640, 742, 878, 906
    (`has_starving_finalize_job`, `verify_finalize_job`
    `ok_or_else` ×2, `test_job` / `test_session` tenant_id).
  - `billing/state.rs`: 11 hunks @455, 507, 533, 608, 1206,
    1232, 1336, 1557, 1570, 1588, 1611 (`line_tax`,
    `sync_subscription_lifecycle`, fee tests).
  - `payments/records.rs`: 25 hunks @28, 73, 117, 137, 193,
    225, 249, 257, 263, 271, 279, 287, 295, 303, 308, 314,
    319, 332, 339, 344, 350, 355, 633, 644, 685 (blank-line
    style + registry type alias).
  Full logs: `/tmp/c11-verify/` (`*.log2`, `SUMMARY.txt`).
- Re-read verification: the edited `finalize.rs` regions were
  read back after editing — DB helpers (FK guards, atomic
  claim, reaper, finish, loaders), `db_sync_finalize_jobs`,
  claim rollback, reap mirror, `start_finalize_worker` /
  `finalize_tick` / `run_finalize_job` wiring all checked
  (borrow/field access via `RefMut` deref, `cutoff` in scope
  in the reaper, `drop(entry)` before the DB claim call).
- NOT run (per task instructions): `cargo check` / `cargo
  test` — coordinator runs lib-crate checks centrally. No
  live PostgreSQL was available, so §5 is analysis, not an
  observed run.

## 8. Files changed in this slice

- `crates/unified-core/migrations/2026-11-23-000126_c11_db_read_path/{up,down}.sql` (new)
- `src/modules/documents/state/finalize.rs` (write-through)
- `src/modules/billing/state.rs` (`hydrate_from_db`)
- `src/modules/store/state.rs` (`hydrate_from_db`)
- `src/modules/payments/records.rs` (`hydrate_from_db`)
- `src/modules/esg/state.rs` (`hydrate_from_db`)
- `src/modules/forums/state.rs` (`hydrate_from_db`)
- `crates/unified-db-platform/src/schema/{documents,esg,forums}.rs`,
  `models/{documents,esg,forums}.rs`, `schema.rs`, `models.rs`
- `crates/unified-db-commerce/src/schema/payments.rs`,
  `models/payments.rs`
- `docs/integration/INTEGRATION_NOTES_C11.md` (this file)
