-- Project Supervision module tables for the Unified Pentagonal Project
-- (`src/modules/supervision.rs`): supervision assignments,
-- inspection templates, conducted inspections with per-item results,
-- progress-report snapshots, defects, approval rows, daily logs, and
-- photo evidence references. Matches src/db/schema.rs conventions.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `project_id` columns are TEXT with NO foreign key on purpose:
--     project ids are opaque strings owned by the projects module
--     (migration 000031 creates the `projects` table with a UUID
--     primary key, but the projects module's in-memory ids are
--     strings and this module must never assume the UUID form).
--     The projects module stays the source of truth for project
--     data; deleting a project does not delete supervision rows —
--     the supervision history is kept for audit. `milestone_id`
--     and `task_id` are the same kind of opaque TEXT reference
--     to the projects module, also with no foreign key.
--   - `user_id` columns are TEXT with NO foreign key on purpose:
--     actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE. Supervision rows
--     are company-scoped where a company is known; rows created
--     without a company keep `company_id` NULL.
--   - Percents are NUMERIC(5,2) (0.00-100.00). Money never appears
--     in this module.
--   - `supervision_inspection_items` copies the template item
--     title at inspection creation time: the template may change
--     later, but a conducted inspection must keep the wording it
--     was conducted against. `template_item_id` keeps the link
--     back to the template item.
--   - `supervision_progress_reports` stores a snapshot, not a
--     live value: `reported_percent_complete` is the supervisor's
--     number, `project_percent_complete` is the percent the
--     projects module returned through the hook query at snapshot
--     time (NULL when the source is `unverified`), and
--     `planned_percent_complete` is the plan target. The variance
--     (reported minus planned) is derived on read, never stored.
--   - `supervision_approvals` rows are append-only audit trail:
--     no handler in the module ever updates or deletes one.
--   - `supervision_photos` stores metadata ONLY: `storage_target`
--     and `storage_key` identify the object in the storage module
--     and `content_sha256` is the SHA-256 hex of the uploaded
--     bytes. No photo bytes live in this schema.
--   - `supervision_daily_logs` has UNIQUE (project_id, log_date):
--     one site log per project per day.
--   - Signatures (`signed_by` / `signed_at` on inspections and
--     daily logs) are recorded attestations by a user id, not
--     cryptographic signatures: no key material is involved.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One supervision assignment: a user assigned to a project site with
-- a role (supervisor / inspector / approver) for a period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_assignments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL
        CHECK (role IN ('supervisor', 'inspector', 'approver')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'suspended', 'ended')),
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ,
    notes TEXT,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at IS NULL OR ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS idx_supervision_assignments_project_id
    ON supervision_assignments (project_id);
CREATE INDEX IF NOT EXISTS idx_supervision_assignments_user_id
    ON supervision_assignments (user_id);
CREATE INDEX IF NOT EXISTS idx_supervision_assignments_company_id_status
    ON supervision_assignments (company_id, status);

-- ---------------------------------------------------------------------------
-- One inspection template: a reusable checklist. `project_id` NULL
-- means a shared template usable for any project.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_inspection_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title TEXT NOT NULL,
    description TEXT,
    category TEXT NOT NULL DEFAULT 'general',
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_supervision_templates_project_id
    ON supervision_inspection_templates (project_id);
CREATE INDEX IF NOT EXISTS idx_supervision_templates_category
    ON supervision_inspection_templates (category);

-- ---------------------------------------------------------------------------
-- One checklist item inside an inspection template.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_template_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES supervision_inspection_templates (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title TEXT NOT NULL,
    description TEXT,
    position INTEGER NOT NULL CHECK (position >= 1),
    required BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (template_id, position)
);

CREATE INDEX IF NOT EXISTS idx_supervision_template_items_template_id
    ON supervision_template_items (template_id);

