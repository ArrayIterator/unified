# Database Schema Conventions and Audit

Project: Unified Pentagonal Project — Rust, Actix-web 4, Diesel 2.2 +
PostgreSQL. This document states the schema rules every table must follow,
records the audit of the schema as it exists after migration
`2026-11-22-000003_schema_audit`, and lists the follow-ups that were
deliberately left open. The three sources of truth must always agree:

- `migrations/*/up.sql` (the DDL that creates the tables),
- `src/db/schema.rs` (the Diesel `table!` model of the same tables),
- `src/db/models.rs` (Queryable / Insertable pairs for every table).

After migration `2026-11-22-000003_schema_audit` all three describe the
same 29 tables; see "Consistency check" below.

## 1. Conventions

1. **Naming.** Table and column names are `snake_case`. Table names are
   plural (`users`, `store_orders`, `vcs_connections`). The two deliberate
   exceptions are documented in section 4: `store_catalog` and
   `vcs_provider_settings` use natural keys.
2. **Primary keys.** Every table has a UUID primary key
   (`UUID PRIMARY KEY DEFAULT gen_random_uuid()`), except:
   - junction tables (`user_roles`, `software_tag_assignments`), which use
     a composite primary key of their two foreign keys — a surrogate id
     would add nothing;
   - tables with a deliberate natural key (see section 4).
3. **Timestamps.** Every table has `created_at TIMESTAMPTZ NOT NULL
   DEFAULT now()` and `updated_at TIMESTAMPTZ NOT NULL DEFAULT now()`.
   Junction and event rows are insert/delete-only in practice, but the
   columns exist anyway so the convention holds literally; on those rows
   `updated_at` normally equals `created_at`.
4. **Foreign keys.** Every column that references another table declares a
   foreign key with an explicit `ON DELETE` rule (`CASCADE` when the child
   row is meaningless without the parent, `SET NULL` when it can outlive
   it). `ON UPDATE` defaults to `NO ACTION` on existing constraints
   (section 4); new foreign keys should state both rules explicitly.
   UUID primary keys are never updated, so the default is safe in
   practice.
5. **Indexes.** Every foreign key column has an index, and so does every
   column that is filtered or searched on (`status`, `slug` lookups,
   `expires_at` sweeps, provider filters). Uniqueness is enforced by a
   `UNIQUE` constraint or unique index, never by application code alone.
   A unique index that includes a nullable column uses `COALESCE` on the
   nullable part so the rule also applies when that part is `NULL`
   (plain `UNIQUE` treats `NULL`s as distinct).
6. **CHECK constraints.** Every status / enum column has a `CHECK` with
   the exact value list the application code validates (see section 3).
   Counters, sizes, and money amounts have non-negative `CHECK`s unless
   a negative value is meaningful (none is today). Currency columns check
   the ISO 4217 shape (3 ASCII letters) that `store.rs` validates;
   country codes check the ISO 3166-1 alpha-2 shape (2 ASCII letters)
   that `geo.rs` validates.
7. **Money is `NUMERIC(19,4)` — never `DOUBLE PRECISION` / float.**
   Binary floating point cannot represent decimal money exactly, so
   every money column is `NUMERIC(19,4)` in SQL and Diesel `Numeric`
   in `schema.rs`. The Rust type is `diesel::data_types::PgNumeric`,
   the built-in Diesel type for `NUMERIC` when neither the `bigdecimal`
   nor the `rust_decimal` Cargo feature is enabled (both would be new
   dependencies and are out of scope for this audit). `PgNumeric` has
   no serde implementation, so the five money models in `models.rs`
   (`StoreProduct`, `StorePricingTier`, `StoreCoupon`, `StoreOrder`,
   `StoreOrderItem`) derive Queryable / Selectable / Insertable only,
   without Serialize / Deserialize; the API layer must format money
   values explicitly. If the project later approves a decimal crate,
   switching those fields to it restores the serde derives.
8. **No renames that break modules.** A table or column is never renamed
   in an audit migration if code in `src/` depends on the old name;
   the rename is recorded as a follow-up instead.

## 2. How to add a new table

1. Create the next migration directory:
   `migrations/YYYY-MM-DD-NNNNNN_<name>/` with `up.sql` and `down.sql`.
   Numbering is chronological: the next free number after the highest
   existing one. `down.sql` must reverse `up.sql` exactly (drop added
   indexes / constraints / columns, restore changed column types).
