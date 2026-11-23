-- Compliance Center tables for the Unified Pentagonal Project
-- (`src/modules/compliance.rs`): data export requests, data
-- deletion requests with per-module audit items, an append-only
-- consent log, retention policies and their review runs,
-- processing-activity records (RoPA), checklists with evidence,
-- and a breach log with an append-only timeline.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/compliance.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `compliance_export_requests.status` is `requested`,
--     `processing`, `ready`, `failed`, or `expired` (`expired`
--     is derived at read time once `download_expires_at` passes).
--     `storage_target` + `storage_key` are a storage-module
--     reference (no file bytes live here); the export file itself
--     is assembled by the owning modules. `requester_id` is a
--     plain VARCHAR user id: the module identifies callers by the
--     `X-User-Id` header, exactly like `store_orders.user_id` in
--     migration 000002. No foreign key to `users` is stored here.
--   - `compliance_deletion_requests.status` is `requested`,
--     `approved`, `rejected`, `executing`, or `executed`.
--     Approval must come from a manager other than the requester
--     (enforced in the module). This module never deletes other
--     modules' rows directly.
--   - `compliance_deletion_items` is the per-module audit record
--     of WHAT was deleted: `module_id` names the owning module,
--     `subject` what is to be deleted, `status` is `pending`,
--     `dispatched`, or `executed`, and `detail` records what was
--     actually deleted once the owning module confirms.
--   - `compliance_consents` is APPEND-ONLY: `granted` true means
--     consent given, false means withdrawn. A withdrawal is a new
--     row; rows are never updated or deleted, and the module
--     exposes no update or delete route for them.
--   - `compliance_retention_policies` has one row per data
--     category (unique). `review_action` names what the review
--     does. Enforcement is the `compliance.retention.review`
--     scheduler job, which only flags items — nothing here ever
--     auto-deletes.
--   - `compliance_retention_reviews` records review runs:
--     `items_flagged` and `scheduler_status` (`not_scheduled`,
--     `queued`, or `declined`).
--   - `compliance_processing_records` is the RoPA table:
--     purpose, data categories, recipients, retention, and
--     lawful basis.
--   - `compliance_checklists.status` is `open`, `in_progress`,
--     or `done`. Evidence is a storage-module reference
--     (`evidence_storage_target` + `evidence_storage_key`).
--   - `compliance_breach_log.status` is `reported`,
--     `investigating`, `contained`, `notified`, or `closed`;
--     `notification_status` is `not_started`, `in_progress`, or
--     `notified`. `compliance_breach_timeline` is append-only.

CREATE TABLE IF NOT EXISTS compliance_export_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    requester_id VARCHAR(255) NOT NULL,
    scope TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(20) NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'processing', 'ready', 'failed', 'expired')),
    storage_target VARCHAR(512),
    storage_key VARCHAR(512),
    download_expires_at TIMESTAMPTZ,
    failure_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_export_requests_requester_idx ON compliance_export_requests (requester_id);
CREATE INDEX IF NOT EXISTS compliance_export_requests_status_idx ON compliance_export_requests (status);
CREATE INDEX IF NOT EXISTS compliance_export_requests_created_at_idx ON compliance_export_requests (created_at);

CREATE TABLE IF NOT EXISTS compliance_deletion_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    requester_id VARCHAR(255) NOT NULL,
    reason TEXT NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'approved', 'rejected', 'executing', 'executed')),
    approved_by VARCHAR(255),
    approved_at TIMESTAMPTZ,
    executed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_deletion_requests_requester_idx ON compliance_deletion_requests (requester_id);
CREATE INDEX IF NOT EXISTS compliance_deletion_requests_status_idx ON compliance_deletion_requests (status);
CREATE INDEX IF NOT EXISTS compliance_deletion_requests_created_at_idx ON compliance_deletion_requests (created_at);

CREATE TABLE IF NOT EXISTS compliance_deletion_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deletion_request_id UUID NOT NULL REFERENCES compliance_deletion_requests (id) ON DELETE CASCADE,
    module_id VARCHAR(200) NOT NULL,
    subject TEXT NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'dispatched', 'executed')),
    detail TEXT,
    executed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_deletion_items_request_idx ON compliance_deletion_items (deletion_request_id);
