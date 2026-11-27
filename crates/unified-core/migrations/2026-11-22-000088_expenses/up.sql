-- Expenses module tables for the Unified Pentagonal Project
-- (`src/modules/expenses.rs`): categories, policies, reports,
-- expense lines, receipt references, reimbursements, adjustments
-- and refunds, and stored currency rates. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the gamification precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `employee_id` / `company_id` / `project_id` / `client_id`
--     columns are plain TEXT with NO foreign key on purpose: users,
--     companies, projects, and clients live in other modules and
--     this module must not assume their tables or migration order.
--     `approval_chain_id` is likewise an opaque reference to a
--     chain owned by the approvals flow.
--   - `expense_receipts` stores storage references only
--     (`storage_target` + `storage_key`); no receipt bytes are
--     stored here.
--   - Money is stored in minor units (BIGINT). Mileage and
--     per-diem amounts are computed server-side from the policy
--     rate; the rate actually applied is recorded on the line in
--     `rate_minor_applied` so a later policy change never rewrites
--     history.
--   - `violations` is TEXT[]: policy flags recorded on the line
--     (`over_per_expense_limit`, `over_per_day_limit`,
--     `receipt_missing`) and on the report (`over_report_total`).
--     Flagged lines are never silently auto-approved.
--   - `expense_adjustments` are separate entries that adjust net
--     spend in analytics; they never mutate a report total or an
--     expense line, so submitted history stays intact.
--   - `expense_currency_rates.rate` is quoted as target minor
--     units per one base minor unit; the inverse pair is derived
--     as `1 / rate` by the module when only the reverse direction
--     is stored. Rates are stored data only - never fetched live.
--   - Report status is CHECK-constrained to the state machine
--     vocabulary (`draft`, `submitted`, `approved`, `rejected`,
--     `reimbursed`); reimbursement status to (`pending`, `paid`,
--     `failed`); adjustment kind to (`refund`, `adjustment`).
--   - All ids are Uuid, all timestamps are Timestamptz. No secret
--     is stored in any table here.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Expense categories (admin-editable rules).
CREATE TABLE IF NOT EXISTS expense_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    company_id TEXT,
    receipt_required BOOLEAN NOT NULL DEFAULT FALSE,
    receipt_required_above_minor BIGINT CHECK (receipt_required_above_minor IS NULL OR receipt_required_above_minor >= 0),
    per_expense_limit_minor BIGINT CHECK (per_expense_limit_minor IS NULL OR per_expense_limit_minor >= 0),
    per_day_limit_minor BIGINT CHECK (per_day_limit_minor IS NULL OR per_day_limit_minor >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expense_categories_company ON expense_categories (company_id);
CREATE INDEX IF NOT EXISTS idx_expense_categories_active ON expense_categories (active);

-- Expense policies (per-company rules).
CREATE TABLE IF NOT EXISTS expense_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    company_id TEXT,
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    mileage_rate_minor BIGINT CHECK (mileage_rate_minor IS NULL OR mileage_rate_minor >= 0),
    mileage_unit TEXT NOT NULL DEFAULT 'km' CHECK (char_length(mileage_unit) BETWEEN 1 AND 16),
    per_diem_rate_minor BIGINT CHECK (per_diem_rate_minor IS NULL OR per_diem_rate_minor >= 0),
    max_report_total_minor BIGINT CHECK (max_report_total_minor IS NULL OR max_report_total_minor >= 0),
    approval_chain_id TEXT,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expense_policies_company ON expense_policies (company_id);
CREATE INDEX IF NOT EXISTS idx_expense_policies_active ON expense_policies (active);

-- Expense reports.
CREATE TABLE IF NOT EXISTS expense_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id TEXT NOT NULL CHECK (char_length(employee_id) BETWEEN 1 AND 200),
    company_id TEXT,
    policy_id UUID REFERENCES expense_policies (id) ON DELETE SET NULL,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'approved', 'rejected', 'reimbursed')),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    total_minor BIGINT NOT NULL DEFAULT 0 CHECK (total_minor >= 0),
    violations TEXT[] NOT NULL DEFAULT '{}',
    approval_request_id TEXT,
    submitted_at TIMESTAMPTZ,
    decided_at TIMESTAMPTZ,
    decided_by TEXT,
    decision_reason TEXT,
    reimbursement_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expense_reports_employee ON expense_reports (employee_id);
