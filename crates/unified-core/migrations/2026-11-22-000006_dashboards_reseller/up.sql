-- Reseller and dashboard tables for the Unified Pentagonal Project
-- (dashboards.rs): reseller-to-client links, admin view-as
-- (impersonation) sessions, and the dashboard path settings.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `reseller_links` is keyed by (reseller_id, client_id): one
--     link per pair, exactly like the in-memory DashMap key.
--   - `view_as_sessions` is keyed by `admin_id`: one active
--     view-as session per admin, exactly like the in-memory DashMap
--     key. The row records the admin so the session can be ended
--     and audited; the impersonated session itself keeps only the
--     target user's capabilities.
--   - `dashboard_settings` is a singleton row keyed by `key =
--     'global'`, the same pattern as `vcs_provider_settings`.
--     The admin and user dashboard paths are single-segment paths
--     (see `DashboardsState::first_segment`); anything else is
--     ignored by the code, so no CHECK is added here beyond
--     non-empty values.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One reseller-to-client link. `verified` is true when the client id
-- was confirmed to exist in the users table at link time.
CREATE TABLE IF NOT EXISTS reseller_links (
    reseller_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    client_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (reseller_id, client_id)
);

CREATE INDEX IF NOT EXISTS idx_reseller_links_client_id
    ON reseller_links (client_id);

-- One active admin view-as session, keyed by the admin's user id.
CREATE TABLE IF NOT EXISTS view_as_sessions (
    admin_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    target_user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    reason TEXT NOT NULL DEFAULT '',
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_view_as_sessions_target_user_id
    ON view_as_sessions (target_user_id);
CREATE INDEX IF NOT EXISTS idx_view_as_sessions_expires_at
    ON view_as_sessions (expires_at);

-- Dashboard settings (singleton row): the admin and user dashboard
-- path prefixes and the view-as session lifetime in minutes.
CREATE TABLE IF NOT EXISTS dashboard_settings (
    key VARCHAR PRIMARY KEY CHECK (key = 'global'),
    admin_prefix VARCHAR NOT NULL DEFAULT 'admin',
    user_prefix VARCHAR NOT NULL DEFAULT 'dashboard',
    view_as_ttl_minutes BIGINT NOT NULL DEFAULT 30 CHECK (view_as_ttl_minutes > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