2. In `up.sql`: `CREATE TABLE` with a UUID primary key, `created_at` /
   `updated_at`, foreign keys with explicit `ON DELETE` (and `ON UPDATE`
   where it matters), `CHECK` constraints for every status / enum and
   for value ranges, indexes on every foreign key and every filtered /
   searched column, and unique constraints where the domain requires
   them. Money columns are `NUMERIC(19,4)`. Use `CREATE INDEX IF NOT
   EXISTS` / `CREATE UNIQUE INDEX IF NOT EXISTS` so a re-run is safe.
3. Add the matching `diesel::table!` block to `src/db/schema.rs`, plus
   `joinable!` entries for each foreign key (one per table pair — Diesel
   allows only one `joinable!` between two tables, so if two columns of
   one table reference the same table, only one gets a `joinable!`;
   document the other one) and an entry in
   `allow_tables_to_appear_in_same_query!`.
4. Add a Queryable / Insertable pair to `src/db/models.rs` following the
   existing pattern: the Queryable struct lists every column in schema
   order; the Insertable struct lists every column except `created_at` /
   `updated_at` (and other defaulted columns), which the database fills.
   Money fields use `PgNumeric` and skip the serde derives (rule 7).
5. Verify the three-way consistency (section 5) before handing over.

## 3. Audit findings — per-table verdicts

Verdicts: **compliant** (already follows the conventions),
**fixed** (the audit migration changed it), **needs-follow-up** (a known
deviation that was deliberately not fixed; see section 4).

| Table | Verdict | Notes |
|---|---|---|
| `users` | compliant | UUID PK, timestamps, `email` unique, `company_id` FK + index, `is_active` index. |
| `roles` | compliant | UUID PK, timestamps, `slug` unique. |
| `user_roles` | fixed | Added `updated_at`. Composite PK `(user_id, role_id)` is the junction-table exception; `role_id` indexed. |
| `companies` | compliant | UUID PK, timestamps, `slug` unique. |
| `company_sites` | needs-follow-up | Otherwise compliant (`domain` unique, `company_id` FK + index). One-primary-per-company rule not enforced in SQL — see section 4. |
| `domains` | fixed | Added `ssl_status` CHECK (`none`, `pending`, `active`, `expired`, `renewing`, `failed`; values from `domains.rs` / `hosting.rs` / `saas.rs`, `failed` reserved for the SSL flow) and `ssl_expires_at` index. |
| `geo_overrides` | fixed | Added `country_code` CHECK (2 ASCII letters; `geo.rs` validation). `cidr` unique, `is_active` / `country_code` indexed. |
| `licenses` | fixed | Added `status` CHECK (`active`, `suspended`, `revoked`, `expired`; the exact list validated in `licensing.rs`), `max_activations >= 1` CHECK (validated in `licensing.rs`), and `expires_at` index. `license_key` unique. |
| `license_activations` | fixed | Added `updated_at`. `(license_id, instance_id)` unique. |
| `software_taxonomy` | fixed | Added `depth >= 0` CHECK. Kind CHECK, `(kind, parent, slug)` unique index, `parent_id` / `kind` indexes already present. |
| `software` | compliant | Kind / status CHECKs, `slug` unique, all FKs + indexes present. |
| `software_versions` | compliant | `(software_id, version)` unique, `software_id` / `product` indexed. `file_id` has no FK by design — see section 4. |
| `software_releases` | fixed | Added `updated_at`, `download_count >= 0` CHECK, `channel` index, and unique `(version_id, channel)` (one release per version + channel). |
| `release_files` | fixed | Added `size_bytes >= 0` CHECK. |
| `software_tags` | compliant | `slug` unique. |
| `software_tag_assignments` | fixed | Added `updated_at`. Composite PK is the junction-table exception; `tag_id` indexed. |
| `store_products` | fixed | Money `DOUBLE PRECISION` -> `NUMERIC(19,4)` (`price`, `renewal_price`); added currency-shape CHECK, `price >= 0`, `renewal_price >= 0`, `trial_days >= 0`, `updates_years >= 0` CHECKs. |
| `store_pricing_tiers` | fixed | Money -> `NUMERIC(19,4)` (`price`, `renewal_price`); added `price >= 0`, `renewal_price >= 0`, `trial_days >= 0` CHECKs. |
| `store_coupons` | fixed | Money -> `NUMERIC(19,4)` (`discount_value`); added discount CHECK (non-negative; percent is `(0, 100]`, per `store.rs`), `used_count >= 0`, `usage_limit >= 0` CHECKs, and `expires_at` index. |
| `store_orders` | fixed | Money -> `NUMERIC(19,4)` (`subtotal`, `discount_total`, `total`); added currency-shape CHECK, non-negative totals CHECKs, and `provider` index. `user_id` is `VARCHAR` with no FK — see section 4. |
| `store_order_items` | fixed | Added `created_at` / `updated_at`; money -> `NUMERIC(19,4)` (`unit_price`, `line_total`); added `pricing_model` / `billing_period` CHECKs (values validated in `store.rs`), `unit_price >= 0`, `line_total >= 0` CHECKs, and indexes on `tier_id`, `software_id`, `company_id`, `software_slug`. |
| `store_entitlements` | fixed | Added indexes on `product_id`, `tier_id`, `software_id`, `release_id`, `license_key`, `expires_at`; added `downloads_used >= 0` CHECK. `user_id` is `VARCHAR` with no FK — see section 4. |
| `store_downloads` | fixed | Added `created_at` / `updated_at`; added indexes on `order_id`, `software_slug`. |
| `store_catalog` | fixed | Added indexes on `release_id`, `kind`. Primary key is the natural `slug` — see section 4. |
| `store_issued_licenses` | fixed | Added `updated_at`, index on `company_id`, unique index on `license_key`. `user_id` is `VARCHAR` with no FK — see section 4. |
| `vcs_connections` | compliant | Provider / `sync_status` CHECKs, `(provider, owner, repo, host)` unique index, FK + filter indexes present. |
| `vcs_version_drafts` | needs-follow-up | Otherwise compliant (provider / source CHECKs, `(connection_id, tag)` unique, FK + status indexes). No `status` CHECK — see section 4. |
| `vcs_sync_history` | fixed | Added `created_at` / `updated_at`. |
| `vcs_provider_settings` | compliant | Primary key is the natural `provider` — see section 4. |

