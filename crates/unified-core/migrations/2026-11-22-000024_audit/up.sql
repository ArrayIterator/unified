-- Audit-log table for the Unified Pentagonal Project (audit.rs):
-- the durable form of the in-memory audit entries.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory audit store is still
-- authoritative; see TODO.md):
--   - `audit_entries` is append-only. `seq` is a monotonic
--     sequence number unique across the table. `resource` is
--     `type:id` when present. `metadata` is the structured event
--     payload. `updated_at` is added per the project convention
--     that every table carries both timestamps; it normally equals
--     `created_at`.
--   - The audited event list (`AUDITED_EVENTS` in audit.rs) is
--     code, not data; adding an event (for example
--     `route.changed`, or the api-manager access-control events)
--     is a code change, not a migration.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One audit entry: who did what, on which resource, with the event
-- payload and the actor IP.
CREATE TABLE IF NOT EXISTS audit_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    seq BIGINT NOT NULL,
    actor VARCHAR NOT NULL,
    action VARCHAR NOT NULL,
    resource VARCHAR NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    ip VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_audit_entries_seq
    ON audit_entries (seq);
CREATE INDEX IF NOT EXISTS idx_audit_entries_actor
    ON audit_entries (actor);
CREATE INDEX IF NOT EXISTS idx_audit_entries_action
    ON audit_entries (action);
CREATE INDEX IF NOT EXISTS idx_audit_entries_resource
    ON audit_entries (resource);
CREATE INDEX IF NOT EXISTS idx_audit_entries_created_at
    ON audit_entries (created_at);
