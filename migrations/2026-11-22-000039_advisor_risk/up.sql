-- Advisor + Risk Mitigation tables for the Unified Pentagonal Project
-- (`src/modules/advisor.rs`, `src/modules/risk.rs`).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - Money is NUMERIC(19,4) everywhere. The advisor stores
--     suggestion figures (overdue balances, collection totals,
--     subscription amounts) in `advisor_suggestions.figures` as
--     JSONB, so no money column exists in these tables; when a
--     money column is added later it MUST be NUMERIC(19,4) with
--     `diesel::data_types::PgNumeric` on the Rust side.
--   - `company_id` is UUID, matching `invoices.company_id`. User
--     and owner ids are VARCHAR, matching `invoices.user_id`:
--     user ids are opaque strings in this codebase, not UUIDs.
--   - Advisor suggestions are rule outputs, not user input: the
--     severity formula that graded a suggestion lives in
--     `severity_reason` next to the `figures` JSONB it was computed
--     from, so no severity is ever stored without its data cited.
--     `dedupe_key` is the (company, rule, source) identity used to
--     refresh an open suggestion instead of creating a duplicate.
--   - Risk scores are SMALLINT 1..=25, computed server-side as
--     likelihood * impact; the CHECK constraints below enforce the
--     same range the module validates (likelihood and impact each
--     1..=5). `band` is a derived display value, not a second
--     source of truth: low 1-4, medium 5-9, high 10-16, critical
--     17-25.
--   - `risk_mitigation_tasks.external_project_task_id` is the id of
--     the backing task in the Projects module, once it answers
--     `risk.mitigation_task_requested`. It stays NULL while the
--     projects module has no handler for that event (see the
--     convergence note in INTEGRATION_NOTES_BATCH3_B4.md).
--   - Checklist instances snapshot their template items at
--     creation time (`risk_checklist_instance_items` copies title,
--     required flag, and sort order), so editing a template never
--     rewrites an in-flight checklist run.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One advisor recommendation, produced by one named rule.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS advisor_suggestions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID,
    rule_id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    body TEXT NOT NULL,
    recommended_action TEXT NOT NULL,
    severity VARCHAR NOT NULL
        CHECK (severity IN ('info', 'warning', 'critical')),
    severity_reason TEXT NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'new'
        CHECK (status IN ('new', 'acknowledged', 'dismissed', 'snoozed')),
    source_type VARCHAR NOT NULL,
    source_id VARCHAR NOT NULL,
    source_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    figures JSONB NOT NULL DEFAULT '{}'::jsonb,
    dedupe_key VARCHAR NOT NULL,
    expires_at TIMESTAMPTZ,
    snoozed_until TIMESTAMPTZ,
    acknowledged_by VARCHAR,
    acknowledged_at TIMESTAMPTZ,
    dismissed_by VARCHAR,
    dismissed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_advisor_suggestions_dedupe_key
    ON advisor_suggestions (dedupe_key);
CREATE INDEX IF NOT EXISTS idx_advisor_suggestions_company_id
    ON advisor_suggestions (company_id);
CREATE INDEX IF NOT EXISTS idx_advisor_suggestions_rule_id
    ON advisor_suggestions (rule_id);
CREATE INDEX IF NOT EXISTS idx_advisor_suggestions_severity
    ON advisor_suggestions (severity);
CREATE INDEX IF NOT EXISTS idx_advisor_suggestions_status
    ON advisor_suggestions (status);
CREATE INDEX IF NOT EXISTS idx_advisor_suggestions_created_at
    ON advisor_suggestions (created_at);

-- ---------------------------------------------------------------------------
-- One rule evaluation run: the audit trail of what the advisor
-- evaluated and what it produced.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS advisor_rule_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID,
    rule_id VARCHAR NOT NULL,
    ran_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    suggestions_created INTEGER NOT NULL DEFAULT 0
        CHECK (suggestions_created >= 0),
    suggestions_updated INTEGER NOT NULL DEFAULT 0
        CHECK (suggestions_updated >= 0),
    inputs_summary JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_advisor_rule_runs_company_id
    ON advisor_rule_runs (company_id);
CREATE INDEX IF NOT EXISTS idx_advisor_rule_runs_rule_id
    ON advisor_rule_runs (rule_id);
CREATE INDEX IF NOT EXISTS idx_advisor_rule_runs_ran_at
    ON advisor_rule_runs (ran_at);

-- ---------------------------------------------------------------------------
-- One risk in the register. Score and band are derived, never
-- supplied by the client.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID,
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    category VARCHAR NOT NULL
        CHECK (category IN ('operational', 'financial', 'security', 'compliance', 'strategic', 'reputational', 'technical')),
    likelihood SMALLINT NOT NULL
        CHECK (likelihood >= 1 AND likelihood <= 5),
    impact SMALLINT NOT NULL
        CHECK (impact >= 1 AND impact <= 5),
    score SMALLINT NOT NULL
        CHECK (score >= 1 AND score <= 25),
    band VARCHAR NOT NULL
        CHECK (band IN ('low', 'medium', 'high', 'critical')),
    owner_id VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'identified'
        CHECK (status IN ('identified', 'assessed', 'mitigating', 'monitored', 'closed', 'accepted')),
    next_review_at TIMESTAMPTZ,
    alert_threshold SMALLINT NOT NULL DEFAULT 15
        CHECK (alert_threshold >= 1 AND alert_threshold <= 25),
    created_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risks_company_id
    ON risks (company_id);
