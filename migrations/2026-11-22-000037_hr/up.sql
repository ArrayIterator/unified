-- HR module tables for the Unified Pentagonal Project
-- (`src/modules/hr.rs`): employees (user ids only), positions,
-- contracts, leave types / balances / requests, attendance fed by
-- TimeTracker events, payroll runs and items, performance-review
-- cycles and reviews, onboarding/offboarding checklists and items,
-- and employee document metadata (files live in Storage).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the project task list):
--   - `hr_employees` stores the linked user id ONLY. Names, emails,
--     and all other profile data live in the Users module; the
--     `user_id` column is a plain VARCHAR reference with NO foreign
--     key on purpose, matching how other modules reference users
--     (for example `ai_usage_records.actor`).
--   - `hr_employees.department_id` and `hr_positions.department_id`
--     are plain VARCHAR references with NO foreign key: department
--     records are owned by the Departments module and are validated
--     through the `department.membership_check` hook at the API
--     layer, not by the database.
--   - Money is NUMERIC(19,4) everywhere it appears:
--     `hr_positions.salary_band_min/max`,
--     `hr_contracts.salary_amount`, `hr_payroll_runs.total_gross/
--     total_net`, `hr_payroll_items.gross/adjustments/deductions/
--     net`. The Rust structs carry these as `f64` rounded to four
--     decimals in memory; the Diesel structs in src/db/models.rs use
--     `diesel::data_types::PgNumeric`.
--   - Leave days are NUMERIC(10,2): a day is the unit of leave, and
--     half days must be representable. `hr_leave_requests.days` is
--     the inclusive calendar-day count between start and end.
--   - `hr_leave_balances` is unique on (employee_id, leave_type_id,
--     year): one balance row per employee, leave type, and year.
--   - `hr_attendance_records.entry_id` is the TimeTracker entry id
--     and is unique when present, so a repeated
--     `timetracker.time.logged` / `timetracker.entry_logged` event
--     cannot create a duplicate row. PostgreSQL treats NULL as
--     distinct, so manual rows without an entry id are unaffected.
--   - `hr_payroll_runs` is unique on `period`: one run per month.
--     `hr_payroll_items` is unique on (payroll_run_id, employee_id):
--     one line per employee per run. Payroll never pays out: a
--     finalized run only emits `hr.payroll_run_completed` for the
--     billing module.
--   - `hr_reviews` is unique on (cycle_id, employee_id): one review
--     per employee per cycle. `rating` is 1 through 5 and is NULL
--     until the review is completed.
--   - `hr_checklists` is unique on (employee_id, kind) only for open
--     checklists; that rule is enforced by the module (a completed
--     checklist does not block a later one), so there is no partial
--     unique index here.
--   - `hr_documents.storage_file_id` is a plain VARCHAR reference
--     with NO foreign key: the file itself lives in Storage.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One employee. Only the linked user id is stored here.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_employees (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL,
    employee_code VARCHAR NOT NULL,
    position_id UUID,
    department_id VARCHAR,
    department_verified BOOLEAN NOT NULL DEFAULT FALSE,
    manager_id UUID,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'on_leave', 'terminated')),
    hire_date DATE NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_employees_user_id
    ON hr_employees (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_employees_employee_code
    ON hr_employees (employee_code);
CREATE INDEX IF NOT EXISTS idx_hr_employees_position_id
    ON hr_employees (position_id);
CREATE INDEX IF NOT EXISTS idx_hr_employees_department_id
    ON hr_employees (department_id);
CREATE INDEX IF NOT EXISTS idx_hr_employees_manager_id
    ON hr_employees (manager_id);
CREATE INDEX IF NOT EXISTS idx_hr_employees_status
    ON hr_employees (status);

-- ---------------------------------------------------------------------------
-- One position (job title) with its salary band.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_positions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    department_id VARCHAR,
    level VARCHAR,
    salary_band_min NUMERIC(19,4) CHECK (salary_band_min IS NULL OR salary_band_min >= 0),
    salary_band_max NUMERIC(19,4) CHECK (salary_band_max IS NULL OR salary_band_max >= 0),
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'inactive')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (salary_band_min IS NULL OR salary_band_max IS NULL OR salary_band_min <= salary_band_max)
);

