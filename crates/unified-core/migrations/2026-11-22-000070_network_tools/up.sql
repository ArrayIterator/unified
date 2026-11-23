-- Network Tools module tables for the Unified Pentagonal Project
-- (`src/modules/network_tools.rs`): saved checks, tool run history,
-- uptime monitors and their events, and certificate expiry watches.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` columns are TEXT with NO foreign key on purpose:
--     actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity; network tool rows only reference users by
--     id and never duplicate user rows.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its saved checks, runs, monitors, events, and watches.
--   - JSON columns are JSONB: `network_tool_saved_checks.parameters`,
--     `network_tool_runs.summary`, `network_tool_uptime_events.detail`.
--     `network_tool_cert_watches.alerted_thresholds` is INTEGER[].
--   - No money columns here. All ids are Uuid, all timestamps are
--     Timestamptz. `network_tool_runs` intentionally has no foreign
--     key to `network_tool_saved_checks`: one-off runs have no saved
--     check, and a deleted saved check must not delete history.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One saved check: a named tool + target + parameters, owned by a user
-- (optionally scoped to a company), re-runnable through
-- `POST /api/v1/network-tools/saved-checks/{id}/run`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS network_tool_saved_checks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 128),
    tool TEXT NOT NULL CHECK (tool IN (
        'dns', 'ping', 'traceroute', 'port_check', 'ssl_check',
        'whois', 'http_check', 'ip_info', 'subnet', 'webhook_test',
        'uptime_check', 'blacklist', 'cert_check'
    )),
    target TEXT NOT NULL CHECK (char_length(target) BETWEEN 1 AND 2048),
    parameters JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_network_tool_saved_checks_user_id
    ON network_tool_saved_checks (user_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_saved_checks_company_id
    ON network_tool_saved_checks (company_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_saved_checks_tool
    ON network_tool_saved_checks (tool);

-- ---------------------------------------------------------------------------
-- One tool run: every execution of any tool, success or failure. The
-- `summary` holds outcome metadata only (duration, method, error
-- message) - never a response body.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS network_tool_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    target TEXT NOT NULL CHECK (char_length(target) BETWEEN 1 AND 512),
    duration_ms BIGINT NOT NULL DEFAULT 0,
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failed')),
    summary JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_network_tool_runs_user_id
    ON network_tool_runs (user_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_runs_company_id
    ON network_tool_runs (company_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_runs_tool
    ON network_tool_runs (tool);
CREATE INDEX IF NOT EXISTS idx_network_tool_runs_created_at
    ON network_tool_runs (created_at DESC);

-- ---------------------------------------------------------------------------
-- One uptime monitor: a URL probed on an interval with an expected
-- status and a failure threshold. `consecutive_failures` counts the
-- current failing streak; `last_status` is 'up', 'down', or NULL when
-- never probed.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS network_tool_uptime_monitors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 128),
    url TEXT NOT NULL CHECK (char_length(url) BETWEEN 1 AND 2048),
    interval_seconds INTEGER NOT NULL DEFAULT 300
        CHECK (interval_seconds BETWEEN 60 AND 86400),
    expected_status INTEGER NOT NULL DEFAULT 200
        CHECK (expected_status BETWEEN 100 AND 599),
    failure_threshold INTEGER NOT NULL DEFAULT 3
        CHECK (failure_threshold BETWEEN 1 AND 10),
    consecutive_failures INTEGER NOT NULL DEFAULT 0
        CHECK (consecutive_failures >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_status TEXT CHECK (last_status IN ('up', 'down')),
    last_checked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_network_tool_uptime_monitors_user_id
    ON network_tool_uptime_monitors (user_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_uptime_monitors_company_id
    ON network_tool_uptime_monitors (company_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_uptime_monitors_enabled
    ON network_tool_uptime_monitors (enabled);

-- ---------------------------------------------------------------------------
-- One uptime event: the outcome of a single probe of a monitor.
-- Deleted with the monitor (ON DELETE CASCADE).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS network_tool_uptime_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id UUID NOT NULL REFERENCES network_tool_uptime_monitors (id)
        ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('up', 'down')),
    http_status INTEGER CHECK (http_status BETWEEN 100 AND 599),
    latency_ms BIGINT CHECK (latency_ms >= 0),
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_network_tool_uptime_events_monitor_id
    ON network_tool_uptime_events (monitor_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_uptime_events_created_at
    ON network_tool_uptime_events (created_at DESC);

-- ---------------------------------------------------------------------------
-- One certificate watch: a domain whose certificate expiry is watched
-- at the 30/14/7/1-day thresholds. `alerted_thresholds` records which
-- thresholds already fired, so each threshold alerts exactly once
-- (-1 means "expired").
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS network_tool_cert_watches (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    company_id UUID REFERENCES companies (id) ON DELETE CASCADE,
    domain TEXT NOT NULL CHECK (char_length(domain) BETWEEN 1 AND 253),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_days_remaining INTEGER,
    last_checked_at TIMESTAMPTZ,
    alerted_thresholds INTEGER[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, domain)
);

CREATE INDEX IF NOT EXISTS idx_network_tool_cert_watches_user_id
    ON network_tool_cert_watches (user_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_cert_watches_company_id
    ON network_tool_cert_watches (company_id);
CREATE INDEX IF NOT EXISTS idx_network_tool_cert_watches_domain
    ON network_tool_cert_watches (domain);
