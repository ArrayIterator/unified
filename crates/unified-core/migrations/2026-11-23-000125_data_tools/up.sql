-- Data Tools (migration 000125): retention rules, run history, and the
-- minimal document_shares table.
--
-- The in-memory DashMap stores in `src/modules/data_tools/` are the
-- runtime authority (same convention as the other batch modules); these
-- tables are the durable persistence targets for the C11 write-through.
--
-- `document_shares` does NOT belong to a sharing feature that exists
-- yet: signed download URLs are stateless HMAC today and no share
-- table was ever built. Data Tools creates the minimal table here so
-- the v1 resolver has a real target for expired / revoked shares
-- instead of a stub that always reports zero. When the Documents
-- module grows real sharing, it takes over this table (and answers
-- `data_tools.cleanup.document_shares`); Data Tools then drops its
-- in-memory share map and keeps only the rule + run records. See
-- `docs/integration/INTEGRATION_NOTES_DATA_TOOLS.md`.
--
-- Retention is data, never hard-coded: every target's grace period
-- lives in `cleanup_rules` and the resolver reads it from there.
-- `retention_days` is the grace period AFTER the expiry / revocation
-- / event timestamp before a row becomes purgeable (0 = purge as soon
-- as it is expired). `cross_check` is report-only: its rule exists so
-- the target shows up in scans, but its `dry_run_default` is true and
-- the resolver never deletes for it (owners delete their own orphans
-- after answering the hook).

-- ---------------------------------------------------------------------------
-- One retention rule per cleanup target. `target` is the stable
-- machine name (see `TARGETS` in `src/modules/data_tools/types.rs`).
-- `scope` is `platform` for the v1 rules; per-tenant rules are
-- follow-up work (the column is here so the shape does not change).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cleanup_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target TEXT NOT NULL UNIQUE,
    retention_days INTEGER NOT NULL DEFAULT 30 CHECK (retention_days >= 0 AND retention_days <= 3650),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    dry_run_default BOOLEAN NOT NULL DEFAULT FALSE,
    scope TEXT NOT NULL DEFAULT 'platform'
        CHECK (scope IN ('tenant', 'platform')),
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cleanup_rules_target
    ON cleanup_rules (target);
CREATE INDEX IF NOT EXISTS idx_cleanup_rules_scope_enabled
    ON cleanup_rules (scope, enabled);

-- Default rules (configurable defaults, not hard-coded retention).
-- `document_shares`: 30 days of grace after expiry / revocation.
-- `document_upload_sessions`: 7 days after the session expired.
-- `auth_revoked_tokens`: 30 days after the token expired.
-- `document_downloads`: 90 days of download history.
-- `spool_temps`: 1 day (temps older than 24h).
-- `cross_check`: report-only, never deletes (dry_run_default true).
INSERT INTO cleanup_rules (target, retention_days, enabled, dry_run_default, scope, description)
VALUES
    ('document_shares', 30, TRUE, FALSE, 'platform', 'Document shares expired or revoked beyond grace.'),
    ('document_upload_sessions', 7, TRUE, FALSE, 'platform', 'Expired chunked-upload sessions and their spool dirs.'),
    ('auth_revoked_tokens', 30, TRUE, FALSE, 'platform', 'Revoked auth tokens whose expiry passed.'),
    ('document_downloads', 90, TRUE, FALSE, 'platform', 'Download log rows older than retention.'),
    ('spool_temps', 1, TRUE, FALSE, 'platform', 'Temporary / spool files older than 24h.'),
    ('cross_check', 0, TRUE, TRUE, 'platform', 'Files on disk with no DB row / DB rows with no file. Report-only: never deletes.')
ON CONFLICT (target) DO NOTHING;

-- ---------------------------------------------------------------------------
-- One cleanup run: started/finished timestamps, the dry-run flag, the
-- per-target removed counts and any errors as JSON. This table IS the
-- audit record for a run (the `data_tools.run.completed` hook event
-- carries the same payload for the Audit module to subscribe to).
-- Audit events themselves are NEVER deleted by the cleaner.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS cleanup_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    dry_run BOOLEAN NOT NULL DEFAULT TRUE,
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'completed', 'failed')),
    counts JSONB NOT NULL DEFAULT '{}'::jsonb,
    errors JSONB NOT NULL DEFAULT '[]'::jsonb,
    triggered_by TEXT NOT NULL DEFAULT 'manual'
        CHECK (triggered_by IN ('manual', 'scheduler', 'api')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cleanup_runs_started_at
    ON cleanup_runs (started_at DESC);
CREATE INDEX IF NOT EXISTS idx_cleanup_runs_status
    ON cleanup_runs (status);

-- ---------------------------------------------------------------------------
-- Minimal document share row (see the header note): a share grants
-- access to one document until `expires_at` (NULL = no expiry) and
-- can be revoked. The cleaner deletes only the share row — never
-- the document, never any audit event — once the share is expired
-- or revoked beyond the `document_shares` rule grace period.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS document_shares (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES documents (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    scope TEXT NOT NULL DEFAULT 'owner'
        CHECK (scope IN ('owner', 'company', 'module', 'public')),
    expires_at TIMESTAMPTZ,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    revoked_at TIMESTAMPTZ,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_document_shares_document_id
    ON document_shares (document_id);
CREATE INDEX IF NOT EXISTS idx_document_shares_expires_at
    ON document_shares (expires_at);
CREATE INDEX IF NOT EXISTS idx_document_shares_revoked_at
    ON document_shares (revoked_at);
