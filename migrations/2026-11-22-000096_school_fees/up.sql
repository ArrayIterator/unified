-- School fee tables for the Unified Pentagonal Project
-- (`src/modules/billing.rs`, school-fees section): admin-editable
-- fee type definitions, per-student fee schedules, generated fee
-- charges linked to billing invoices, per-student discounts and
-- scholarships, issued receipts, guardian links, and sent
-- reminders. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the e-learning precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All money amounts are NUMERIC(19,4). In memory the module
--     keeps integer minor units; the NUMERIC columns are the
--     durable form of those amounts.
--   - `student_user_id` / `guardian_user_id` / `program_code` /
--     `level_code` / `academic_period_id` are plain TEXT with NO
--     foreign key on purpose: students, programs and periods live
--     in the academic module and this module must not assume its
--     table or migration order. `invoice_id` is plain TEXT for the
--     same reason: invoices live in the billing tables.
--   - `fee_charges.discount_ids` is JSONB: an array of discount
--     ids applied to the charge at generation time.
--   - `school_fee_reminders` is keyed by
--     (charge_id, offset_days, recipient_user_id) so the reminder
--     scan never sends the same reminder twice to the same person.
--   - No secret is stored in any table here.

CREATE TABLE school_fee_types (
    id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    category TEXT NOT NULL,
    billing_period TEXT NOT NULL,
    default_amount_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    auto_assign_on_enrolment BOOLEAN NOT NULL DEFAULT FALSE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE school_fee_schedules (
    id TEXT PRIMARY KEY,
    student_user_id TEXT NOT NULL,
    program_code TEXT NOT NULL,
    level_code TEXT NOT NULL,
    academic_period_id TEXT NOT NULL,
    fee_type_id TEXT NOT NULL REFERENCES school_fee_types (id),
    amount_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    due_date TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX school_fee_schedules_student_idx ON school_fee_schedules (student_user_id);
CREATE INDEX school_fee_schedules_fee_type_idx ON school_fee_schedules (fee_type_id);
CREATE INDEX school_fee_schedules_period_idx ON school_fee_schedules (academic_period_id);

CREATE TABLE school_fee_charges (
    id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL REFERENCES school_fee_schedules (id),
    student_user_id TEXT NOT NULL,
    fee_type_id TEXT NOT NULL REFERENCES school_fee_types (id),
    academic_period_id TEXT NOT NULL,
    period_label TEXT NOT NULL,
    amount_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    discount_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    total_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    amount_paid_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    invoice_id TEXT,
    discount_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    due_date TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    paid_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX school_fee_charges_schedule_period_uidx
    ON school_fee_charges (schedule_id, period_label) WHERE status <> 'void';
CREATE INDEX school_fee_charges_student_idx ON school_fee_charges (student_user_id);
CREATE INDEX school_fee_charges_invoice_idx ON school_fee_charges (invoice_id);
CREATE INDEX school_fee_charges_due_idx ON school_fee_charges (due_date);

CREATE TABLE school_fee_discounts (
    id TEXT PRIMARY KEY,
    student_user_id TEXT NOT NULL,
    fee_type_id TEXT REFERENCES school_fee_types (id),
    kind TEXT NOT NULL,
    percent_basis_points BIGINT NOT NULL DEFAULT 0,
    amount_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    valid_from TIMESTAMPTZ,
    valid_to TIMESTAMPTZ,
    approved BOOLEAN NOT NULL DEFAULT FALSE,
    approved_by TEXT,
    reason TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX school_fee_discounts_student_idx ON school_fee_discounts (student_user_id);

CREATE TABLE school_fee_receipts (
    id TEXT PRIMARY KEY,
    number TEXT NOT NULL UNIQUE,
    charge_id TEXT NOT NULL REFERENCES school_fee_charges (id),
    student_user_id TEXT NOT NULL,
    amount_minor NUMERIC(19,4) NOT NULL DEFAULT 0,
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    method TEXT,
    reference TEXT,
    issued_by TEXT NOT NULL,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX school_fee_receipts_charge_idx ON school_fee_receipts (charge_id);

CREATE TABLE school_fee_guardians (
    id TEXT PRIMARY KEY,
    guardian_user_id TEXT NOT NULL,
    student_user_id TEXT NOT NULL,
    relationship TEXT NOT NULL DEFAULT 'guardian',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX school_fee_guardians_link_uidx
    ON school_fee_guardians (guardian_user_id, student_user_id) WHERE active;
CREATE INDEX school_fee_guardians_student_idx ON school_fee_guardians (student_user_id);

CREATE TABLE school_fee_reminders (
    charge_id TEXT NOT NULL REFERENCES school_fee_charges (id),
    offset_days INTEGER NOT NULL,
    recipient_user_id TEXT NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (charge_id, offset_days, recipient_user_id)
);
