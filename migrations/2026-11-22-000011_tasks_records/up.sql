-- Tasks and system-records tables for the Unified Pentagonal
-- Project (tasks.rs, records.rs): tasks with comments and checklist
-- items, and the append-only queryable system-records store.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (in-memory stores are still authoritative;
-- see TODO.md):
--   - `tasks.status` workflow: todo -> in_progress -> review ->
--     done; todo / in_progress / review -> blocked -> todo |
--     in_progress; any non-terminal status -> cancelled. `done`
--     and `cancelled` are terminal. `overdue` is computed, never
--     stored.
--   - `tasks.priority` is `low`, `medium`, `high`, or `urgent`.
--   - `tasks.comments` and `tasks.checklist` from the `Task` struct
--     are normalized into `task_comments` and
--     `task_checklist_items` instead of embedded JSON, the same
--     normalization the store module uses for order items.
--   - `system_records` is append-only: no update or delete path
--     exists, only a retention purge. `updated_at` is added per the
--     project convention that every table carries both timestamps;
--     it normally equals `created_at`. `seq` is a monotonic
--     sequence number unique across the table.
--   - `system_records.actor` / `entity_type` / `entity_id` /
--     `summary` / `details` / `ip` follow the field conventions
--     the audit module uses (`actor_id` / `actor` / `user_id` for
--     the actor, `resource` as `type:id` when present).
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- A task: title, description, assignee, reporter, priority, status
-- workflow, due date, optional company linkage and project name,
-- tags, and time estimate / time spent.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    assignee_id UUID REFERENCES users (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    reporter_id UUID REFERENCES users (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    priority VARCHAR NOT NULL DEFAULT 'medium' CHECK (priority IN ('low', 'medium', 'high', 'urgent')),
    status VARCHAR NOT NULL DEFAULT 'todo' CHECK (status IN ('todo', 'in_progress', 'review', 'done', 'blocked', 'cancelled')),
    due_date TIMESTAMPTZ,
    company_id UUID REFERENCES companies (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    project_name VARCHAR,
    tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    estimate_minutes INTEGER CHECK (estimate_minutes >= 0),
    spent_minutes INTEGER NOT NULL DEFAULT 0 CHECK (spent_minutes >= 0),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tasks_assignee_id
    ON tasks (assignee_id);
CREATE INDEX IF NOT EXISTS idx_tasks_reporter_id
    ON tasks (reporter_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status
    ON tasks (status);
CREATE INDEX IF NOT EXISTS idx_tasks_priority
    ON tasks (priority);
CREATE INDEX IF NOT EXISTS idx_tasks_company_id
    ON tasks (company_id);
CREATE INDEX IF NOT EXISTS idx_tasks_due_date
    ON tasks (due_date);

-- ---------------------------------------------------------------------------
-- One comment on a task.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS task_comments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    author_id UUID REFERENCES users (id) ON DELETE SET NULL ON UPDATE NO ACTION,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_task_comments_task_id
    ON task_comments (task_id);

-- ---------------------------------------------------------------------------
-- One checklist item on a task.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS task_checklist_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks (id) ON DELETE CASCADE ON UPDATE NO ACTION,
    title VARCHAR NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_task_checklist_items_task_id
    ON task_checklist_items (task_id);

-- ---------------------------------------------------------------------------
-- One system record: who did what, when, on which entity. Records
-- are created only by listening to events on the hook bus; the
-- retention purge is the only delete path.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS system_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    seq BIGINT NOT NULL,
    actor VARCHAR NOT NULL,
    action VARCHAR NOT NULL,
    entity_type VARCHAR NOT NULL,
    entity_id VARCHAR NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    ip VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_system_records_seq
    ON system_records (seq);
CREATE INDEX IF NOT EXISTS idx_system_records_actor
    ON system_records (actor);
CREATE INDEX IF NOT EXISTS idx_system_records_action
    ON system_records (action);
CREATE INDEX IF NOT EXISTS idx_system_records_entity
    ON system_records (entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_system_records_created_at
    ON system_records (created_at);