CREATE INDEX IF NOT EXISTS idx_hr_positions_department_id
    ON hr_positions (department_id);
CREATE INDEX IF NOT EXISTS idx_hr_positions_status
    ON hr_positions (status);

-- ---------------------------------------------------------------------------
-- One employment contract. Salary is monthly pay in `currency`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_contracts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    contract_type VARCHAR NOT NULL
        CHECK (contract_type IN ('permanent', 'fixed_term', 'intern', 'part_time', 'contractor')),
    start_date DATE NOT NULL,
    end_date DATE,
    salary_amount NUMERIC(19,4) NOT NULL CHECK (salary_amount >= 0),
    currency VARCHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'expired', 'terminated')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR end_date >= start_date)
);

CREATE INDEX IF NOT EXISTS idx_hr_contracts_employee_id
    ON hr_contracts (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_contracts_status
    ON hr_contracts (status);

-- ---------------------------------------------------------------------------
-- One leave type (annual leave, sick leave, and so on).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_leave_types (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    code VARCHAR NOT NULL,
    default_days_per_year NUMERIC(10,2) NOT NULL DEFAULT 0
        CHECK (default_days_per_year >= 0 AND default_days_per_year <= 366),
    paid BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_leave_types_code
    ON hr_leave_types (code);

-- ---------------------------------------------------------------------------
-- One employee's balance for one leave type in one calendar year.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_leave_balances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    leave_type_id UUID NOT NULL REFERENCES hr_leave_types (id)
        ON DELETE RESTRICT ON UPDATE NO ACTION,
    year INTEGER NOT NULL CHECK (year >= 2000 AND year <= 2100),
    entitled_days NUMERIC(10,2) NOT NULL DEFAULT 0 CHECK (entitled_days >= 0),
    used_days NUMERIC(10,2) NOT NULL DEFAULT 0 CHECK (used_days >= 0),
    carried_over_days NUMERIC(10,2) NOT NULL DEFAULT 0 CHECK (carried_over_days >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_leave_balances_employee_type_year
    ON hr_leave_balances (employee_id, leave_type_id, year);
CREATE INDEX IF NOT EXISTS idx_hr_leave_balances_employee_id
    ON hr_leave_balances (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_leave_balances_year
    ON hr_leave_balances (year);

-- ---------------------------------------------------------------------------
-- One leave/vacation request with its approval workflow.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_leave_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    leave_type_id UUID NOT NULL REFERENCES hr_leave_types (id)
        ON DELETE RESTRICT ON UPDATE NO ACTION,
    start_date DATE NOT NULL,
    end_date DATE NOT NULL,
    days NUMERIC(10,2) NOT NULL CHECK (days > 0),
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'cancelled')),
    reason TEXT NOT NULL DEFAULT '',
    reviewed_by VARCHAR,
    review_note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (end_date >= start_date)
);

CREATE INDEX IF NOT EXISTS idx_hr_leave_requests_employee_id
    ON hr_leave_requests (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_leave_requests_leave_type_id
    ON hr_leave_requests (leave_type_id);
CREATE INDEX IF NOT EXISTS idx_hr_leave_requests_status
    ON hr_leave_requests (status);
CREATE INDEX IF NOT EXISTS idx_hr_leave_requests_start_date
    ON hr_leave_requests (start_date);

-- ---------------------------------------------------------------------------
-- One day of attendance, fed by TimeTracker events or entered manually.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_attendance_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    work_date DATE NOT NULL,
    minutes_worked INTEGER NOT NULL CHECK (minutes_worked >= 1 AND minutes_worked <= 1440),
    source VARCHAR NOT NULL DEFAULT 'timetracker',
    entry_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_attendance_records_entry_id
    ON hr_attendance_records (entry_id);
CREATE INDEX IF NOT EXISTS idx_hr_attendance_records_employee_id
    ON hr_attendance_records (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_attendance_records_work_date
    ON hr_attendance_records (work_date);

-- ---------------------------------------------------------------------------
-- One payroll run for one period (YYYY-MM). Finalizing only emits
-- `hr.payroll_run_completed`; nothing here pays anyone.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_payroll_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period VARCHAR NOT NULL,
    currency VARCHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'computed', 'finalized', 'cancelled')),
    total_gross NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (total_gross >= 0),
    total_net NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (total_net >= 0),
    total_employees INTEGER NOT NULL DEFAULT 0 CHECK (total_employees >= 0),
    finalized_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_payroll_runs_period
    ON hr_payroll_runs (period);
CREATE INDEX IF NOT EXISTS idx_hr_payroll_runs_status
    ON hr_payroll_runs (status);

-- ---------------------------------------------------------------------------
-- One employee's line inside a payroll run.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_payroll_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payroll_run_id UUID NOT NULL REFERENCES hr_payroll_runs (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    gross NUMERIC(19,4) NOT NULL CHECK (gross >= 0),
    adjustments NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (adjustments >= 0),
    deductions NUMERIC(19,4) NOT NULL DEFAULT 0 CHECK (deductions >= 0),
    net NUMERIC(19,4) NOT NULL CHECK (net >= 0),
    notes TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_payroll_items_run_employee
    ON hr_payroll_items (payroll_run_id, employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_payroll_items_payroll_run_id
    ON hr_payroll_items (payroll_run_id);
CREATE INDEX IF NOT EXISTS idx_hr_payroll_items_employee_id
    ON hr_payroll_items (employee_id);

-- ---------------------------------------------------------------------------
-- One performance-review cycle (for example "2026 H2").
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_review_cycles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    period_start DATE NOT NULL,
    period_end DATE NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('draft', 'active', 'closed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start)
);

CREATE INDEX IF NOT EXISTS idx_hr_review_cycles_status
    ON hr_review_cycles (status);

-- ---------------------------------------------------------------------------
-- One performance review: rating, goals, and reviewer notes.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cycle_id UUID NOT NULL REFERENCES hr_review_cycles (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    reviewer_id VARCHAR NOT NULL,
    rating SMALLINT CHECK (rating IS NULL OR (rating >= 1 AND rating <= 5)),
    goals JSONB NOT NULL DEFAULT '[]'::jsonb,
    reviewer_notes TEXT NOT NULL DEFAULT '',
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'submitted', 'completed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_hr_reviews_cycle_employee
    ON hr_reviews (cycle_id, employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_reviews_employee_id
    ON hr_reviews (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_reviews_reviewer_id
    ON hr_reviews (reviewer_id);
CREATE INDEX IF NOT EXISTS idx_hr_reviews_status
    ON hr_reviews (status);

-- ---------------------------------------------------------------------------
-- One onboarding or offboarding checklist for one employee.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_checklists (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('onboarding', 'offboarding')),
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'in_progress', 'completed')),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_hr_checklists_employee_id
    ON hr_checklists (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_checklists_kind
    ON hr_checklists (kind);
CREATE INDEX IF NOT EXISTS idx_hr_checklists_status
    ON hr_checklists (status);

-- ---------------------------------------------------------------------------
-- One task inside a checklist.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_checklist_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    checklist_id UUID NOT NULL REFERENCES hr_checklists (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    completed_by VARCHAR,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_hr_checklist_items_checklist_id
    ON hr_checklist_items (checklist_id);

-- ---------------------------------------------------------------------------
-- One employee document. Metadata only — the file itself lives in
-- Storage under `storage_file_id`; no file bytes are stored here.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS hr_documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id UUID NOT NULL REFERENCES hr_employees (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    document_type VARCHAR NOT NULL DEFAULT 'other'
        CHECK (document_type IN ('contract', 'id', 'passport', 'certificate', 'other')),
    storage_file_id VARCHAR NOT NULL,
    mime_type VARCHAR,
    size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    uploaded_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_hr_documents_employee_id
    ON hr_documents (employee_id);
CREATE INDEX IF NOT EXISTS idx_hr_documents_storage_file_id
    ON hr_documents (storage_file_id);
CREATE INDEX IF NOT EXISTS idx_hr_documents_document_type
    ON hr_documents (document_type);