CREATE INDEX IF NOT EXISTS idx_risks_category
    ON risks (category);
CREATE INDEX IF NOT EXISTS idx_risks_status
    ON risks (status);
CREATE INDEX IF NOT EXISTS idx_risks_band
    ON risks (band);
CREATE INDEX IF NOT EXISTS idx_risks_score
    ON risks (score);
CREATE INDEX IF NOT EXISTS idx_risks_owner_id
    ON risks (owner_id);
CREATE INDEX IF NOT EXISTS idx_risks_next_review_at
    ON risks (next_review_at);

-- ---------------------------------------------------------------------------
-- One mitigation plan, owned by one risk.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_mitigation_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    risk_id UUID NOT NULL REFERENCES risks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    target_date TIMESTAMPTZ,
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'completed', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_mitigation_plans_risk_id
    ON risk_mitigation_plans (risk_id);
CREATE INDEX IF NOT EXISTS idx_risk_mitigation_plans_status
    ON risk_mitigation_plans (status);

-- ---------------------------------------------------------------------------
-- One mitigation task. The backing Projects task is requested via
-- the `risk.mitigation_task_requested` hook event; its id lands in
-- `external_project_task_id` once the Projects module answers.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_mitigation_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID NOT NULL REFERENCES risk_mitigation_plans (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    risk_id UUID NOT NULL REFERENCES risks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    assignee_id VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'in_progress', 'done', 'cancelled')),
    due_date TIMESTAMPTZ,
    external_project_task_id VARCHAR,
    awaiting_projects_handler BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_mitigation_tasks_plan_id
    ON risk_mitigation_tasks (plan_id);
CREATE INDEX IF NOT EXISTS idx_risk_mitigation_tasks_risk_id
    ON risk_mitigation_tasks (risk_id);
CREATE INDEX IF NOT EXISTS idx_risk_mitigation_tasks_status
    ON risk_mitigation_tasks (status);
CREATE INDEX IF NOT EXISTS idx_risk_mitigation_tasks_assignee_id
    ON risk_mitigation_tasks (assignee_id);

-- ---------------------------------------------------------------------------
-- One compliance checklist template.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_checklist_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    category VARCHAR NOT NULL,
    created_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_checklist_templates_category
    ON risk_checklist_templates (category);

-- ---------------------------------------------------------------------------
-- One item of a checklist template.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_checklist_template_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES risk_checklist_templates (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    required BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_checklist_template_items_template_id
    ON risk_checklist_template_items (template_id);

-- ---------------------------------------------------------------------------
-- One per-company run of a checklist template, optionally linked to
-- a risk. Deleting the risk unlinks the instance (SET NULL) instead
-- of deleting the compliance record.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_checklist_instances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES risk_checklist_templates (id)
        ON DELETE RESTRICT ON UPDATE NO ACTION,
    company_id UUID,
    risk_id UUID REFERENCES risks (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'in_progress'
        CHECK (status IN ('in_progress', 'completed')),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_checklist_instances_template_id
    ON risk_checklist_instances (template_id);
CREATE INDEX IF NOT EXISTS idx_risk_checklist_instances_company_id
    ON risk_checklist_instances (company_id);
CREATE INDEX IF NOT EXISTS idx_risk_checklist_instances_risk_id
    ON risk_checklist_instances (risk_id);
CREATE INDEX IF NOT EXISTS idx_risk_checklist_instances_status
    ON risk_checklist_instances (status);

-- ---------------------------------------------------------------------------
-- One item of a checklist instance. Title, required flag, and sort
-- order are snapshotted from the template item at creation time.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_checklist_instance_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id UUID NOT NULL REFERENCES risk_checklist_instances (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    template_item_id UUID NOT NULL,
    title VARCHAR NOT NULL,
    required BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    evidence TEXT NOT NULL DEFAULT '',
    completed_by VARCHAR,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_checklist_instance_items_instance_id
    ON risk_checklist_instance_items (instance_id);
CREATE INDEX IF NOT EXISTS idx_risk_checklist_instance_items_completed
    ON risk_checklist_instance_items (completed);

-- ---------------------------------------------------------------------------
-- One recorded risk alert (threshold crossing or overdue review):
-- the alerts history.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS risk_alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    risk_id UUID NOT NULL REFERENCES risks (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    company_id UUID,
    kind VARCHAR NOT NULL
        CHECK (kind IN ('threshold_crossed', 'review_overdue')),
    message TEXT NOT NULL,
    score SMALLINT CHECK (score IS NULL OR (score >= 1 AND score <= 25)),
    notified_user_id VARCHAR,
    figures JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_risk_alerts_risk_id
    ON risk_alerts (risk_id);
CREATE INDEX IF NOT EXISTS idx_risk_alerts_company_id
    ON risk_alerts (company_id);
CREATE INDEX IF NOT EXISTS idx_risk_alerts_kind
    ON risk_alerts (kind);
CREATE INDEX IF NOT EXISTS idx_risk_alerts_created_at
    ON risk_alerts (created_at);
