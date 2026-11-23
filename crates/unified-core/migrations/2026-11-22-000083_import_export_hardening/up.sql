-- Import / export hardening tables and job columns for the
-- Unified Pentagonal Project (import_export.rs).
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `mapping_templates` holds saved field-mapping templates
--     (file column -> entity column) per entity and company.
--   - `export_schedules` holds stored scheduled-export definitions
--     with their cron-style schedule string. `scheduling` is the
--     outcome of offering the schedule to the scheduler module
--     (`scheduler.enqueue`, kind `export.run`) as JSON.
--     `pii_allowed` is captured at creation time from the creator's
--     `import_export:export_pii` capability and is never widened by
--     a run.
--   - `import_sessions` holds chunked import sessions. `staged_rows`
--     from the `ImportSession` struct is NOT stored: it holds
--     validated rows only until the session completes into an import
--     job. `row_errors` is stored as JSON, `received_chunks` as a
--     JSON array of chunk indexes. `status` is `open`, `completed`,
--     or `failed`.
--   - The `ALTER TABLE` additions mirror the new `actor` /
--     `company_id` fields on `import_jobs` / `export_jobs` and the
--     new export window / redaction fields on `export_jobs`.
--     `row_limit` / `row_offset` are used instead of `limit` /
--     `offset` because those words are reserved in SQL.
--   - Secrets never appear in these tables: import files that
--     contain secret-looking columns are rejected before storage.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One saved field-mapping template.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS mapping_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    entity VARCHAR NOT NULL,
    mapping JSONB NOT NULL DEFAULT '{}'::jsonb,
    actor VARCHAR NOT NULL DEFAULT 'system',
    company_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_mapping_templates_entity
    ON mapping_templates (entity);
CREATE INDEX IF NOT EXISTS idx_mapping_templates_company_id
    ON mapping_templates (company_id);

-- ---------------------------------------------------------------------------
-- One scheduled export definition.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS export_schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    entity VARCHAR NOT NULL,
    format VARCHAR NOT NULL,
    fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    filters JSONB NOT NULL DEFAULT '{}'::jsonb,
    redact_pii BOOLEAN NOT NULL DEFAULT FALSE,
    redact_names BOOLEAN NOT NULL DEFAULT FALSE,
    pii_allowed BOOLEAN NOT NULL DEFAULT FALSE,
    schedule VARCHAR NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    company_id UUID,
    actor VARCHAR NOT NULL DEFAULT 'system',
    last_run_at TIMESTAMPTZ,
    last_export_id UUID,
    last_error TEXT,
    run_count BIGINT NOT NULL DEFAULT 0 CHECK (run_count >= 0),
    scheduling JSONB NOT NULL DEFAULT '{"enqueued": false, "message": ""}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_export_schedules_entity
    ON export_schedules (entity);
CREATE INDEX IF NOT EXISTS idx_export_schedules_enabled
    ON export_schedules (enabled);
CREATE INDEX IF NOT EXISTS idx_export_schedules_company_id
    ON export_schedules (company_id);

-- ---------------------------------------------------------------------------
-- One chunked import session.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS import_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity VARCHAR NOT NULL,
    format VARCHAR NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('open', 'completed', 'failed')),
    total_chunks INTEGER CHECK (total_chunks IS NULL OR total_chunks >= 1),
    received_chunks JSONB NOT NULL DEFAULT '[]'::jsonb,
    total_rows BIGINT NOT NULL DEFAULT 0 CHECK (total_rows >= 0),
    valid_rows BIGINT NOT NULL DEFAULT 0 CHECK (valid_rows >= 0),
    invalid_rows BIGINT NOT NULL DEFAULT 0 CHECK (invalid_rows >= 0),
    row_errors JSONB NOT NULL DEFAULT '[]'::jsonb,
    mapping JSONB NOT NULL DEFAULT '{}'::jsonb,
    actor VARCHAR NOT NULL DEFAULT 'system',
    company_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_import_sessions_entity
    ON import_sessions (entity);
CREATE INDEX IF NOT EXISTS idx_import_sessions_status
    ON import_sessions (status);
CREATE INDEX IF NOT EXISTS idx_import_sessions_company_id
    ON import_sessions (company_id);

-- ---------------------------------------------------------------------------
-- New columns on the existing job tables (from 000018).
-- ---------------------------------------------------------------------------
ALTER TABLE import_jobs ADD COLUMN IF NOT EXISTS actor VARCHAR NOT NULL DEFAULT 'system';
ALTER TABLE import_jobs ADD COLUMN IF NOT EXISTS company_id UUID;
CREATE INDEX IF NOT EXISTS idx_import_jobs_company_id
    ON import_jobs (company_id);

ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS actor VARCHAR NOT NULL DEFAULT 'system';
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS company_id UUID;
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS pii_redacted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS redacted_fields JSONB NOT NULL DEFAULT '[]'::jsonb;
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS row_limit BIGINT NOT NULL DEFAULT 10000 CHECK (row_limit >= 1);
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS row_offset BIGINT NOT NULL DEFAULT 0 CHECK (row_offset >= 0);
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS has_more BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE export_jobs ADD COLUMN IF NOT EXISTS rows_excluded_by_tenant_scope BIGINT NOT NULL DEFAULT 0 CHECK (rows_excluded_by_tenant_scope >= 0);
CREATE INDEX IF NOT EXISTS idx_export_jobs_company_id
    ON export_jobs (company_id);
