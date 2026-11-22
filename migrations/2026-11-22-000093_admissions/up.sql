-- Admissions tables for the Unified Pentagonal Project
-- (src/modules/admissions.rs, batch 13 builder B3).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `admission_periods` holds one admission period per academic
--     year and term, with open/close dates, an optional registration
--     fee, and an optional link to the forms-module form used for
--     online registration (`registration_form_id`).
--   - `admission_period_quotas` holds per-program, per-level seat
--     quotas. Quotas are data: one row per (`program_id`, `level`)
--     pair that accepts students in a period.
--   - `admission_stage_definitions` holds admin-defined selection
--     stages per period (key, order, weight in the total, minimum
--     passing score, required flag).
--   - `admission_required_documents` holds the documents a period
--     requires from every applicant (`document_key` is the stable
--     key uploads are matched against).
--   - `admission_applications` holds one application per applicant
--     per period. Only the applicant user id is stored; profile data
--     lives in the Users module. `program_id` and `level` are
--     references owned by the academic system. `student_id` is set
--     only when the academic system answers
--     `academic.student.created`; this module never creates student
--     records. `fee_invoice_status` is `none`, `requested`, `paid`,
--     or `waived`. Money is NUMERIC(19,4); scores are NUMERIC(10,4).
--   - `admission_application_scores` holds one score per
--     (application, stage) pair. The weighted total is derived, not
--     stored in a separate table.
--   - `admission_checklist_items` holds one item per (application,
--     required document) pair, snapshotted when the application is
--     created. `document_id` is a documents-module document id;
--     no file bytes are stored here.
--   - `admission_waitlist_entries` holds the ordered waitlist
--     (`position` orders promotion within a period).
--   - `admission_results` holds one announced result per
--     application. `published` controls whether the applicant may
--     see it.
--   - `admission_settings` holds the admin-editable vocabularies
--     and defaults (`status.period`, `status.application`,
--     `status.checklist`, `status.waitlist`, `decision.result`,
--     `defaults.fee_currency`, `defaults.score_scale_max`,
--     `defaults.waitlist_order`,
--     `defaults.result_visible_to_applicant`). The seed rows below
--     are the configurable defaults; an administrator may change
--     them through the settings endpoint.
--   - Secrets never appear in these tables.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One admission period (academic year + term).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_periods (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    academic_year VARCHAR NOT NULL,
    term VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    open_date DATE NOT NULL,
    close_date DATE NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'draft',
    registration_fee_amount NUMERIC(19,4),
    fee_currency VARCHAR NOT NULL DEFAULT 'USD',
    registration_form_id VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_periods_close_after_open CHECK (close_date >= open_date),
    CONSTRAINT admission_periods_year_term_unique UNIQUE (academic_year, term)
);

CREATE INDEX IF NOT EXISTS idx_admission_periods_status
    ON admission_periods (status);
CREATE INDEX IF NOT EXISTS idx_admission_periods_open_date
    ON admission_periods (open_date);

-- ---------------------------------------------------------------------------
-- One per-program, per-level seat quota inside a period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_period_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    program_id VARCHAR NOT NULL,
    level VARCHAR NOT NULL,
    seats INTEGER NOT NULL CHECK (seats >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_period_quotas_pair_unique UNIQUE (period_id, program_id, level)
);

CREATE INDEX IF NOT EXISTS idx_admission_period_quotas_period_id
    ON admission_period_quotas (period_id);

-- ---------------------------------------------------------------------------
-- One selection stage definition of a period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_stage_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    stage_key VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    position INTEGER NOT NULL DEFAULT 1,
    weight NUMERIC(10,4) NOT NULL DEFAULT 1 CHECK (weight >= 0),
    pass_score NUMERIC(10,4),
    required BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_stage_definitions_key_unique UNIQUE (period_id, stage_key)
);

CREATE INDEX IF NOT EXISTS idx_admission_stage_definitions_period_id
    ON admission_stage_definitions (period_id);

-- ---------------------------------------------------------------------------
-- One document a period requires from every applicant.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_required_documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    document_key VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    description VARCHAR,
    required BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_required_documents_key_unique UNIQUE (period_id, document_key)
);

CREATE INDEX IF NOT EXISTS idx_admission_required_documents_period_id
    ON admission_required_documents (period_id);

-- ---------------------------------------------------------------------------
-- One application for one period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_applications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    applicant_user_id VARCHAR NOT NULL,
    full_name VARCHAR NOT NULL,
    email VARCHAR NOT NULL,
    phone VARCHAR,
    program_id VARCHAR NOT NULL,
    level VARCHAR NOT NULL,
    form_submission_id VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'draft',
    total_score NUMERIC(10,4),
    fee_amount NUMERIC(19,4),
    fee_currency VARCHAR NOT NULL DEFAULT 'USD',
    fee_invoice_status VARCHAR NOT NULL DEFAULT 'none',
    fee_invoice_id VARCHAR,
    student_id VARCHAR,
    source VARCHAR NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_applications_submission_unique UNIQUE (form_submission_id)
);

