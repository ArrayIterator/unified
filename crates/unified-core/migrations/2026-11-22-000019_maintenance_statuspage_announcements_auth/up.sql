-- Maintenance, status page, announcement, and Google / 2FA
-- tables for the Unified Pentagonal Project (maintenance.rs,
-- statuspage.rs, announcements.rs, auth_manager.rs): maintenance
-- windows with history, status components with daily uptime and
-- incidents, announcements with changelog and read receipts, and
-- the Google connector records plus 2FA recovery-code hashes.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `maintenance_windows.status` values are the lifecycle
--     states used by the module (`scheduled`, `active`,
--     `completed`, `cancelled`); `maintenance_history.event` is
--     `scheduled`, `started`, `ended`, `cancelled`, `activated`
--     (manual), or `deactivated` (manual).
--   - `status_daily_uptime` is keyed by (component_id, date): one
--     row per component per calendar day (UTC, `YYYY-MM-DD`).
--     `uptime_percent` is a measurement, not money.
--   - `status_incidents.affected_components` is the JSON array of
--     component ids. `severity` is `minor`, `major`, or
--     `critical`.
--   - `announcement_read_receipts` is keyed by (announcement_id,
--     user_id): one receipt per reader. `user_id` is VARCHAR (the
--     announcements module keeps user ids as strings).
--   - `auth_recovery_codes` stores hashes only, never plaintext
--     codes: one row per code, keyed by (user_id, code_hash).
--     `user_id` is VARCHAR (the auth manager validates it as a
--     string of at most 256 characters).
--   - `google_accounts` stores OAuth tokens. The tokens are
--     write-only: only the account metadata view ever leaves the
--     module. Token values must be encrypted before insert, the
--     same reservation as the `*_encrypted` columns of
--     `vcs_provider_settings` (no encryption helper exists yet).
--   - `google_oauth_states` (the pending OAuth authorization
--     flows, with PKCE code verifiers) are intentionally NOT
--     persisted: they are short-lived flow state, and persisting
--     a code verifier would widen the exposure of a credential.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- A maintenance window: a scheduled period of degraded or
-- unavailable service.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS maintenance_windows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    affected_services JSONB NOT NULL DEFAULT '[]'::jsonb,
    status VARCHAR NOT NULL CHECK (status IN ('scheduled', 'active', 'completed', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    activated_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS idx_maintenance_windows_status
    ON maintenance_windows (status);
CREATE INDEX IF NOT EXISTS idx_maintenance_windows_starts_at
    ON maintenance_windows (starts_at);

-- ---------------------------------------------------------------------------
-- One maintenance history entry: a window lifecycle event.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS maintenance_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    window_id UUID NOT NULL REFERENCES maintenance_windows (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    event VARCHAR NOT NULL CHECK (event IN ('scheduled', 'started', 'ended', 'cancelled', 'activated', 'deactivated')),
    message TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_maintenance_history_window_id
    ON maintenance_history (window_id);

-- ---------------------------------------------------------------------------
-- One status-page component.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS status_components (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    "group" VARCHAR,
    status VARCHAR NOT NULL,
    sort_order BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_status_components_status
    ON status_components (status);

-- ---------------------------------------------------------------------------
-- Daily uptime for one component: one row per component per day.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS status_daily_uptime (
    component_id UUID NOT NULL REFERENCES status_components (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    date VARCHAR NOT NULL CHECK (date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'),
    uptime_percent DOUBLE PRECISION NOT NULL CHECK (uptime_percent >= 0 AND uptime_percent <= 100),
    minutes_down BIGINT NOT NULL DEFAULT 0 CHECK (minutes_down >= 0),
    total_minutes BIGINT NOT NULL DEFAULT 1440 CHECK (total_minutes >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (component_id, date)
);

-- ---------------------------------------------------------------------------
-- One status incident.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS status_incidents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status VARCHAR NOT NULL,
    severity VARCHAR NOT NULL CHECK (severity IN ('minor', 'major', 'critical')),
    affected_components JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_status_incidents_status
    ON status_incidents (status);
CREATE INDEX IF NOT EXISTS idx_status_incidents_severity
    ON status_incidents (severity);

-- ---------------------------------------------------------------------------
-- One update posted to an incident.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS status_incident_updates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id UUID NOT NULL REFERENCES status_incidents (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    status VARCHAR NOT NULL,
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_status_incident_updates_incident_id
    ON status_incident_updates (incident_id);

-- ---------------------------------------------------------------------------
-- One announcement.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS announcements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    body TEXT NOT NULL,
    audience VARCHAR NOT NULL DEFAULT '',
    status VARCHAR NOT NULL,
    publish_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_announcements_status
    ON announcements (status);
CREATE INDEX IF NOT EXISTS idx_announcements_audience
    ON announcements (audience);

-- ---------------------------------------------------------------------------
-- One changelog entry.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS changelog_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version VARCHAR NOT NULL,
    date VARCHAR NOT NULL CHECK (date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'),
    category VARCHAR NOT NULL DEFAULT '',
    title VARCHAR NOT NULL,
    body TEXT NOT NULL,
    link VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_changelog_entries_version
    ON changelog_entries (version);

-- ---------------------------------------------------------------------------
-- One announcement read receipt: one row per (announcement, user).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS announcement_read_receipts (
    announcement_id UUID NOT NULL REFERENCES announcements (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    read_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (announcement_id, user_id)
);

-- ---------------------------------------------------------------------------
-- 2FA recovery codes: hash only, never the plaintext. One row per
-- code; a set holds ten codes.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS auth_recovery_codes (
    user_id VARCHAR NOT NULL,
    code_hash VARCHAR NOT NULL,
    used BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    used_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, code_hash)
);

CREATE INDEX IF NOT EXISTS idx_auth_recovery_codes_user_id
    ON auth_recovery_codes (user_id);

-- ---------------------------------------------------------------------------
-- A linked Google account's tokens. Never serialized: only the
-- account metadata view leaves the module.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS google_accounts (
    account_id VARCHAR PRIMARY KEY,
    email VARCHAR NOT NULL,
    name VARCHAR,
    access_token_encrypted TEXT NOT NULL DEFAULT '',
    refresh_token_encrypted TEXT NOT NULL DEFAULT '',
    expires_at TIMESTAMPTZ NOT NULL,
    scopes JSONB NOT NULL DEFAULT '[]'::jsonb,
    linked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_google_accounts_email
    ON google_accounts (email);

-- ---------------------------------------------------------------------------
-- A Google Drive folder linked as a named storage target.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS google_drive_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    account_id VARCHAR NOT NULL REFERENCES google_accounts (account_id) ON DELETE CASCADE ON UPDATE NO ACTION,
    email VARCHAR NOT NULL,
    folder_id VARCHAR,
    status VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_google_drive_links_account_id
    ON google_drive_links (account_id);