CREATE INDEX IF NOT EXISTS idx_expense_reports_status ON expense_reports (status);
CREATE INDEX IF NOT EXISTS idx_expense_reports_company ON expense_reports (company_id);
CREATE INDEX IF NOT EXISTS idx_expense_reports_policy ON expense_reports (policy_id);

-- Expense lines inside a report.
CREATE TABLE IF NOT EXISTS expenses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_id UUID NOT NULL REFERENCES expense_reports (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    category_id UUID NOT NULL REFERENCES expense_categories (id) ON DELETE RESTRICT,
    expense_date DATE NOT NULL,
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    description TEXT NOT NULL CHECK (char_length(description) BETWEEN 1 AND 5000),
    project_id TEXT,
    client_id TEXT,
    distance DOUBLE PRECISION CHECK (distance IS NULL OR distance > 0),
    days BIGINT CHECK (days IS NULL OR days BETWEEN 1 AND 365),
    rate_minor_applied BIGINT CHECK (rate_minor_applied IS NULL OR rate_minor_applied >= 0),
    violations TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expenses_report ON expenses (report_id, expense_date);
CREATE INDEX IF NOT EXISTS idx_expenses_category ON expenses (category_id);
CREATE INDEX IF NOT EXISTS idx_expenses_date ON expenses (expense_date);

-- Receipt references (storage references only, no bytes).
CREATE TABLE IF NOT EXISTS expense_receipts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    expense_id UUID NOT NULL REFERENCES expenses (id) ON DELETE CASCADE,
    storage_target TEXT NOT NULL CHECK (char_length(storage_target) BETWEEN 1 AND 200),
    storage_key TEXT NOT NULL CHECK (char_length(storage_key) BETWEEN 1 AND 512),
    file_name TEXT NOT NULL CHECK (char_length(file_name) BETWEEN 1 AND 200),
    content_type TEXT,
    uploaded_by TEXT NOT NULL CHECK (char_length(uploaded_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expense_receipts_expense ON expense_receipts (expense_id);

-- Reimbursements (pay-out tracking for approved reports).
CREATE TABLE IF NOT EXISTS expense_reimbursements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_id UUID NOT NULL REFERENCES expense_reports (id) ON DELETE CASCADE,
    employee_id TEXT NOT NULL CHECK (char_length(employee_id) BETWEEN 1 AND 200),
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'paid', 'failed')),
    payable_reference TEXT NOT NULL CHECK (char_length(payable_reference) BETWEEN 1 AND 200),
    failure_reason TEXT,
    paid_at TIMESTAMPTZ,
    attempts BIGINT NOT NULL DEFAULT 1 CHECK (attempts >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expense_reimbursements_report ON expense_reimbursements (report_id);
CREATE INDEX IF NOT EXISTS idx_expense_reimbursements_employee ON expense_reimbursements (employee_id);
CREATE INDEX IF NOT EXISTS idx_expense_reimbursements_status ON expense_reimbursements (status);

-- Refunds and adjustments (separate entries, never history edits).
CREATE TABLE IF NOT EXISTS expense_adjustments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_id UUID NOT NULL REFERENCES expense_reports (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('refund', 'adjustment')),
    amount_minor BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    reason TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 1000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_expense_adjustments_report ON expense_adjustments (report_id);

-- Stored currency rates (stored data only, never fetched live).
CREATE TABLE IF NOT EXISTS expense_currency_rates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    base_currency TEXT NOT NULL CHECK (char_length(base_currency) = 3),
    target_currency TEXT NOT NULL CHECK (char_length(target_currency) = 3),
    rate DOUBLE PRECISION NOT NULL CHECK (rate > 0),
    source TEXT NOT NULL DEFAULT 'manual' CHECK (char_length(source) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT expense_currency_rates_pair_differ CHECK (base_currency <> target_currency)
);
CREATE INDEX IF NOT EXISTS idx_expense_currency_rates_pair ON expense_currency_rates (base_currency, target_currency);