CREATE INDEX IF NOT EXISTS idx_admission_applications_period_id
    ON admission_applications (period_id);
CREATE INDEX IF NOT EXISTS idx_admission_applications_applicant
    ON admission_applications (applicant_user_id);
CREATE INDEX IF NOT EXISTS idx_admission_applications_status
    ON admission_applications (status);
CREATE INDEX IF NOT EXISTS idx_admission_applications_program
    ON admission_applications (period_id, program_id, level);

-- ---------------------------------------------------------------------------
-- One recorded score for one application in one stage.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_application_scores (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    application_id UUID NOT NULL REFERENCES admission_applications (id) ON DELETE CASCADE,
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    stage_id UUID NOT NULL REFERENCES admission_stage_definitions (id) ON DELETE CASCADE,
    stage_key VARCHAR NOT NULL,
    score NUMERIC(10,4) NOT NULL CHECK (score >= 0),
    scored_by VARCHAR NOT NULL,
    note VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_application_scores_pair_unique UNIQUE (application_id, stage_id)
);

CREATE INDEX IF NOT EXISTS idx_admission_application_scores_application_id
    ON admission_application_scores (application_id);
CREATE INDEX IF NOT EXISTS idx_admission_application_scores_stage_id
    ON admission_application_scores (stage_id);

-- ---------------------------------------------------------------------------
-- One checklist item: one required document for one application.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_checklist_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    application_id UUID NOT NULL REFERENCES admission_applications (id) ON DELETE CASCADE,
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    required_document_id UUID NOT NULL REFERENCES admission_required_documents (id) ON DELETE CASCADE,
    document_key VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    required BOOLEAN NOT NULL DEFAULT TRUE,
    status VARCHAR NOT NULL DEFAULT 'pending',
    document_id VARCHAR,
    verified_by VARCHAR,
    verified_at TIMESTAMPTZ,
    rejection_reason VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_checklist_items_pair_unique UNIQUE (application_id, required_document_id)
);

CREATE INDEX IF NOT EXISTS idx_admission_checklist_items_application_id
    ON admission_checklist_items (application_id);
CREATE INDEX IF NOT EXISTS idx_admission_checklist_items_status
    ON admission_checklist_items (status);

-- ---------------------------------------------------------------------------
-- One waitlist entry.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_waitlist_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    application_id UUID NOT NULL REFERENCES admission_applications (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 1),
    status VARCHAR NOT NULL DEFAULT 'waiting',
    promoted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_admission_waitlist_entries_period_id
    ON admission_waitlist_entries (period_id);
CREATE INDEX IF NOT EXISTS idx_admission_waitlist_entries_application_id
    ON admission_waitlist_entries (application_id);
CREATE INDEX IF NOT EXISTS idx_admission_waitlist_entries_status
    ON admission_waitlist_entries (period_id, status, position);

-- ---------------------------------------------------------------------------
-- One announced result for one application.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id UUID NOT NULL REFERENCES admission_periods (id) ON DELETE CASCADE,
    application_id UUID NOT NULL REFERENCES admission_applications (id) ON DELETE CASCADE,
    decision VARCHAR NOT NULL,
    published BOOLEAN NOT NULL DEFAULT FALSE,
    published_at TIMESTAMPTZ,
    decided_by VARCHAR NOT NULL,
    note VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admission_results_application_unique UNIQUE (application_id)
);

CREATE INDEX IF NOT EXISTS idx_admission_results_period_id
    ON admission_results (period_id);
CREATE INDEX IF NOT EXISTS idx_admission_results_published
    ON admission_results (period_id, published);

-- ---------------------------------------------------------------------------
-- One admin-editable setting (vocabulary or default).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS admission_settings (
    key VARCHAR PRIMARY KEY,
    value VARCHAR NOT NULL,
    description VARCHAR NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO admission_settings (key, value, description) VALUES
    ('status.period', 'draft,open,closed,archived', 'Allowed admission-period statuses, comma-separated.'),
    ('status.application', 'draft,submitted,under_review,accepted,rejected,waitlisted,enrolled,withdrawn', 'Allowed application statuses, comma-separated.'),
    ('status.checklist', 'pending,submitted,verified,rejected', 'Allowed document-checklist item statuses, comma-separated.'),
    ('status.waitlist', 'waiting,promoted,declined,expired', 'Allowed waitlist entry statuses, comma-separated.'),
    ('decision.result', 'accepted,rejected,waitlisted', 'Allowed result decisions, comma-separated.'),
    ('defaults.fee_currency', 'USD', 'Default 3-letter currency code for registration fees.'),
    ('defaults.score_scale_max', '100', 'Maximum score on the scoring scale (scores run 0..=max).'),
    ('defaults.waitlist_order', 'score_desc_submitted_asc', 'Waitlist ordering policy: score_desc_submitted_asc or submitted_asc.'),
    ('defaults.result_visible_to_applicant', 'true', 'Whether applicants may see their own result once published (true/false).')
ON CONFLICT (key) DO NOTHING;
