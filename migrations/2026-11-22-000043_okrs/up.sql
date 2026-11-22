-- OKR / Goals tables for the Unified Pentagonal Project
-- (`src/modules/okrs.rs`): cycles / quarters, objectives, key
-- results, check-ins, and project / task links. Matches
-- src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/okrs.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `okr_cycles.company_id`, `okr_objectives.owner_id`, and
--     `okr_checkins.author_id` are plain VARCHAR references (the
--     module identifies callers by the `X-User-Id` header and keeps
--     user ids as strings, exactly like `store_orders.user_id` in
--     migration 000002). No foreign key to `users` is stored here.
--   - `okr_cycles.status` is draft, active, or closed. The partial
--     unique index below enforces one active cycle per company,
--     matching the write-time check in the module.
--   - `okr_objectives.level` is company, team, or personal.
--     `parent_objective_id` is a self reference (SET NULL on
--     delete: deleting a parent keeps its children, matching the
--     module). `final_score` is set only when the cycle closes
--     (mean key-result progress, 0.0–1.0).
--   - `okr_key_results` values are NUMERIC(19,4) (money-grade
--     precision, reused for generic measured values). Progress is
--     computed, never stored: (current - start) / (target - start).
--     The module rejects a target equal to the start value.
--   - `okr_checkins.confidence` is low, medium, or high. `kind`
--     is note (ordinary check-in) or override (manual value change,
--     which always carries a `reason`). `value_snapshot` is the key
--     result's current value at check-in time.
--   - `okr_links.link_type` is project or task and `target_id` is
--     the linked project / task UUID (plain UUID reference, no FK:
--     the projects / tasks modules own those rows). Task links
--     drive the progress roll-up; project links are alignment
--     only. The unique index prevents the same target being linked
--     twice to one key result.

CREATE TABLE IF NOT EXISTS okr_cycles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR NOT NULL,
    name VARCHAR(200) NOT NULL,
    description TEXT,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'active', 'closed'
    )),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS idx_okr_cycles_company_id ON okr_cycles (company_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_okr_cycles_one_active_per_company
    ON okr_cycles (company_id) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS okr_objectives (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cycle_id UUID NOT NULL REFERENCES okr_cycles (id) ON DELETE CASCADE,
    company_id VARCHAR NOT NULL,
    parent_objective_id UUID REFERENCES okr_objectives (id) ON DELETE SET NULL,
    title VARCHAR(300) NOT NULL,
    description TEXT,
    level VARCHAR(16) NOT NULL CHECK (level IN (
        'company', 'team', 'personal'
    )),
    owner_id VARCHAR NOT NULL,
    final_score NUMERIC(5,4) CHECK (final_score >= 0 AND final_score <= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (parent_objective_id IS NULL OR parent_objective_id <> id)
);

CREATE INDEX IF NOT EXISTS idx_okr_objectives_cycle_id ON okr_objectives (cycle_id);
CREATE INDEX IF NOT EXISTS idx_okr_objectives_company_id ON okr_objectives (company_id);
CREATE INDEX IF NOT EXISTS idx_okr_objectives_parent_id ON okr_objectives (parent_objective_id);
CREATE INDEX IF NOT EXISTS idx_okr_objectives_owner_id ON okr_objectives (owner_id);

CREATE TABLE IF NOT EXISTS okr_key_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    objective_id UUID NOT NULL REFERENCES okr_objectives (id) ON DELETE CASCADE,
    title VARCHAR(300) NOT NULL,
    unit VARCHAR(32) NOT NULL,
    target_value NUMERIC(19,4) NOT NULL,
    start_value NUMERIC(19,4) NOT NULL DEFAULT 0,
    current_value NUMERIC(19,4) NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (target_value <> start_value)
);

CREATE INDEX IF NOT EXISTS idx_okr_key_results_objective_id ON okr_key_results (objective_id);

CREATE TABLE IF NOT EXISTS okr_checkins (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    key_result_id UUID NOT NULL REFERENCES okr_key_results (id) ON DELETE CASCADE,
    author_id VARCHAR NOT NULL,
    note TEXT NOT NULL,
    confidence VARCHAR(16) NOT NULL CHECK (confidence IN (
        'low', 'medium', 'high'
    )),
    kind VARCHAR(16) NOT NULL DEFAULT 'note' CHECK (kind IN (
        'note', 'override'
    )),
    reason TEXT,
    value_snapshot NUMERIC(19,4),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'override' OR reason IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_okr_checkins_key_result_id ON okr_checkins (key_result_id);
CREATE INDEX IF NOT EXISTS idx_okr_checkins_created_at ON okr_checkins (created_at);

CREATE TABLE IF NOT EXISTS okr_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    key_result_id UUID NOT NULL REFERENCES okr_key_results (id) ON DELETE CASCADE,
    link_type VARCHAR(16) NOT NULL CHECK (link_type IN (
        'project', 'task'
    )),
    target_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_okr_links_key_result_id ON okr_links (key_result_id);
CREATE INDEX IF NOT EXISTS idx_okr_links_target_id ON okr_links (target_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_okr_links_unique_target
    ON okr_links (key_result_id, link_type, target_id);
