# Integration notes — batch 3, builder B2: Human Resource Management (`hr`)

Module file: `src/modules/hr.rs` (4,648 lines, 62 routes).
Migration: `migrations/2026-11-22-000037_hr/up.sql` + `down.sql` (14 tables).
Module id `hr`, `requires = ["users", "departments", "notifications", "audit"]`,
`available_as_product = false` (no `product_category`).
The in-memory DashMap store in `hr.rs` is authoritative; the migration and
the Diesel structs below are the persistence mapping for it.

The integrator applies the snippets below verbatim. Do NOT edit
`src/modules/hr.rs` or the migration while applying them. Counts in the
README lines (module / migration / table totals) must be recounted after
ALL parallel batch-3 builders land — the numbers below are the HR delta
only (+1 module, +1 migration, +14 tables).

## 1. `src/modules/mod.rs`

### 1a. Module declaration (alphabetical: after `hosting`, before `i18n`)

```rust
pub mod hosting;
pub mod hr;
pub mod i18n;
```

### 1b. Registration (People and support section, after `departments`, before `tickets`)

```rust
    crm::register(registry);
    departments::register(registry);
    hr::register(registry);
    tickets::register(registry);
```

## 2. `src/db/schema.rs`

Append the 14 `table!` blocks after the last existing `table!` block
(`project_activity`, migration 000031), before the `joinable!` section.
Append the 14 table names to `allow_tables_to_appear_in_same_query!`
(in migration order, after `project_activity`). Update the file header
comment to mention migration 000037. Add the `joinable!` lines below
after the existing `joinable!` lines.

```rust
// Human Resource Management (2026-11-22-000037_hr).

diesel::table! {
    hr_employees (id) {
        id -> Uuid,
        user_id -> Varchar,
        employee_code -> Varchar,
        position_id -> Nullable<Uuid>,
        department_id -> Nullable<Varchar>,
        department_verified -> Bool,
        manager_id -> Nullable<Uuid>,
        status -> Varchar,
        hire_date -> Date,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_positions (id) {
        id -> Uuid,
        title -> Varchar,
        department_id -> Nullable<Varchar>,
        level -> Nullable<Varchar>,
        salary_band_min -> Nullable<Numeric>,
        salary_band_max -> Nullable<Numeric>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_contracts (id) {
        id -> Uuid,
        employee_id -> Uuid,
        contract_type -> Varchar,
        start_date -> Date,
        end_date -> Nullable<Date>,
        salary_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_types (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        default_days_per_year -> Numeric,
        paid -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_balances (id) {
        id -> Uuid,
        employee_id -> Uuid,
        leave_type_id -> Uuid,
        year -> Int4,
        entitled_days -> Numeric,
        used_days -> Numeric,
        carried_over_days -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_requests (id) {
        id -> Uuid,
        employee_id -> Uuid,
        leave_type_id -> Uuid,
        start_date -> Date,
        end_date -> Date,
        days -> Numeric,
        status -> Varchar,
        reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        review_note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_attendance_records (id) {
        id -> Uuid,
        employee_id -> Uuid,
        user_id -> Varchar,
        work_date -> Date,
        minutes_worked -> Int4,
        source -> Varchar,
        entry_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_payroll_runs (id) {
        id -> Uuid,
        period -> Varchar,
        currency -> Varchar,
        status -> Varchar,
        total_gross -> Numeric,
        total_net -> Numeric,
        total_employees -> Int4,
        finalized_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_payroll_items (id) {
        id -> Uuid,
        payroll_run_id -> Uuid,
        employee_id -> Uuid,
        gross -> Numeric,
        adjustments -> Numeric,
        deductions -> Numeric,
        net -> Numeric,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_review_cycles (id) {
        id -> Uuid,
        name -> Varchar,
        period_start -> Date,
        period_end -> Date,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_reviews (id) {
        id -> Uuid,
        cycle_id -> Uuid,
        employee_id -> Uuid,
        reviewer_id -> Varchar,
        rating -> Nullable<SmallInt>,
        goals -> Jsonb,
        reviewer_notes -> Text,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_checklists (id) {
        id -> Uuid,
        employee_id -> Uuid,
        kind -> Varchar,
        status -> Varchar,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_checklist_items (id) {
        id -> Uuid,
        checklist_id -> Uuid,
        title -> Varchar,
        completed -> Bool,
        completed_by -> Nullable<Varchar>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_documents (id) {
        id -> Uuid,
        employee_id -> Uuid,
        title -> Varchar,
        document_type -> Varchar,
        storage_file_id -> Varchar,
        mime_type -> Nullable<Varchar>,
        size_bytes -> Nullable<BigInt>,
        uploaded_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

`joinable!` lines to add:

```rust
diesel::joinable!(hr_contracts -> hr_employees (employee_id));
diesel::joinable!(hr_leave_balances -> hr_employees (employee_id));
diesel::joinable!(hr_leave_balances -> hr_leave_types (leave_type_id));
diesel::joinable!(hr_leave_requests -> hr_employees (employee_id));
diesel::joinable!(hr_leave_requests -> hr_leave_types (leave_type_id));
diesel::joinable!(hr_attendance_records -> hr_employees (employee_id));
diesel::joinable!(hr_payroll_items -> hr_payroll_runs (payroll_run_id));
diesel::joinable!(hr_payroll_items -> hr_employees (employee_id));
diesel::joinable!(hr_reviews -> hr_review_cycles (cycle_id));
diesel::joinable!(hr_reviews -> hr_employees (employee_id));
diesel::joinable!(hr_checklists -> hr_employees (employee_id));
diesel::joinable!(hr_checklist_items -> hr_checklists (checklist_id));
diesel::joinable!(hr_documents -> hr_employees (employee_id));
```

`allow_tables_to_appear_in_same_query!` additions (after `project_activity`):

```rust
    hr_employees,
    hr_positions,
    hr_contracts,
    hr_leave_types,
    hr_leave_balances,
    hr_leave_requests,
    hr_attendance_records,
    hr_payroll_runs,
    hr_payroll_items,
    hr_review_cycles,
    hr_reviews,
    hr_checklists,
    hr_checklist_items,
    hr_documents,
