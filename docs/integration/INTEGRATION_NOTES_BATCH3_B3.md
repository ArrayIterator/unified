# Integration notes — Tax Management module (Batch 3, builder B3)

Module: `tax` — `src/modules/tax.rs` (new), migration
`migrations/2026-11-22-000038_tax/` (new: `up.sql`, `down.sql`).
This file is the exact snippet set for the integrator. Do NOT edit
`src/modules/tax.rs` or the migration while applying these snippets;
if a snippet conflicts with another batch-3 module's snippet, keep
both and renumber/reconcile in the order the integrator applies them.

## 0. What this module is

- Module id `tax`, name "Tax Management", version default `1.0.0`.
- `requires = ["billing", "settings", "notifications", "scheduler",
  "audit"]`. `available_as_product = false`, no `product_category`.
- `route_version_support` is `All` (declared explicitly).
- 26 routes under `/api/v1/tax`, all listed in `route_info`.
- In-memory DashMap store is authoritative; the migration is the
  persistence target (same convention as the other modules).
- Money is `NUMERIC(19,4)` in SQL and `f64` rounded to 4 decimal
  places in Rust (matching the billing module's `f64` convention).
  `rate_percent` is a percentage (11.0000 means 11%), NOT a
  fraction — the billing module stores per-line rates as fractions
  (0.10 means 10%).

## 1. `src/modules/mod.rs`

Add the module declaration in alphabetical order, between `tasks`
and `theme`:

```rust
pub mod tasks;
pub mod tax;
pub mod theme;
```

Add the registration in the Commerce group, directly after the
billing registration (tax consumes billing events and requires the
billing module):

```rust
    // Commerce.
    store::register(registry);
    licensing::register(registry);
    software::register(registry);
    billing::register(registry);
    tax::register(registry);
    products::register(registry);
    payments::register(registry);
    bank_transfer::register(registry);
```

## 2. `src/db/schema.rs`

Append these `table!` blocks after the `project_activity` table
(the last table in the file). Then add the six table names to the
`allow_tables_to_appear_in_same_query!` list (after
`project_activity`), and update the file header comment's migration
range to end at `2026-11-22-000038_tax`. There are no foreign keys
and therefore no `joinable!` entries: `company_id` is a plain UUID
reference on purpose (deleting a company must not delete the tax
history), and `tax_computations.source_id` is a plain string
reference to an invoice or order id.

```rust
diesel::table! {
    tax_rules (id) {
        id -> Uuid,
        company_id -> Uuid,
        country_code -> Varchar,
        region_code -> Nullable<Varchar>,
        name -> Varchar,
        tax_type -> Varchar,
        rate_percent -> Numeric,
        threshold_amount -> Nullable<Numeric>,
        currency -> Varchar,
        inclusive -> Bool,
        compound -> Bool,
        effective_from -> Date,
        effective_to -> Nullable<Date>,
        sort_order -> Int4,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_settings (id) {
        id -> Uuid,
        company_id -> Uuid,
        fiscal_year_start_month -> Int4,
        fiscal_year_start_day -> Int4,
        filing_frequency -> Varchar,
        default_currency -> Varchar,
        default_country_code -> Nullable<Varchar>,
        default_region_code -> Nullable<Varchar>,
        reminder_days_before -> Int4,
        filing_due_days_after_period -> Int4,
        notify_user_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_computations (id) {
        id -> Uuid,
        company_id -> Uuid,
        source_type -> Varchar,
        source_id -> Varchar,
        country_code -> Nullable<Varchar>,
        region_code -> Nullable<Varchar>,
        currency -> Varchar,
        base_amount -> Numeric,
        tax_total -> Numeric,
        grand_total -> Numeric,
        lines -> Jsonb,
        computed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_filings (id) {
        id -> Uuid,
        company_id -> Uuid,
        country_code -> Varchar,
        region_code -> Nullable<Varchar>,
        period_start -> Date,
        period_end -> Date,
        frequency -> Varchar,
        due_date -> Date,
        status -> Varchar,
        tax_total -> Numeric,
        submitted_at -> Nullable<Timestamptz>,
        submitted_via -> Nullable<Varchar>,
        reference -> Nullable<Varchar>,
        last_reminder_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_connectors (id) {
        id -> Uuid,
        company_id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint_url -> Varchar,
        allowed_hosts -> Jsonb,
        auth_scheme -> Varchar,
        credential_encrypted -> Nullable<Text>,
        credential_fingerprint -> Nullable<Varchar>,
        signing_secret_encrypted -> Nullable<Text>,
        signing_secret_fingerprint -> Nullable<Varchar>,
        timeout_ms -> Int4,
        max_retries -> Int4,
        enabled -> Bool,
        request_mapping -> Jsonb,
        response_mapping -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tax_audit (id) {
        id -> Uuid,
        company_id -> Uuid,
        actor -> Varchar,
        action -> Varchar,
        resource_type -> Varchar,
        resource_id -> Varchar,
        details -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

## 3. `src/db/models.rs`

Append after the project models at the end of the file, under a
`// Tax Management (2026-11-22-000038_tax).` section header.

First extend the chrono import (this file currently has no
`NaiveDate` import):

```rust
use chrono::{DateTime, NaiveDate, Utc};
```

Models that carry money (`PgNumeric`) skip the serde derives,
following the existing convention in this file (`PgNumeric` has no
serde implementation). `New*` structs carry every column except
`created_at` / `updated_at` and except columns the database fills
with a DEFAULT (primary-key columns are always carried, callers
supply ids). Secret columns carry `#[serde(skip_serializing)]` on
the read struct, like `User.password_hash`.

```rust
// ---------------------------------------------------------------------------
// Tax Management (2026-11-22-000038_tax).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_rules)]
pub struct TaxRule {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: PgNumeric,
    pub threshold_amount: Option<PgNumeric>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_rules)]
pub struct NewTaxRule {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: PgNumeric,
    pub threshold_amount: Option<PgNumeric>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_settings)]
pub struct TaxSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub fiscal_year_start_month: i32,
    pub fiscal_year_start_day: i32,
    pub filing_frequency: String,
    pub default_currency: String,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: i32,
    pub filing_due_days_after_period: i32,
    pub notify_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_settings)]
pub struct NewTaxSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub fiscal_year_start_month: i32,
    pub fiscal_year_start_day: i32,
    pub filing_frequency: String,
    pub default_currency: String,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: i32,
    pub filing_due_days_after_period: i32,
    pub notify_user_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_computations)]
pub struct TaxComputation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_type: String,
    pub source_id: String,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub currency: String,
    pub base_amount: PgNumeric,
    pub tax_total: PgNumeric,
    pub grand_total: PgNumeric,
    pub lines: Value,
    pub computed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_computations)]
pub struct NewTaxComputation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_type: String,
    pub source_id: String,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub currency: String,
    pub base_amount: PgNumeric,
    pub tax_total: PgNumeric,
    pub grand_total: PgNumeric,
    pub lines: Value,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_filings)]
pub struct TaxFiling {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub tax_total: PgNumeric,
    pub submitted_at: Option<DateTime<Utc>>,
    pub submitted_via: Option<String>,
    pub reference: Option<String>,
    pub last_reminder_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_filings)]
pub struct NewTaxFiling {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub tax_total: PgNumeric,
    pub submitted_at: Option<DateTime<Utc>>,
    pub submitted_via: Option<String>,
    pub reference: Option<String>,
    pub last_reminder_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_connectors)]
pub struct TaxConnector {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Value,
    pub auth_scheme: String,
    #[serde(skip_serializing)]
    pub credential_encrypted: Option<String>,
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing)]
    pub signing_secret_encrypted: Option<String>,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: i32,
    pub max_retries: i32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_connectors)]
pub struct NewTaxConnector {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Value,
    pub auth_scheme: String,
    pub credential_encrypted: Option<String>,
    pub credential_fingerprint: Option<String>,
    pub signing_secret_encrypted: Option<String>,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: i32,
    pub max_retries: i32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_audit)]
pub struct TaxAuditEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_audit)]
pub struct NewTaxAuditEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
}
```

## 4. `README.md`

Append after item 71 (**Project Management**) in the `## Core
modules` numbered list, and update the module total. Note: other
batch-3 modules also append items, so the integrator must renumber
if several land in the same round; the text below assumes tax lands
as item 72.

```markdown
72. **Tax Management** — tax rules per jurisdiction (country/region
    code, rate, thresholds, effective dates, inclusive vs exclusive,
    compound flag), per-company tax settings (rates, thresholds,
    fiscal year start, filing frequency), pure tax computation for
    invoices and orders with a line-by-line breakdown (the source
    document is never mutated), tax reports (summary, per
    jurisdiction, per period) exportable as JSON or CSV, filing
    reminders before each deadline, and a generic HTTP connector
    framework for tax reporting/filing providers with SSRF-protected
    endpoints, write-only encrypted credentials, request signing,
    timeout and retry policy, response mapping, plus a clearly-marked
    example provider mapping that never performs a network call.
```

Change the total line below the list from `72 registered modules`
to `73 registered modules` (again, renumber against the other
batch-3 additions).

## 5. `TODO.md`

Append this section at the end of the file:

```markdown
## Tax Management module (2026-11-22) — Batch 3 B3

- [x] Tax rules per jurisdiction (country/region, rate, thresholds,
  effective dates, inclusive vs exclusive, compound flag), full CRUD
  under `/api/v1/tax/rules`, company-scoped.
- [x] Per-company tax settings (rates, thresholds, fiscal year
  start, filing frequency, reminder window, due-days-after-period,
  default jurisdiction and currency) under `/api/v1/tax/settings`.
- [x] Pure tax computation (`compute_tax_breakdown`) over (amount,
  jurisdiction, rules, date) with a line-by-line breakdown;
  `POST /api/v1/tax/compute` plus stored computations under
  `/api/v1/tax/computations`.
- [x] Billing interconnection: subscribes to `invoice.created` and
  `order.created` (and the `billing.invoice_created` /
  `billing.order_created` alias spellings, which do not exist in the
  billing module — see the discrepancy note below) and emits
  `tax.computed`; the invoice/order itself is never mutated.
- [x] Tax reports: summary, per-jurisdiction, per-period, plus JSON
  and CSV export under `/api/v1/tax/reports/*` (CSV is produced
  directly by the export endpoint, not through the import/export
  module's `export.requested` event).
- [x] Filing reminders: `tax.filing.check_requested` hook plus
  `POST /api/v1/tax/filings/check-due`; emits `tax.filing_due` and
  `notification.requested` before each filing deadline per company
  settings. There is no scheduler job kind for this check (the
  scheduler only dispatches its own fixed kinds); the check must be
  triggered through the hook or the endpoint.
- [x] Filing submission: manual or through a connector
  (`POST /api/v1/tax/filings/{id}/submit`), emitting
  `tax.filing_submitted`.
- [x] Connector framework: generic HTTP connector with allowlisted
  hosts, SSRF protection (HTTPS only, no userinfo, public resolved
  addresses only), write-only encrypted credentials (versioned `v1.`
  HMAC keystream + HMAC tag, fingerprint only in responses),
  request signing for the `hmac` scheme, timeout + retry policy,
  request/response mapping, plus one clearly-marked `example`
  provider mapping that never performs a network call.
- [x] Full audit trail: every tax record change is recorded in the
  module's own `tax_audit` table and emits an audit-shaped `tax.*`
  event (`actor_id`, `resource`, `id` in the payload).
- [ ] Integrator: add the `tax.*` event names to `AUDITED_EVENTS` in
  `src/modules/audit.rs` (see section 6 below) so the audit module
  records them; the module's own `tax_audit` table already records
  everything regardless.
- [ ] Integrator: verify the migration applies cleanly
  (`2026-11-22-000038_tax`) and `cargo check` passes; no toolchain
  exists in the builder sandbox, so compilation was never run.
- [ ] Live verification: real provider submission through a
  `generic_http` connector against a real tax provider endpoint
  (only the `example` kind was exercised by design, and it performs
  no network call).
```

## 6. `src/modules/audit.rs` — `AUDITED_EVENTS`

Add these names to the `AUDITED_EVENTS` list so the audit module
records the tax events (the tax module emits them in the audit
shape — `actor_id`, `resource`, `id` — but the audit module only
records listed events). There is NO `audit.record_requested` event
in this codebase: the audit module subscribes to named events, so
no such event is emitted by the tax module.

```rust
    // Tax Management (tax module).
    "tax.rule_created",
    "tax.rule_updated",
    "tax.rule_deleted",
    "tax.computed",
    "tax.settings_updated",
    "tax.filing_created",
    "tax.filing_due",
    "tax.filing_submitted",
    "tax.connector_created",
    "tax.connector_updated",
    "tax.connector_deleted",
    "tax.connector_tested",
```

## 7. Events (for the integrator's cross-check)

Emitted by this module:

| Event | Payload (key fields) |
|---|---|
| `tax.rule_created` / `tax.rule_updated` / `tax.rule_deleted` | `id`, `actor_id`, `resource`, `company_id`, `country_code`, `tax_type`, `rate_percent` |
| `tax.computed` | `computation_id`, `company_id`, `source_type`, `source_id`, `country_code`, `region_code`, `currency`, `base_amount`, `tax_total`, `grand_total`, `lines` |
| `tax.settings_updated` | `id`, `actor_id`, `resource`, `company_id`, `filing_frequency`, `default_currency` |
| `tax.filing_created` | `id`, `actor_id`, `resource`, `company_id`, `country_code`, `period_start`, `period_end`, `due_date` |
| `tax.filing_due` | `filing_id`, `company_id`, `country_code`, `region_code`, `period_start`, `period_end`, `due_date`, `status`, `tax_total` |
| `tax.filing_submitted` | `id`, `actor_id`, `resource`, `company_id`, `country_code`, `period_start`, `period_end`, `submitted_via`, `reference` |
| `tax.connector_created` / `tax.connector_updated` / `tax.connector_deleted` / `tax.connector_tested` | `id`, `actor_id`, `resource`, `company_id`, `name`, `kind`, `credential_configured`, `credential_fingerprint` (fingerprint only, never a credential value) |
| `notification.requested` | `company_id`, `user_id` (only when `notify_user_id` is configured), `title`, `body`, `kind: "tax"` |

Consumed by this module:

| Event | Behaviour |
|---|---|
| `invoice.created` | Compute tax for the invoice (company from `company_id`, else `user_id` via `user.company_requested`); store the computation; emit `tax.computed`. Never mutates the invoice. |
| `billing.invoice_created` | Alias spelling from the brief; same handler, de-duplicated by (`invoice`, source id). |
| `order.created` | Same as invoices, with source type `order`. |
| `billing.order_created` | Alias spelling from the brief; same handler, de-duplicated. |
| `tax.filing.check_requested` | Run the filing-reminder check (optionally scoped by `company_id` in the payload). |

## 8. Discrepancies found while reading the real code (integrator: read before wiring)

1. **Billing event names.** The brief names `billing.invoice_created`
   and `billing.order_created`. The billing module actually emits
   `invoice.created` (see `billing.rs` `create_invoice` /
   `from_order`) and consumes `order.created` (see its
   `register_hooks`). This module subscribes to all four names and
   de-duplicates by source id, so it works with the real events and
   with the brief's spellings.
2. **Audit event.** The brief says to emit `audit.record_requested`
   "if that is the audit module's event name". It is not: no
   `audit.record_requested` event exists anywhere in `src/`. The
   audit module records events listed in `AUDITED_EVENTS` in
   `audit.rs`. This module therefore emits its `tax.*` events in the
   audit shape (same pattern as the AI module's `emit_audit`) and
   records every change in its own `tax_audit` table; the integrator
   must add the names from section 6 to `AUDITED_EVENTS`.
3. **Scheduler.** There is no generic scheduler tick or
   register-your-kind mechanism: `scheduler.rs` dispatches only its
   fixed `KNOWN_KINDS`, and adding a kind would mean editing
   `scheduler.rs`. Filing reminders therefore run through the
   `tax.filing.check_requested` hook and the
   `POST /api/v1/tax/filings/check-due` endpoint, not through a
   scheduler job kind.
4. **CSV export.** The import/export module's `export.requested`
   event is a job-based flow for its own entities; this module's
   export is a synchronous read of its own store, so CSV is produced
   directly by `GET /api/v1/tax/reports/export?format=csv` (raw
   `text/csv` download, not the JSON envelope). JSON export uses the
   standard envelope.
5. **Migration number.** `2026-11-22-000038_tax` was free when this
   module was written and is still unique: neighbouring migrations
   now present are 000032 (timetracker), 000033 (storage_manager),
   000036 (ai_allowlists), 000037 (hr), and 000039 (advisor_risk).
   If the integrator renumbers any batch-3 migration, renumber this
   one and the references in `src/modules/tax.rs` and this file
   together.
6. **Rate representation.** Tax `rate_percent` is a percentage
   (11.0000 = 11%). Do not confuse it with the billing module's
   per-line fractional rates (0.10 = 10%).
7. **Unique index vs NULL region.** PostgreSQL treats NULLs as
   distinct in a UNIQUE constraint, so `uq_tax_filings_period`
   does not by itself forbid two country-wide (NULL region) filings
   for the same period; the module enforces that in code
   (`create_filing` conflict check, `run_filing_check` lookup).