CREATE INDEX IF NOT EXISTS compliance_deletion_items_module_idx ON compliance_deletion_items (module_id);
CREATE INDEX IF NOT EXISTS compliance_deletion_items_status_idx ON compliance_deletion_items (status);

CREATE TABLE IF NOT EXISTS compliance_consents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR(255) NOT NULL,
    purpose VARCHAR(500) NOT NULL,
    consent_text_version VARCHAR(200) NOT NULL,
    granted BOOLEAN NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_consents_user_idx ON compliance_consents (user_id);
CREATE INDEX IF NOT EXISTS compliance_consents_purpose_idx ON compliance_consents (purpose);
CREATE INDEX IF NOT EXISTS compliance_consents_recorded_at_idx ON compliance_consents (recorded_at);

CREATE TABLE IF NOT EXISTS compliance_retention_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    data_category VARCHAR(120) NOT NULL UNIQUE,
    retain_days BIGINT NOT NULL CHECK (retain_days >= 0 AND retain_days <= 36500),
    review_action VARCHAR(500) NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_retention_policies_category_idx ON compliance_retention_policies (data_category);

CREATE TABLE IF NOT EXISTS compliance_retention_reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    policy_id UUID NOT NULL REFERENCES compliance_retention_policies (id) ON DELETE CASCADE,
    items_flagged BIGINT NOT NULL DEFAULT 0 CHECK (items_flagged >= 0),
    notes TEXT,
    scheduler_status VARCHAR(20) NOT NULL DEFAULT 'not_scheduled' CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    scheduler_job_id VARCHAR(255),
    reviewed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_retention_reviews_policy_idx ON compliance_retention_reviews (policy_id);
CREATE INDEX IF NOT EXISTS compliance_retention_reviews_created_at_idx ON compliance_retention_reviews (created_at);

CREATE TABLE IF NOT EXISTS compliance_processing_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    purpose VARCHAR(500) NOT NULL,
    data_categories TEXT[] NOT NULL DEFAULT '{}',
    recipients TEXT[] NOT NULL DEFAULT '{}',
    retention VARCHAR(500) NOT NULL,
    lawful_basis VARCHAR(120) NOT NULL,
    owner VARCHAR(200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_processing_records_purpose_idx ON compliance_processing_records (purpose);
CREATE INDEX IF NOT EXISTS compliance_processing_records_basis_idx ON compliance_processing_records (lawful_basis);

CREATE TABLE IF NOT EXISTS compliance_checklists (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    description TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'in_progress', 'done')),
    evidence_storage_target VARCHAR(512),
    evidence_storage_key VARCHAR(512),
    due_date TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_checklists_status_idx ON compliance_checklists (status);
CREATE INDEX IF NOT EXISTS compliance_checklists_created_at_idx ON compliance_checklists (created_at);

CREATE TABLE IF NOT EXISTS compliance_breach_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    description TEXT NOT NULL,
    discovered_at TIMESTAMPTZ NOT NULL,
    affected_scope VARCHAR(500) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'reported' CHECK (status IN ('reported', 'investigating', 'contained', 'notified', 'closed')),
    notification_status VARCHAR(20) NOT NULL DEFAULT 'not_started' CHECK (notification_status IN ('not_started', 'in_progress', 'notified')),
    reported_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_breach_log_status_idx ON compliance_breach_log (status);
CREATE INDEX IF NOT EXISTS compliance_breach_log_notification_idx ON compliance_breach_log (notification_status);
CREATE INDEX IF NOT EXISTS compliance_breach_log_discovered_at_idx ON compliance_breach_log (discovered_at);

CREATE TABLE IF NOT EXISTS compliance_breach_timeline (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    breach_id UUID NOT NULL REFERENCES compliance_breach_log (id) ON DELETE CASCADE,
    author_id VARCHAR(255) NOT NULL,
    entry TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS compliance_breach_timeline_breach_idx ON compliance_breach_timeline (breach_id);
CREATE INDEX IF NOT EXISTS compliance_breach_timeline_recorded_at_idx ON compliance_breach_timeline (recorded_at);