### Migration `2026-11-22-000003_schema_audit` — summary of changes

- **Timestamps added (11 columns):** `software_releases.updated_at`,
  `store_order_items.created_at` + `updated_at`,
  `store_downloads.created_at` + `updated_at`,
  `store_issued_licenses.updated_at`,
  `vcs_sync_history.created_at` + `updated_at`,
  `user_roles.updated_at`, `software_tag_assignments.updated_at`,
  `license_activations.updated_at`. All `NOT NULL DEFAULT now()`,
  which backfills existing rows with the migration time.
- **Money converted (10 columns, `DOUBLE PRECISION` -> `NUMERIC(19,4)`):**

  | Column | Before | After |
  |---|---|---|
  | `store_products.price` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_products.renewal_price` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_pricing_tiers.price` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_pricing_tiers.renewal_price` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_coupons.discount_value` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_orders.subtotal` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_orders.discount_total` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_orders.total` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_order_items.unit_price` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |
  | `store_order_items.line_total` | `DOUBLE PRECISION` | `NUMERIC(19,4)` |

  No money column remains `DOUBLE PRECISION` in the final schema:
  `schema.rs` contains zero `Float8` fields.
  Conversion uses `USING (col::numeric(19,4))`. Values with more than
  4 fractional digits would be rounded by the cast; on a pre-launch
  database the stored values are short decimals, so no rounding is
  expected. The old `000002` migration file is intentionally left
  untouched (it may already have run); the conversion happens here.
- **CHECK constraints added (27):** see the per-table rows above and
  the migration file for the exact names (`chk_*`).
- **Indexes added (20 plain + 2 unique):** every foreign key that
  lacked one (`store_order_items.tier_id` / `software_id` /
  `company_id`, `store_entitlements.product_id` / `tier_id` /
  `software_id` / `release_id`, `store_downloads.order_id`,
  `store_catalog.release_id`, `store_issued_licenses.company_id`),
  plus filtered / searched columns (`software_slug` on order items
  and downloads, `license_key` on entitlements, `kind` on the
  catalog, `channel` on releases, `provider` on orders,
  `expires_at` on coupons / licenses / entitlements / domains).
- **Unique constraints added (2):** `store_issued_licenses.license_key`
  (mirrors the unique `licenses.license_key`) and
  `software_releases (version_id, channel)`.

## 4. Deliberately not fixed — follow-ups

