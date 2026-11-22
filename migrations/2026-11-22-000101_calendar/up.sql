-- Calendar module tables for the Unified Pentagonal Project
-- (`src/modules/calendar.rs`): revocable iCalendar subscription
-- feeds, per-company / per-user settings, and per-user per-module
-- visibility toggles. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the todos / library precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `tenant_id` is on EVERY table: calendar data is tenant
--     business data and is resolved through the tenant context
--     (see the "Tenant scoping" section in `src/modules/calendar.rs`).
--     The `tenant_id` values are uuidv7 tenant identifiers stored
--     as TEXT (no foreign key: tenants live in the control plane
--     in the `public` schema and this module must not assume their
--     table or migration order). When batch 17's schema-per-tenant
--     plumbing lands, these tables move into the per-tenant schema;
--     the `tenant_id` column stays as the filter key.
--   - `user_id` / `company_id` columns are plain TEXT with NO
--     foreign key on purpose: users and companies live in other
--     modules and this module must not assume their table or
--     migration order; referential rules are enforced by the
--     module code instead.
--   - No secret is stored in any table here: `calendar_feeds`
--     stores only the SHA-256 hash of an issued feed token, never
--     the raw token. The HMAC signing key lives only in process
--     memory and is never persisted.
--   - No money in this module.
--   - All ids are UUID, all timestamps are Timestamptz.
--   - `calendar_settings.weekend_days` holds ISO weekday numbers
--     (1=Monday..7=Sunday) as a JSON array; `create_link_templates`
--     holds module id -> deep-link template pairs as JSONB.
--   - `calendar_sources` has one row per (tenant, user, module);
--     a missing row means the module is visible.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE IF NOT EXISTS calendar_feeds (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    company_id TEXT,
    user_id TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    modules JSONB NOT NULL DEFAULT '[]'::jsonb,
    label TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS calendar_feeds_tenant_user_idx
    ON calendar_feeds (tenant_id, user_id);
CREATE INDEX IF NOT EXISTS calendar_feeds_token_hash_idx
    ON calendar_feeds (token_hash);
CREATE INDEX IF NOT EXISTS calendar_feeds_expires_idx
    ON calendar_feeds (expires_at);

CREATE TABLE IF NOT EXISTS calendar_settings (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('user', 'company')),
    owner_id TEXT NOT NULL,
    weekend_days JSONB NOT NULL DEFAULT '[6, 7]'::jsonb,
    working_hours_start TEXT NOT NULL DEFAULT '09:00',
    working_hours_end TEXT NOT NULL DEFAULT '17:00',
    default_view TEXT NOT NULL DEFAULT 'month'
        CHECK (default_view IN ('month', 'week', 'day', 'agenda')),
    timezone TEXT NOT NULL DEFAULT 'UTC',
    holiday_country TEXT NOT NULL DEFAULT '',
    create_link_templates JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, scope, owner_id)
);

CREATE INDEX IF NOT EXISTS calendar_settings_tenant_idx
    ON calendar_settings (tenant_id);

CREATE TABLE IF NOT EXISTS calendar_sources (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    module TEXT NOT NULL,
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, user_id, module)
);

CREATE INDEX IF NOT EXISTS calendar_sources_tenant_user_idx
    ON calendar_sources (tenant_id, user_id);