```

## 3. `src/db/models.rs`

Append after the last struct pair (`ProjectActivity` / `NewProjectActivity`).
Money uses `diesel::data_types::PgNumeric` (already imported in
`models.rs`; it has no serde implementation, matching the existing
money fields in this file). Leave days are `PgNumeric` too
(NUMERIC(10,2) in SQL). `rating` is `Option<i16>` (SMALLINT).
`goals` is `serde_json::Value` (JSONB array of strings). The `New*`
structs follow the existing convention: `id` plus every non-timestamp
column, no `created_at` / `updated_at`. Update the file header comment
to migrations 000001–000037. `models.rs` currently imports only
`chrono::{DateTime, Utc}` — extend that import first, because the HR
structs use date columns:

```rust
use chrono::{DateTime, NaiveDate, Utc};
```

```rust
// Human Resource Management (2026-11-22-000037_hr). Money fields are
// `PgNumeric` (NUMERIC(19,4) in SQL); leave days are `PgNumeric`
// (NUMERIC(10,2) in SQL). See the note at the top of this file about
// `PgNumeric` having no serde implementation.

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_employees)]
pub struct HrEmployee {
    pub id: Uuid,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<Uuid>,
    pub department_id: Option<String>,
    pub department_verified: bool,
    pub manager_id: Option<Uuid>,
    pub status: String,
    pub hire_date: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_employees)]
pub struct NewHrEmployee {
    pub id: Uuid,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<Uuid>,
    pub department_id: Option<String>,
    pub department_verified: bool,
    pub manager_id: Option<Uuid>,
    pub status: String,
    pub hire_date: NaiveDate,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_positions)]
pub struct HrPosition {
    pub id: Uuid,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<PgNumeric>,
    pub salary_band_max: Option<PgNumeric>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_positions)]
pub struct NewHrPosition {
    pub id: Uuid,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<PgNumeric>,
    pub salary_band_max: Option<PgNumeric>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_contracts)]
pub struct HrContract {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_contracts)]
pub struct NewHrContract {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: PgNumeric,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_leave_types)]
pub struct HrLeaveType {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub default_days_per_year: PgNumeric,
    pub paid: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_leave_types)]
pub struct NewHrLeaveType {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub default_days_per_year: PgNumeric,
    pub paid: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_leave_balances)]
pub struct HrLeaveBalance {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub year: i32,
    pub entitled_days: PgNumeric,
    pub used_days: PgNumeric,
    pub carried_over_days: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_leave_balances)]
pub struct NewHrLeaveBalance {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub year: i32,
    pub entitled_days: PgNumeric,
    pub used_days: PgNumeric,
    pub carried_over_days: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_leave_requests)]
pub struct HrLeaveRequest {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: PgNumeric,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_leave_requests)]
pub struct NewHrLeaveRequest {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: PgNumeric,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_attendance_records)]
pub struct HrAttendanceRecord {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: i32,
    pub source: String,
    pub entry_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_attendance_records)]