-- ---------------------------------------------------------------------------
-- One conducted site inspection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_inspections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    template_id UUID REFERENCES supervision_inspection_templates (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    title TEXT NOT NULL,
    location TEXT,
    inspector_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'in_progress', 'submitted', 'approved', 'returned', 'cancelled')),
    scheduled_for TIMESTAMPTZ NOT NULL,
    conducted_at TIMESTAMPTZ,
    verdict TEXT CHECK (verdict IS NULL OR verdict IN ('pass', 'conditional_pass', 'fail')),
    summary TEXT,
    signed_by TEXT,
    signed_at TIMESTAMPTZ,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (signed_at IS NULL OR signed_by IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_supervision_inspections_project_id_scheduled_for
    ON supervision_inspections (project_id, scheduled_for);
CREATE INDEX IF NOT EXISTS idx_supervision_inspections_status
    ON supervision_inspections (status);
CREATE INDEX IF NOT EXISTS idx_supervision_inspections_inspector_id
    ON supervision_inspections (inspector_id);
CREATE INDEX IF NOT EXISTS idx_supervision_inspections_template_id
    ON supervision_inspections (template_id);

-- ---------------------------------------------------------------------------
-- One checklist item result inside a conducted inspection. The
-- title is copied from the template at creation time.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_inspection_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    inspection_id UUID NOT NULL REFERENCES supervision_inspections (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    template_item_id UUID,
    title TEXT NOT NULL,
    result TEXT NOT NULL DEFAULT 'pending'
        CHECK (result IN ('pending', 'pass', 'fail', 'na')),
    notes TEXT,
    follow_up_requested BOOLEAN NOT NULL DEFAULT FALSE,
    follow_up_task_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_supervision_inspection_items_inspection_id
    ON supervision_inspection_items (inspection_id);

-- ---------------------------------------------------------------------------
-- One progress report snapshot for a project (optionally one
-- milestone) over a period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_progress_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    milestone_id TEXT,
    title TEXT NOT NULL,
    period_start DATE NOT NULL,
    period_end DATE NOT NULL,
    reported_percent_complete NUMERIC(5, 2) NOT NULL
        CHECK (reported_percent_complete BETWEEN 0 AND 100),
    planned_percent_complete NUMERIC(5, 2)
        CHECK (planned_percent_complete IS NULL OR planned_percent_complete BETWEEN 0 AND 100),
    project_percent_complete NUMERIC(5, 2)
        CHECK (project_percent_complete IS NULL OR project_percent_complete BETWEEN 0 AND 100),
    source TEXT NOT NULL DEFAULT 'unverified'
        CHECK (source IN ('project_hook', 'manual', 'unverified')),
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'submitted', 'approved', 'returned')),
    notes TEXT,
    reported_by TEXT NOT NULL,
    reviewed_by TEXT,
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start),
    CHECK (source <> 'unverified' OR project_percent_complete IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_supervision_progress_reports_project_id_period_end
    ON supervision_progress_reports (project_id, period_end);
CREATE INDEX IF NOT EXISTS idx_supervision_progress_reports_milestone_id
    ON supervision_progress_reports (milestone_id);
CREATE INDEX IF NOT EXISTS idx_supervision_progress_reports_status
    ON supervision_progress_reports (status);

-- ---------------------------------------------------------------------------
-- One defect found on site.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_defects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    inspection_id UUID REFERENCES supervision_inspections (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    task_id TEXT,
    title TEXT NOT NULL,
    description TEXT,
    location TEXT,
    severity TEXT NOT NULL DEFAULT 'medium'
        CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'in_progress', 'resolved', 'verified')),
    reported_by TEXT NOT NULL,
    assigned_to TEXT,
    follow_up_requested BOOLEAN NOT NULL DEFAULT FALSE,
    follow_up_task_id TEXT,
    resolved_at TIMESTAMPTZ,
    verified_by TEXT,
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (verified_at IS NULL OR verified_by IS NOT NULL),
    CHECK (verified_at IS NULL OR status = 'verified')
);

CREATE INDEX IF NOT EXISTS idx_supervision_defects_project_id_status
    ON supervision_defects (project_id, status);
CREATE INDEX IF NOT EXISTS idx_supervision_defects_inspection_id
    ON supervision_defects (inspection_id);
CREATE INDEX IF NOT EXISTS idx_supervision_defects_severity
    ON supervision_defects (severity);
CREATE INDEX IF NOT EXISTS idx_supervision_defects_task_id
    ON supervision_defects (task_id);

-- ---------------------------------------------------------------------------
-- One approval step: the append-only audit trail for progress
-- reports and inspections. `target_kind` is 'progress_report' or
-- 'inspection'; `target_id` is the report / inspection id.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_kind TEXT NOT NULL
        CHECK (target_kind IN ('progress_report', 'inspection')),
    target_id UUID NOT NULL,
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    approver_id TEXT NOT NULL,
    decision TEXT NOT NULL
        CHECK (decision IN ('approved', 'returned')),
    comment TEXT,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_supervision_approvals_target
    ON supervision_approvals (target_kind, target_id, decided_at);
CREATE INDEX IF NOT EXISTS idx_supervision_approvals_project_id
    ON supervision_approvals (project_id);
CREATE INDEX IF NOT EXISTS idx_supervision_approvals_approver_id
    ON supervision_approvals (approver_id);

-- ---------------------------------------------------------------------------
-- One site daily log. One log per project per day.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_daily_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    log_date DATE NOT NULL,
    supervisor_id TEXT NOT NULL,
    weather TEXT,
    workforce_count BIGINT NOT NULL DEFAULT 0
        CHECK (workforce_count BETWEEN 0 AND 100000),
    work_done TEXT,
    materials_received TEXT,
    incidents_on_site TEXT,
    signed_by TEXT,
    signed_at TIMESTAMPTZ,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, log_date),
    CHECK (signed_at IS NULL OR signed_by IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_supervision_daily_logs_project_id_log_date
    ON supervision_daily_logs (project_id, log_date);
CREATE INDEX IF NOT EXISTS idx_supervision_daily_logs_supervisor_id
    ON supervision_daily_logs (supervisor_id);

-- ---------------------------------------------------------------------------
-- One photo evidence record. Metadata only: the bytes live in the
-- storage module under (storage_target, storage_key).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS supervision_photos (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    subject_kind TEXT NOT NULL
        CHECK (subject_kind IN ('inspection', 'defect', 'daily_log', 'progress_report')),
    subject_id UUID NOT NULL,
    storage_target TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    content_type TEXT NOT NULL
        CHECK (content_type IN ('image/png', 'image/jpeg', 'image/webp')),
    size_bytes BIGINT NOT NULL CHECK (size_bytes > 0 AND size_bytes <= 20971520),
    caption TEXT,
    uploaded_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_supervision_photos_project_id
    ON supervision_photos (project_id);
CREATE INDEX IF NOT EXISTS idx_supervision_photos_subject
    ON supervision_photos (subject_kind, subject_id);
