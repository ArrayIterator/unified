# Integration Notes — Batch 3, Builder B1: Calculator Enterprise

Module: **Calculator Enterprise** (`calculator`), a business/economics
calculation toolkit and a **submodule of Company Management**
(`requires = ["company"]`, `available_as_product = false`).

Files delivered by this builder (do not edit anything else):

- `src/modules/calculator.rs`
- `migrations/2026-11-22-000100_calculator/up.sql`
- `migrations/2026-11-22-000100_calculator/down.sql`
- `INTEGRATION_NOTES_BATCH3_B1.md` (this file)

## 1. `src/modules/mod.rs`

Add the module declaration in alphabetical order, after `pub mod cache;`
and before `pub mod certificates;`:

```rust
pub mod calculator;
```

Add the registration in `register_all`, in the Operations group next to
the Company module (after `company::register(registry);`):

```rust
calculator::register(registry);
```

## 2. `src/db/schema.rs`

Add after the last `diesel::table!` block present at integration time
(the projects tables are the last block at the time of writing; other
batch-3 builders append their own blocks — keep migration-number order
where possible and do not reorder existing blocks):

```rust
// ---------------------------------------------------------------------------
// Calculator Enterprise (2026-11-22-000100_calculator).
// ---------------------------------------------------------------------------

diesel::table! {
    calculator_saved_calculations (id) {
        id -> Uuid,
        company_id -> Uuid,
        calculator_type -> Varchar,
        title -> Varchar,
        inputs -> Jsonb,
        results -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_reports (id) {
        id -> Uuid,
        calculation_id -> Uuid,
        company_id -> Uuid,
        title -> Varchar,
        share_token -> Varchar,
        visibility -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_history (id) {
        id -> Uuid,
        company_id -> Uuid,
        calculator_type -> Varchar,
        inputs -> Jsonb,
        results -> Jsonb,
        actor -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_currency_rates (id) {
        id -> Uuid,
        base_currency -> Bpchar,
        target_currency -> Bpchar,
        rate -> Float8,
        provider -> Varchar,
        fetched_at -> Timestamptz,
        max_age_seconds -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Also add, next to the other `joinable!` lines:

```rust
diesel::joinable!(calculator_reports -> calculator_saved_calculations (calculation_id));
```

And append the four table names to the
`allow_tables_to_appear_in_same_query!` list:

```rust
    calculator_saved_calculations,
    calculator_reports,
    calculator_history,
    calculator_currency_rates,
```

Also update the schema header comment so the migration range reads
`2026-11-22-000001_core` through `2026-11-22-000100_calculator`
(or the highest migration number present at integration time).

Money note: `calculator_currency_rates.rate` is `Float8` (DOUBLE
PRECISION) on purpose — an exchange rate is a ratio, not money.
Monetary values live only inside the JSONB `inputs`/`results`
columns, rounded to four decimals by the module (NUMERIC(19,4)).

## 3. `src/db/models.rs`

Add after the last model block present at integration time (the
project models are the last block at the time of writing):

```rust
// ---------------------------------------------------------------------------
// Calculator Enterprise (2026-11-22-000100_calculator).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_saved_calculations)]
pub struct CalculatorSavedCalculation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_saved_calculations)]
pub struct NewCalculatorSavedCalculation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_reports)]
pub struct CalculatorReport {
    pub id: Uuid,
    pub calculation_id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_reports)]
pub struct NewCalculatorReport {
    pub id: Uuid,
    pub calculation_id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_history)]
pub struct CalculatorHistory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_history)]
pub struct NewCalculatorHistory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_currency_rates)]
pub struct CalculatorCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_currency_rates)]
pub struct NewCalculatorCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: i32,
}
```

`models.rs` already imports `Value` (`serde_json::Value`),
`DateTime<Utc>`, and `Uuid`, so no new imports are needed.

## 4. `README.md`

Add after item 12 (**Company Management**) in the `## Core modules`
list:

```markdown
13. **Calculator Enterprise** — business and economics calculation toolkit
    (ROI, break-even, loan amortisation, depreciation, NPV/IRR, cash-flow
    projection, pricing, salary, currency conversion, tax estimation) as a
    submodule of Company Management, with saved calculations, shareable
    reports, and calculation history.
```

Renumber the following items accordingly (13 becomes 14, and so on).

## 5. `TODO.md`

Append to the migration/module log (same style as the 000030/000031
entries):

```markdown
- Added `migrations/2026-11-22-000100_calculator/up.sql` and `down.sql`
  (Calculator Enterprise, submodule of Company Management): four tables —
  `calculator_saved_calculations`, `calculator_reports`,
  `calculator_history`, `calculator_currency_rates` (rate is DOUBLE
  PRECISION as a ratio, not money; money inside JSONB is rounded to four
  decimals). Module `src/modules/calculator.rs` (28 routes) is not yet
  wired into `src/modules/mod.rs` / `src/db/schema.rs` /
  `src/db/models.rs` until integration; the in-memory DashMap store is
  authoritative.
```

## 6. Behaviour the integrator must preserve

- Every route requires `X-User-Id` and `X-Role` headers and a
  `role.check` capability: `calculator:use` for calculations, reads,
  conversion, tax estimation, and settings reads; `calculator:manage`
  for creates, updates, deletes, rates, and settings writes. The
  check fails closed (no answer means not allowed).
- Tax estimation is delegated, never computed here: the module emits
  `tax.estimate_requested` and returns error code
  `tax_estimate_unavailable` (HTTP 422) when no handler answers.
  Do not wire a fallback tax computation.
- Currency conversion reads only stored rates (never hardcoded
  rates); a missing rate returns `currency_rate_unavailable`
  (HTTP 404), and a rate older than its `max_age_seconds` is
  returned with `stale: true`.
- IRR uses bounded bisection (max 200 iterations); non-convergence
  returns error code `irr_not_converged` (HTTP 422).
- Events emitted: `calculator.calculation_saved`,
  `calculator.report_shared`, `calculator.calculation_deleted`.
  Event answered: `calculator.rate_requested`.
- `cargo check` was never run for this module (no toolchain in this
  environment); run it at integration time.