pub struct NewHrAttendanceRecord {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: i32,
    pub source: String,
    pub entry_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_payroll_runs)]
pub struct HrPayrollRun {
    pub id: Uuid,
    pub period: String,
    pub currency: String,
    pub status: String,
    pub total_gross: PgNumeric,
    pub total_net: PgNumeric,
    pub total_employees: i32,
    pub finalized_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_payroll_runs)]
pub struct NewHrPayrollRun {
    pub id: Uuid,
    pub period: String,
    pub currency: String,
    pub status: String,
    pub total_gross: PgNumeric,
    pub total_net: PgNumeric,
    pub total_employees: i32,
    pub finalized_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_payroll_items)]
pub struct HrPayrollItem {
    pub id: Uuid,
    pub payroll_run_id: Uuid,
    pub employee_id: Uuid,
    pub gross: PgNumeric,
    pub adjustments: PgNumeric,
    pub deductions: PgNumeric,
    pub net: PgNumeric,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_payroll_items)]
pub struct NewHrPayrollItem {
    pub id: Uuid,
    pub payroll_run_id: Uuid,
    pub employee_id: Uuid,
    pub gross: PgNumeric,
    pub adjustments: PgNumeric,
    pub deductions: PgNumeric,
    pub net: PgNumeric,
    pub notes: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_review_cycles)]
pub struct HrReviewCycle {
    pub id: Uuid,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_review_cycles)]
pub struct NewHrReviewCycle {
    pub id: Uuid,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_reviews)]
pub struct HrReview {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub employee_id: Uuid,
    pub reviewer_id: String,
    pub rating: Option<i16>,
    pub goals: Value,
    pub reviewer_notes: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_reviews)]
pub struct NewHrReview {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub employee_id: Uuid,
    pub reviewer_id: String,
    pub rating: Option<i16>,
    pub goals: Value,
    pub reviewer_notes: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_checklists)]
pub struct HrChecklist {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub kind: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_checklists)]
pub struct NewHrChecklist {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub kind: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_checklist_items)]
pub struct HrChecklistItem {
    pub id: Uuid,
    pub checklist_id: Uuid,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_checklist_items)]
pub struct NewHrChecklistItem {
    pub id: Uuid,
    pub checklist_id: Uuid,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_documents)]
pub struct HrDocument {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub title: String,
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_documents)]
pub struct NewHrDocument {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub title: String,
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
}
```

## 4. `README.md` lines

The HR delta is +1 module, +1 migration, +14 tables. Recount all three
totals after every parallel batch-3 module lands; the pre-HR baseline
in the tree is 72 modules / 31 migrations / 179 tables.

New list entry, appended after "71. Project Management" (renumber if
other batch-3 entries land first — keep the list sequential):

```markdown
72. **Human Resource Management** — employees (user ids only), positions
    with salary bands, contracts, leave types / balances / requests with
    approval workflow, attendance fed by TimeTracker events, payroll runs
    (computed from contracts, finalized only — never pays out), performance
    reviews, onboarding and offboarding checklists, employee document
    metadata (files live in Storage), org-chart data, and HR reports.
    Role-based access: an employee sees only their own records; salary
    amounts are visible only to HR-capable callers and the employee
    themself.
```

Header line (line ~10, adjust the numbers to the final recount):

```markdown
> bus (see the list below), 32 Diesel migrations covering 193 tables,
```

Module total line (after the list, adjust to the final recount):

```markdown
Plus **Theme Management** (described below) — 73 registered modules in
total, counted from `src/modules/mod.rs` (`pub mod` == register calls ==
module files).
```

ERP interconnection section — append to the flow list:

```markdown
- **TimeTracker → HR attendance:** a logged TimeTracker entry records one
  attendance row for the employee (`timetracker.time.logged` /
  `timetracker.entry_logged`, see the convergence note below).
- **HR leave → Notifications:** approving or rejecting a leave request
  notifies the employee through `notification.requested`.
- **HR payroll → Billing:** finalizing a payroll run emits
  `hr.payroll_run_completed`; the billing module owns any payout.
```

## 5. `TODO.md` lines

Append a new section at the end of the file (append-only; do not edit
anything above):

```markdown
## Human Resource Management module (2026-11-22) — batch 3, builder B2

### Files added

- Added `src/modules/hr.rs` (62 routes under `/api/v1/hr`, module id
  `hr`, requires users / departments / notifications / audit, not
  available as a product).
