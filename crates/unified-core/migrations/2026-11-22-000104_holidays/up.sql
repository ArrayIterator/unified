-- Holidays module tables for the Unified Pentagonal Project
-- (`src/modules/holidays.rs`): provider registry, country list,
-- per-tenant sync configs, sync runs, synced holidays, and manual
-- overrides. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Tenant scoping (standing rule): `holiday_sync_configs`,
-- `holiday_sync_runs`, and `holiday_overrides` are TENANT business
-- data and carry `tenant_id` (uuidv7) + `company_id` on every row.
-- `holiday_providers` and `holiday_countries` are platform
-- reference data (no tenant id). `holidays` is shared reference
-- data keyed by country+year (no tenant id); a tenant only ever
-- sees the countries its configs select, enforced by the module
-- code on every read path.
--
-- Schema-per-tenant note: this SQL is deliberately schema-agnostic
-- (no schema qualification). The migration runner sets
-- `search_path` to the tenant schema when it runs, so these tables
-- are created inside the tenant schema, never in `public`. The
-- schema name comes only from `src/core/tenancy.rs::schema_name()`
-- (batch 17, on disk) via `tenancy::run_tenant_migrations`; no
-- change to this file is needed for the per-tenant placement.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the todos / library / calendar precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `tenant_id` / `company_id` / `provider_id` / `config_id`
--     columns are plain TEXT / UUID with NO foreign key on purpose:
--     tenants, companies, and providers live in other modules and
--     this module must not assume their table or migration order.
--     Cross-table references inside this module are also plain
--     TEXT/UUID without foreign keys, so the tables can be created
--     and dropped in any order and a missing parent row never
--     blocks a write; referential rules are enforced by the module
--     code instead.
--   - No plaintext secret is stored in any table here:
--     `holiday_providers.api_key_encrypted` holds only `v1.`
--     ciphertext (HMAC keystream + HMAC tag, same format as the
--     ai.rs provider keys) and `key_fingerprint` holds only the
--     first 12 hex characters of the SHA-256 of the key.
--   - `holidays` is deduplicated by (country, date, name): the
--     unique index below matches the module rule that a re-sync
--     upserts in place and never duplicates.
--   - `holiday_overrides` allows at most one override per
--     (tenant, country, date): the unique index below matches the
--     module rule that a second override for the same date is a
--     409 conflict.
--   - All ids are UUID, all timestamps are Timestamptz, all dates
--     are DATE.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Provider registry (platform reference data).
CREATE TABLE IF NOT EXISTS holiday_providers (
    id TEXT PRIMARY KEY CHECK (char_length(id) BETWEEN 1 AND 64),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    kind TEXT NOT NULL CHECK (kind IN ('nager_date', 'calendarific', 'abstract_api', 'google_ical', 'generic_ical')),
    base_url TEXT NOT NULL CHECK (char_length(base_url) <= 500),
    allowed_hosts JSONB NOT NULL DEFAULT '[]'::jsonb,
    api_key_required BOOLEAN NOT NULL DEFAULT FALSE,
    api_key_encrypted TEXT CHECK (api_key_encrypted IS NULL OR api_key_encrypted LIKE 'v1.%'),
    key_fingerprint TEXT CHECK (key_fingerprint IS NULL OR char_length(key_fingerprint) = 12),
    min_interval_secs BIGINT NOT NULL DEFAULT 1 CHECK (min_interval_secs BETWEEN 0 AND 3600),
    max_requests_per_minute BIGINT NOT NULL DEFAULT 60 CHECK (max_requests_per_minute BETWEEN 1 AND 1000),
    calendar_ids JSONB NOT NULL DEFAULT '{}'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    builtin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Country list (platform reference data, ISO 3166-1 alpha-2).
CREATE TABLE IF NOT EXISTS holiday_countries (
    code TEXT PRIMARY KEY CHECK (char_length(code) = 2),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Per-tenant sync configs (tenant business data).
CREATE TABLE IF NOT EXISTS holiday_sync_configs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    company_id TEXT NOT NULL CHECK (char_length(company_id) BETWEEN 1 AND 128),
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    fallback_provider_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    countries JSONB NOT NULL DEFAULT '[]'::jsonb,
    years JSONB NOT NULL DEFAULT '[]'::jsonb,
    schedule TEXT NOT NULL CHECK (char_length(schedule) BETWEEN 1 AND 100),
    feed_url TEXT CHECK (feed_url IS NULL OR char_length(feed_url) BETWEEN 1 AND 2000),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_holiday_sync_configs_tenant
    ON holiday_sync_configs (tenant_id);
CREATE INDEX IF NOT EXISTS idx_holiday_sync_configs_provider
    ON holiday_sync_configs (provider_id);

-- Sync runs: last-sync state per (provider, country, year)
-- (tenant business data).
CREATE TABLE IF NOT EXISTS holiday_sync_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    company_id TEXT NOT NULL CHECK (char_length(company_id) BETWEEN 1 AND 128),
    config_id UUID NOT NULL,
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    country TEXT NOT NULL CHECK (char_length(country) = 2),
    year BIGINT NOT NULL CHECK (year BETWEEN 2000 AND 2100),
    status TEXT NOT NULL CHECK (status IN ('success', 'failed')),
    trigger TEXT NOT NULL CHECK (trigger IN ('manual', 'scheduled', 'sync_all')),
    holidays_found BIGINT NOT NULL DEFAULT 0 CHECK (holidays_found >= 0),
    holidays_stored BIGINT NOT NULL DEFAULT 0 CHECK (holidays_stored >= 0),
    error TEXT CHECK (error IS NULL OR char_length(error) <= 500),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_holiday_sync_runs_tenant
    ON holiday_sync_runs (tenant_id);
CREATE INDEX IF NOT EXISTS idx_holiday_sync_runs_config
    ON holiday_sync_runs (config_id);
CREATE INDEX IF NOT EXISTS idx_holiday_sync_runs_provider_country_year
    ON holiday_sync_runs (provider_id, country, year);

-- Synced holidays (shared reference data, keyed by country+year).
CREATE TABLE IF NOT EXISTS holidays (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    country TEXT NOT NULL CHECK (char_length(country) = 2),
    date DATE NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 300),
    local_name TEXT CHECK (local_name IS NULL OR char_length(local_name) BETWEEN 1 AND 300),
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    source TEXT NOT NULL DEFAULT 'synced' CHECK (source = 'synced'),
    global BOOLEAN NOT NULL DEFAULT TRUE,
    types JSONB NOT NULL DEFAULT '[]'::jsonb,
    counties JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_holidays_country_date_name UNIQUE (country, date, name)
);
CREATE INDEX IF NOT EXISTS idx_holidays_country_date
    ON holidays (country, date);
CREATE INDEX IF NOT EXISTS idx_holidays_provider
    ON holidays (provider_id);

-- Manual overrides (tenant business data). One override per
-- (tenant, country, date): a second override for the same date is
-- refused by the module (409) and by the unique index below.
CREATE TABLE IF NOT EXISTS holiday_overrides (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 128),
    company_id TEXT NOT NULL CHECK (char_length(company_id) BETWEEN 1 AND 128),
    country TEXT NOT NULL CHECK (char_length(country) = 2),
    date DATE NOT NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    override_kind TEXT NOT NULL CHECK (override_kind IN ('day_off', 'working_day')),
    note TEXT CHECK (note IS NULL OR char_length(note) <= 500),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_holiday_overrides_tenant_country_date
        UNIQUE (tenant_id, country, date)
);
CREATE INDEX IF NOT EXISTS idx_holiday_overrides_tenant
    ON holiday_overrides (tenant_id);
CREATE INDEX IF NOT EXISTS idx_holiday_overrides_country_date
    ON holiday_overrides (country, date);
