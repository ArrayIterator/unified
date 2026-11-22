-- TimeTracker module tables for the Unified Pentagonal Project
-- (`src/modules/timetracker.rs`): timers, manual time entries,
-- activity samples, app / website usage, screenshot metadata,
-- hourly rates, timesheets, global / per-user / per-buyer settings,
-- teams, team memberships, staff records, and staff invites.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `project_id` / `task_id` are foreign keys to the projects
--     module's tables (`projects`, `project_tasks`, created by
--     migration 000031, which runs before this one), ON DELETE SET
--     NULL so deleting a project or task unlinks its tracked time
--     instead of deleting the time record.
--   - `user_id` columns are TEXT with NO foreign key on purpose:
--     actor ids in this codebase are opaque strings (the
--     `X-User-Id` header), not necessarily the UUID primary key of
--     the `users` table. The users module stays the source of
--     truth for identity; staff records here only reference users
--     by id and never duplicate user rows.
--   - `company_id` columns DO reference `companies (id)` (created
--     by migration 000001), ON DELETE CASCADE: deleting a company
--     removes its teams, staff records, invites, and buyer
--     settings. Buyer settings are one row per company
--     (`UNIQUE (company_id)`).
--   - Money is NUMERIC(19,4): `timetracker_rates.rate` and
--     `timetracker_buyer_settings.default_hourly_rate`. Durations
--     are integer seconds (BIGINT), never floating hours: hours
--     are derived as `duration_secs / 3600.0` by the module.
--     `timetracker_activity.activity_score` is an integer 0-100.
--   - `timetracker_screenshots` stores metadata ONLY: `target`
--     and `key` identify the object in the storage module and
--     `content_sha256` is the SHA-256 hex of the uploaded bytes.
--     No screenshot bytes live in this schema.
--   - `timetracker_app_usage.window_title_hash` is the SHA-256 hex
--     of the window title. The raw title is never stored anywhere.
--   - `timetracker_staff_invites.token_hash` is the SHA-256 hex of
--     the invite token. The raw token is shown once at creation
--     and never stored.
--   - `timetracker_timesheets.entry_ids` is a UUID array with NO
--     foreign key on purpose: entries are approved through the
--     timesheet, and deleting an entry must not delete the sheet
--     that recorded it. `timetracker_timers.entry_id` references
--     `timetracker_entries (id)` ON DELETE SET NULL.
--   - `timetracker_settings` holds the global product defaults
--     (one row, `scope = 'global'`); per-buyer overrides live in
--     `timetracker_buyer_settings` and per-user consent in
--     `timetracker_user_settings`. The module resolves settings
--     staff override -> buyer setting -> global default.
--   - `timetracker_team_members` is keyed by
--     `UNIQUE (team_id, user_id)`: one membership per user per
--     team. `timetracker_staff_settings` is one row per user
--     (`UNIQUE (user_id)`): a user belongs to exactly one company
--     for tracking purposes.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One tracked block of time: created manually, or created by
-- stopping a timer. `duration_secs` always equals
-- `ended_at - started_at`. Created before `timetracker_timers`
-- because timers reference it (`entry_id`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    project_id UUID REFERENCES projects (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    task_id UUID REFERENCES project_tasks (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    timer_id UUID,
    description TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ NOT NULL,
    duration_secs BIGINT NOT NULL CHECK (duration_secs >= 0 AND duration_secs <= 86400),
    billable BOOLEAN NOT NULL DEFAULT FALSE,
    approval TEXT NOT NULL DEFAULT 'pending'
        CHECK (approval IN ('pending', 'approved', 'rejected')),
    approved_by TEXT,
    approved_at TIMESTAMPTZ,
    rejection_note TEXT,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ended_at > started_at)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_entries_user_id_started_at
    ON timetracker_entries (user_id, started_at);
CREATE INDEX IF NOT EXISTS idx_timetracker_entries_project_id
    ON timetracker_entries (project_id);
CREATE INDEX IF NOT EXISTS idx_timetracker_entries_task_id
    ON timetracker_entries (task_id);
CREATE INDEX IF NOT EXISTS idx_timetracker_entries_approval
    ON timetracker_entries (approval);
CREATE INDEX IF NOT EXISTS idx_timetracker_entries_timer_id
    ON timetracker_entries (timer_id);

-- ---------------------------------------------------------------------------
-- One timer: the live counterpart of a time entry. Stopping it
-- creates the entry (`entry_id`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_timers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    project_id UUID REFERENCES projects (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    task_id UUID REFERENCES project_tasks (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    description TEXT,
    billable BOOLEAN NOT NULL DEFAULT FALSE,
    status TEXT NOT NULL
        CHECK (status IN ('running', 'paused', 'stopped')),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    paused_total_secs BIGINT NOT NULL DEFAULT 0 CHECK (paused_total_secs >= 0),
    paused_at TIMESTAMPTZ,
    idle_total_secs BIGINT NOT NULL DEFAULT 0 CHECK (idle_total_secs >= 0),
    stopped_at TIMESTAMPTZ,
    entry_id UUID REFERENCES timetracker_entries (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_timetracker_timers_user_id_status
    ON timetracker_timers (user_id, status);
CREATE INDEX IF NOT EXISTS idx_timetracker_timers_project_id
    ON timetracker_timers (project_id);
CREATE INDEX IF NOT EXISTS idx_timetracker_timers_task_id
    ON timetracker_timers (task_id);

-- ---------------------------------------------------------------------------
-- One agent-reported activity sample: score 0-100 for one
-- interval, plus the idle seconds observed in that interval.
-- Idle time is recorded, never silently discarded.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_activity (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    timer_id UUID REFERENCES timetracker_timers (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    project_id UUID REFERENCES projects (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    task_id UUID REFERENCES project_tasks (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    activity_score INTEGER NOT NULL CHECK (activity_score BETWEEN 0 AND 100),
    idle_secs BIGINT NOT NULL DEFAULT 0 CHECK (idle_secs >= 0),
    interval_start TIMESTAMPTZ NOT NULL,
    interval_end TIMESTAMPTZ NOT NULL,
    triggered_auto_pause BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (interval_end > interval_start)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_activity_user_id_interval_start
    ON timetracker_activity (user_id, interval_start);
CREATE INDEX IF NOT EXISTS idx_timetracker_activity_timer_id
    ON timetracker_activity (timer_id);

-- ---------------------------------------------------------------------------
-- One agent-reported application / website usage interval. Only
-- the hash of the window title is stored, never the raw title.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_app_usage (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    application_name TEXT NOT NULL,
    domain TEXT,
    window_title_hash TEXT CHECK (window_title_hash IS NULL OR window_title_hash ~ '^[0-9a-f]{64}$'),
    category TEXT NOT NULL DEFAULT 'other'
        CHECK (category IN ('work', 'communication', 'development', 'browsing', 'social', 'entertainment', 'other')),
    duration_secs BIGINT NOT NULL CHECK (duration_secs >= 0 AND duration_secs <= 86400),
    interval_start TIMESTAMPTZ NOT NULL,
    interval_end TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (interval_end > interval_start)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_app_usage_user_id_interval_start
    ON timetracker_app_usage (user_id, interval_start);
CREATE INDEX IF NOT EXISTS idx_timetracker_app_usage_category
    ON timetracker_app_usage (category);

-- ---------------------------------------------------------------------------
-- One screenshot. Metadata only: the bytes live in the storage
-- module under (`storage_target`, `storage_key`).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_screenshots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    timer_id UUID REFERENCES timetracker_timers (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    project_id UUID REFERENCES projects (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    task_id UUID REFERENCES project_tasks (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    storage_target TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    content_type TEXT NOT NULL
        CHECK (content_type IN ('image/png', 'image/jpeg', 'image/webp')),
    size_bytes BIGINT NOT NULL CHECK (size_bytes > 0 AND size_bytes <= 5242880),
    blurred BOOLEAN NOT NULL DEFAULT FALSE,
    captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_timetracker_screenshots_user_id_captured_at
    ON timetracker_screenshots (user_id, captured_at);
CREATE INDEX IF NOT EXISTS idx_timetracker_screenshots_timer_id
    ON timetracker_screenshots (timer_id);

-- ---------------------------------------------------------------------------
-- One hourly rate for one user. Money is NUMERIC(19,4).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_rates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    rate NUMERIC(19, 4) NOT NULL CHECK (rate >= 0),
    effective_from TIMESTAMPTZ NOT NULL DEFAULT now(),
    effective_to TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_to > effective_from)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_rates_user_id_effective_from
    ON timetracker_rates (user_id, effective_from);

-- ---------------------------------------------------------------------------
-- One timesheet: a set of entries for one user over one period.
-- Approving a sheet approves its still-pending entries.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_timesheets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'submitted', 'approved', 'rejected')),
    entry_ids UUID[] NOT NULL DEFAULT '{}',
    submitted_at TIMESTAMPTZ,
    reviewed_by TEXT,
    reviewed_at TIMESTAMPTZ,
    review_note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (period_end > period_start)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_timesheets_user_id_period_start
    ON timetracker_timesheets (user_id, period_start);
CREATE INDEX IF NOT EXISTS idx_timetracker_timesheets_status
    ON timetracker_timesheets (status);

-- ---------------------------------------------------------------------------
-- Global product-default settings (one row, scope = 'global').
-- Per-buyer overrides live in timetracker_buyer_settings.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope TEXT NOT NULL DEFAULT 'global',
    idle_threshold_secs BIGINT NOT NULL DEFAULT 300 CHECK (idle_threshold_secs BETWEEN 30 AND 86400),
    retention_days BIGINT NOT NULL DEFAULT 90 CHECK (retention_days BETWEEN 1 AND 3650),
    overtime_hours_per_week NUMERIC(19, 4) NOT NULL DEFAULT 40 CHECK (overtime_hours_per_week >= 0 AND overtime_hours_per_week <= 168),
    overtime_multiplier NUMERIC(19, 4) NOT NULL DEFAULT 1.5 CHECK (overtime_multiplier >= 1 AND overtime_multiplier <= 4),
    low_activity_threshold BIGINT NOT NULL DEFAULT 30 CHECK (low_activity_threshold BETWEEN 0 AND 100),
    storage_target TEXT NOT NULL DEFAULT 'timetracker-screenshots',
    default_currency TEXT NOT NULL DEFAULT 'USD' CHECK (default_currency ~ '^[A-Z]{3}$'),
    auto_approve_timer_entries BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scope)
);

-- ---------------------------------------------------------------------------
-- One user's own tracking settings, including screenshot consent.
-- Screenshots stay disabled until the user opts in.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_user_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    screenshots_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    blur_screenshots BOOLEAN NOT NULL DEFAULT FALSE,
    idle_threshold_secs BIGINT CHECK (idle_threshold_secs IS NULL OR idle_threshold_secs BETWEEN 30 AND 86400),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_user_settings_user_id
    ON timetracker_user_settings (user_id);

-- ---------------------------------------------------------------------------
-- One team (group) inside one company.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_teams (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    name TEXT NOT NULL,
    description TEXT,
    manager_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_timetracker_teams_company_id
    ON timetracker_teams (company_id);

-- ---------------------------------------------------------------------------
-- One membership of a user in a team, with a tracker-scoped role.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_team_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES timetracker_teams (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member'
        CHECK (role IN ('admin', 'manager', 'member', 'viewer')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (team_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_team_members_team_id
    ON timetracker_team_members (team_id);
CREATE INDEX IF NOT EXISTS idx_timetracker_team_members_user_id
    ON timetracker_team_members (user_id);

-- ---------------------------------------------------------------------------
-- One staff record: a user attached to one company for tracking,
-- with their tracker-scoped role, direct manager, and per-staff
-- setting overrides. One row per user.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_staff_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL,
    company_id UUID NOT NULL REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    role TEXT NOT NULL DEFAULT 'member'
        CHECK (role IN ('admin', 'manager', 'member', 'viewer')),
    manager_id TEXT,
    screenshots_enabled BOOLEAN,
    blur_screenshots BOOLEAN,
    idle_threshold_secs BIGINT CHECK (idle_threshold_secs IS NULL OR idle_threshold_secs BETWEEN 30 AND 86400),
    activity_level TEXT CHECK (activity_level IS NULL OR activity_level IN ('off', 'minimal', 'full')),
    timezone TEXT,
    working_hours_start TEXT CHECK (working_hours_start IS NULL OR working_hours_start ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    working_hours_end TEXT CHECK (working_hours_end IS NULL OR working_hours_end ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_staff_settings_company_id
    ON timetracker_staff_settings (company_id);

-- ---------------------------------------------------------------------------
-- One invitation to join a company as tracker staff. Only the
-- SHA-256 hash of the token is stored; the raw token is shown
-- once at creation time.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_staff_invites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    email TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member'
        CHECK (role IN ('admin', 'manager', 'member', 'viewer')),
    team_id UUID REFERENCES timetracker_teams (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    invited_by TEXT NOT NULL,
    token_hash TEXT NOT NULL CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'expired', 'revoked')),
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    accepted_user_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_timetracker_staff_invites_company_id_status
    ON timetracker_staff_invites (company_id, status);
CREATE INDEX IF NOT EXISTS idx_timetracker_staff_invites_email
    ON timetracker_staff_invites (email);

-- ---------------------------------------------------------------------------
-- Per-buyer (per purchasing company) settings. Every column is an
-- override: NULL (or an empty list) falls back to the global
-- product default in timetracker_settings. One row per company.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS timetracker_buyer_settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    screenshots_enabled BOOLEAN,
    screenshot_interval_secs BIGINT CHECK (screenshot_interval_secs IS NULL OR screenshot_interval_secs BETWEEN 60 AND 3600),
    activity_level TEXT CHECK (activity_level IS NULL OR activity_level IN ('off', 'minimal', 'full')),
    idle_timeout_secs BIGINT CHECK (idle_timeout_secs IS NULL OR idle_timeout_secs BETWEEN 30 AND 86400),
    blur_screenshots BOOLEAN,
    allowed_apps TEXT[] NOT NULL DEFAULT '{}',
    blocked_apps TEXT[] NOT NULL DEFAULT '{}',
    allowed_websites TEXT[] NOT NULL DEFAULT '{}',
    blocked_websites TEXT[] NOT NULL DEFAULT '{}',
    default_hourly_rate NUMERIC(19, 4) CHECK (default_hourly_rate IS NULL OR default_hourly_rate >= 0),
    timezone TEXT,
    working_hours_start TEXT CHECK (working_hours_start IS NULL OR working_hours_start ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    working_hours_end TEXT CHECK (working_hours_end IS NULL OR working_hours_end ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    working_days BIGINT[] NOT NULL DEFAULT '{}',
    retention_days BIGINT CHECK (retention_days IS NULL OR retention_days BETWEEN 1 AND 3650),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (company_id)
);

CREATE INDEX IF NOT EXISTS idx_timetracker_buyer_settings_company_id
    ON timetracker_buyer_settings (company_id);