- Added `migrations/2026-11-22-000037_hr/up.sql` and `down.sql`
  (14 tables, PostgreSQL only, money NUMERIC(19,4)).

### Features

- Employees (user id only), positions with salary bands, contracts
  (one active contract at a time; a new contract expires the previous
  one), leave types / balances / requests with approve / reject /
  cancel, overlap rejection, and balance deduction on approval.
- Attendance from TimeTracker events (both event names accepted, see
  convergence note) plus manual HR entry with entry-id de-duplication.
- Payroll runs: create / compute from active contracts / finalize;
  finalizing emits `hr.payroll_run_completed` and never pays out.
- Performance-review cycles and reviews (rating 1-5, required before
  completion), onboarding/offboarding checklists, employee document
  metadata (Storage holds the file), org-chart data (manager links,
  cycles rejected), headcount / leave-usage / payroll-summary reports.
- Role-based access on every handler: `hr:read`, `hr:read_all`,
  `hr:manage`, `hr:payroll` via the `role.check` hook, fail closed.
  Salary amounts are stripped from responses for callers without HR
  capability and for other employees.

### Integrator work still open (see section 6)

- `src/modules/mod.rs`, `src/db/schema.rs`, `src/db/models.rs`,
  `README.md` (snippets in INTEGRATION_NOTES_BATCH3_B2.md).
- `src/modules/audit.rs`: add the 7 `hr.*` events to `AUDITED_EVENTS`.
- Grant the `hr:*` capabilities to the HR roles (no default role holds
  them); the admin role (`"*": ["*"]`) already passes every check.
```

## 6. Convergence notes (read before integrating)

### 6a. TimeTracker event name — CONVERGENCE, must be reconciled

The brief documents `timetracker.entry_logged` with payload
`{user_id, date, minutes, source}`. The TimeTracker module already in
this tree (`src/modules/timetracker.rs`) emits **`timetracker.time.logged`**
with a DIFFERENT payload:

```json
{
  "entry_id": "<uuid>",
  "user_id": "<user id>",
  "started_at": "<RFC 3339>",
  "ended_at": "<RFC 3339>",
  "duration_secs": 5400,
  "hours": 1.5,
  "billable": true,
  "source": "timer"
}
```

`src/modules/hr.rs` subscribes to BOTH event names and accepts BOTH
payload shapes (minutes, else `duration_secs` / 60, else `hours` * 60;
date, else the date part of `started_at` / `ended_at`), de-duplicates
on `entry_id`, and ignores unknown users and unparsable payloads. It
works with either TimeTracker today. The integrator should pick ONE
canonical name and payload and remove the other subscription; until
then, keep both.

### 6b. `department.membership_check` is agent membership, not employment

The Departments module answers `department.membership_check` with the
departments a user *agents for*. That is not the department an employee
belongs to, so HR cannot reject a department id just because the answer
omits it. `hr.rs` therefore records `department_verified: true` only
when the hook answer lists the supplied department, and stores the id
either way. If a `department.exists` event is ever added, HR should use
it for strict validation instead.

### 6c. `storage.file_requested` has no handler yet

No module in this tree answers `storage.file_requested`. Document
creation in `hr.rs` emits it defensively: a handler answering
`{"exists": false}` makes creation fail with 404; no answer means the
metadata is stored as supplied. The Storage Management builder should
confirm the event name and answer shape (`{"file_id", "exists"}`).

### 6d. Audit events

`hr.rs` emits `hr.employee_created`, `hr.leave_requested`,
`hr.leave_approved`, `hr.leave_rejected`, `hr.payroll_run_completed`,
`hr.review_completed`, `hr.onboarding_completed`, plus
`notification.requested` for leave decisions and completed reviews.
The audit module only records events listed in its `AUDITED_EVENTS`;
add the seven `hr.*` names there or HR actions will not appear in the
audit trail.

### 6e. Capabilities are not in the default roles

`hr:read`, `hr:read_all`, `hr:manage`, and `hr:payroll` exist only as
strings checked through the `role.check` hook. No default role in
`src/modules/roles.rs` grants any of them, so until an administrator
grants them to an HR role, only the admin role can use the HR write
and payroll endpoints. Employees can still file and view their own
leave only if their role holds `hr:read` — grant `hr:read` to the
member role (or the role employees use) if self-service leave is
wanted. `hr:manage` also grants payroll access.

### 6f. Money and currency

Payroll is computed in the run's currency only. A contract in another
currency is skipped during compute (never converted at an invented
rate). The integrator / billing owner should decide whether
multi-currency runs need conversion or per-currency runs.
