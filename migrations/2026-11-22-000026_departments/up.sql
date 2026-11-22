-- Departments tables for the Unified Pentagonal Project
-- (`src/modules/departments.rs`): support departments (CORE module),
-- their ticket routing rules, SLA policies, working hours, and
-- auto-replies.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `departments.working_hours` is the JSON form of the
--     `WorkingHours` struct (`days` 0 = Sunday through 6 = Saturday,
--     `start` / `end`, `timezone`, `utc_offset_minutes`).
--     `departments.auto_reply` is the JSON form of the `AutoReply`
--     struct (`enabled`, `subject`, `body`).
--   - `departments.agents` is the JSON array of agent user ids from
--     the `Department.agents` field. Names live in the Users module
--     and are resolved by callers, so no user foreign key is stored
--     here. The `AddAgent.role_id` is validated through the
--     `role.check` hook when supplied and is NOT persisted: it is a
--     validation-time check only, not a stored assignment.
--   - SLA defaults are the module's create defaults
--     (`create_department` in departments.rs): 240 minutes for first
--     response, 1440 minutes for resolution. Updates reject values
--     below 1, hence the CHECKs.
--   - The partial unique index on `is_default` is safe because the
--     module demotes every existing default department before
--     promoting a new one (create and update paths in
--     departments.rs); at most one active default can exist.
--   - `department_routing_rules.match_type` is `category`, `product`,
--     or `keyword` (departments.rs); rules run in `sort_order` order,
--     lowest first, ties broken by creation time.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One support department (CORE).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS departments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR NOT NULL,
    slug VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    email VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'inactive')),
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    sla_first_response_minutes INTEGER NOT NULL DEFAULT 240
        CHECK (sla_first_response_minutes >= 1),
    sla_resolution_minutes INTEGER NOT NULL DEFAULT 1440
        CHECK (sla_resolution_minutes >= 1),
    working_hours JSONB NOT NULL DEFAULT '{}'::jsonb,
    auto_reply JSONB NOT NULL DEFAULT '{}'::jsonb,
    agents JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_departments_slug
    ON departments (slug);
-- At most one default department: the fallback when no routing rule
-- matches. The module demotes the old default before promoting a
-- new one, so this index never rejects a legitimate change.
CREATE UNIQUE INDEX IF NOT EXISTS uq_departments_single_default
    ON departments (is_default) WHERE is_default;
CREATE INDEX IF NOT EXISTS idx_departments_status
    ON departments (status);

-- ---------------------------------------------------------------------------
-- One ticket routing rule: which department receives a ticket.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS department_routing_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    department_id UUID NOT NULL REFERENCES departments (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    match_type VARCHAR NOT NULL
        CHECK (match_type IN ('category', 'product', 'keyword')),
    match_value VARCHAR NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0 CHECK (sort_order >= 0),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_department_routing_rules_department_id
    ON department_routing_rules (department_id);
CREATE INDEX IF NOT EXISTS idx_department_routing_rules_department_sort
    ON department_routing_rules (department_id, sort_order);