1. **Store `user_id` is `VARCHAR`, not a UUID foreign key.**
   `store_orders.user_id`, `store_entitlements.user_id`, and
   `store_issued_licenses.user_id` are `VARCHAR` and have no foreign
   key to `users(id)`. Changing the type to `UUID` would break the
   store module, which uses string user ids throughout. Follow-up for
   the store module: adopt UUID user ids, then change these columns to
   `UUID REFERENCES users (id) ON DELETE SET NULL` and index them.
2. **`file_id` columns have no foreign key.** `software_versions.file_id`,
   `store_products.file_id`, `store_order_items.file_id`, and
   `store_catalog.file_id` reference files without a referential
   target. `software_versions.file_id` is intentionally unlinked (the
   reverse reference `release_files.version_id` already links files to
   versions; a second link would be circular — see the `000002`
   migration notes). The others need a decision on what a "file" is
   (a `release_files` row or a dedicated files table) before a foreign
   key can be added.
3. **One primary site per company is not enforced in SQL.**
   `company_sites.is_primary` should be unique per company, but the
   partial unique index `(company_id) WHERE is_primary` was NOT added:
   `company.rs` inserts a new primary site first and demotes the old
   one afterwards, so the index would reject the insert. Follow-up:
   reorder that code to demote before insert (ideally in one
   transaction), then add the partial unique index.
4. **No unique index on `release_files (release_id, filename)`.**
   Re-uploading a file with the same name into the same release would
   be rejected by the index while the in-memory store accepts it.
   Needs a replace-on-re-upload decision in the software module first.
5. **No `CHECK` on `vcs_version_drafts.status`.** Only `'draft'` is
   ever written today; the later lifecycle states (published /
   discarded / …) are undefined, so any list would be invented.
   Add the `CHECK` once the draft lifecycle is defined.
6. **Existing foreign keys keep the default `ON UPDATE NO ACTION`.**
   Their constraints were created inline in the `000001` / `000002`
   migrations, so their names are generated; rewriting them would need
   constraint-name lookups. UUID primary keys are never updated, so
   the default is safe. New foreign keys should state both rules.
7. **Natural primary keys kept.** `store_catalog` is keyed by `slug`
   (the update-API catalog is looked up by slug; the slug is the
   identity) and `vcs_provider_settings` by `provider` (one row per
   provider). Both are deliberate; do not "fix" them with surrogate
   UUIDs.
8. **Diesel `joinable!` limits.** `software.category_id` also references
   `software_taxonomy`, but Diesel allows only one `joinable!` between
   two tables, so only `group_id` has one. `software_taxonomy.parent_id`
   is a self-reference and has no `joinable!`. Both are queryable with
   explicit `ON` clauses instead.
9. **`PgNumeric` ergonomics.** Money fields use the raw
   `diesel::data_types::PgNumeric` type, which has no serde support
   and no convenient decimal constructors. If the project later
   approves a new dependency, `bigdecimal` or `rust_decimal` (both
   with serde support) would be the better money type; that change
   needs a decision, not an audit migration.
10. **Numeric `CHECK`s not added where the semantics are undecided.**
    `seats`, `download_limit`, `quantity`, and `usage_limit` have no
    lower-bound `CHECK` because `0` might mean "unlimited" in the
    store module; that needs a product decision before a constraint
    can be written.

## 5. Consistency check

Verified by script (not by running the database — no PostgreSQL is
available in this environment, and `cargo check` cannot run here
because there is no Rust toolchain):

- The three `up.sql` files create 29 tables; `schema.rs` models 29
  tables; `models.rs` models 29 tables. The three sets are identical.
- Every Queryable struct in `models.rs` lists exactly the columns of
  its `schema.rs` table, in the same order (29/29 match). Insertable
  structs list the same columns minus `created_at` / `updated_at` and
  other defaulted columns, following the pre-existing pattern.
- `schema.rs` contains zero `Float8` fields; all 10 money columns are
  `Numeric`.
- Every table in `schema.rs` has `created_at` and `updated_at`.
- The migration, `schema.rs`, and `models.rs` were grepped for stub
  macros: 0 hits in code. All code, comments, and
  identifiers are in English.

Riskiest Diesel API surfaces used (unverified by compilation, no
toolchain here): `diesel::data_types::PgNumeric` as the Rust type for
`sql_types::Numeric` (the documented built-in type when neither the
`bigdecimal` nor the `rust_decimal` feature is enabled); the `Numeric`
type name inside `table!`; the six new `joinable!` entries; and the
`Queryable` / `Selectable` derives on structs containing `PgNumeric`.
