-- Todos tables (`src/modules/todos.rs`): personal, per-user todos — lists
-- (including a default Inbox), todos with due dates, priorities,
-- tags, one-level subtasks, storage-key attachments (metadata only),
-- list sharing (read/edit, ownership never transfers), recurring
-- todos, a completion log, and due-date reminders.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE POSTGRESQL
-- DATABASE. It is a persistence target for the in-memory DashMap
-- stores in `src/modules/todos.rs`, which stay authoritative until
-- a database-backed implementation is wired in (see TODO.md).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `todo_lists.owner_id`, `todos.owner_id`,
--     `todo_shares.owner_id` / `shared_with_user_id`,
--     `todo_completion_log.user_id`, and
--     `todo_reminders.owner_id` are plain VARCHAR user ids: the
--     module identifies callers by the `X-User-Id` header and keeps
--     user ids as strings, exactly like `store_orders.user_id` in
--     migration 000002 and `meeting_rooms.host_id` in migration
--     000027. No foreign key to `users` is stored here.
--   - `todos.priority` is `low`, `normal`, `high`, or `urgent`
--     (`TodoPriority`; note there is no `medium` here, unlike the
--     tasks module). `todos.status` is `open` or `done`
--     (`TodoStatus`): there is no in-progress / review / blocked
--     workflow — that is the tasks module's job.
--   - `todos.attachments` is JSONB: an array of
--     `TodoAttachment` records (`id`, `storage_target`,
--     `storage_key`, `file_name`, `size_bytes`, `created_at`).
--     Only metadata is stored: the bytes live in the storage
--     module. There is no separate attachments table.
--   - `todos.recurrence_frequency` is `daily`, `weekly`, or
--     `monthly` (`RecurrenceFrequency`); `recurrence_interval`
--     defaults to 1 and is validated to 1..365 in the module;
--     `recurrence_count` and `recurrence_until` are mutually
--     exclusive in the module and a recurring todo always has a
--     due date (both enforced by the CHECKs below as well as in
--     code). `recurrence_remaining` counts how many further
--     occurrences may still be created; `NULL` means "repeat
--     until `recurrence_until`". `recurrence_group_id` is shared
--     by every occurrence of one series.
--   - `todo_shares` is keyed by (list_id, shared_with_user_id),
--     the same key the in-memory store uses
--     (`"{list_id}:{user_id}"`). `permission` is `read` or `edit`.
--     Ownership stays with `todo_lists.owner_id`: sharing never
--     transfers it, and the CHECK below forbids sharing a list
--     with its own owner.
--   - `todo_completion_log` is append-only: one row per
--     completion or reopen (`action` is `completed` or
--     `reopened`), with the acting user id. Streaks are computed
--     from this table, never stored as a claim.
--   - `todo_reminders.due_at` is a denormalized copy of the
--     todo's due time at reminder creation, so "a reminder always
--     carries a due date" is enforceable with a plain `NOT NULL`
--     plus `remind_at <= due_at` CHECK. `offset_minutes` is
--     0..43200 (0 = at the due time, 43200 = 30 days before).
--     `status` is `pending`, `sent`, or `cancelled`;
--     `scheduler_status` is `not_scheduled`, `queued`, or
--     `declined` (the `todos.reminders.send` scheduler kind does
--     not exist yet — see the module header).
--   - `todo_lists`, `todos`, `todo_subtasks`,
--     `todo_completion_log`, and `todo_reminders` carry
--     `updated_at` although some of the structs are append-only or
--     rarely updated: the project convention is created_at +
--     updated_at on every table, and updated_at normally equals
--     created_at for an immutable row.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One todo list. Exactly one owner; sharing lives in `todo_shares`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS todo_lists (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    color VARCHAR NOT NULL DEFAULT 'blue'
        CHECK (color IN ('red', 'orange', 'amber', 'green', 'teal', 'blue', 'indigo', 'purple', 'pink', 'gray', 'brown')),
    icon VARCHAR NOT NULL DEFAULT 'folder'
        CHECK (icon IN ('inbox', 'star', 'briefcase', 'home', 'shopping', 'health', 'study', 'travel', 'finance', 'calendar', 'flag', 'check', 'folder', 'tag', 'bell', 'gift', 'music', 'sport', 'book', 'heart')),
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_todo_lists_owner_id
    ON todo_lists (owner_id);
CREATE INDEX IF NOT EXISTS idx_todo_lists_archived
    ON todo_lists (archived);
CREATE INDEX IF NOT EXISTS idx_todo_lists_created_at
    ON todo_lists (created_at);

-- ---------------------------------------------------------------------------
-- One todo. Exactly one owner; a shared-list viewer never becomes
-- an owner. `overdue` is computed, never stored.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS todos (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id UUID NOT NULL REFERENCES todo_lists (id) ON DELETE CASCADE,
    owner_id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    notes TEXT,
    due_at TIMESTAMPTZ,
    timezone VARCHAR NOT NULL DEFAULT 'UTC',
    priority VARCHAR NOT NULL DEFAULT 'normal'
        CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
    status VARCHAR NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'done')),
    tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    subtasks JSONB NOT NULL DEFAULT '[]'::jsonb,
    attachments JSONB NOT NULL DEFAULT '[]'::jsonb,
    recurrence_frequency VARCHAR
        CHECK (recurrence_frequency IN ('daily', 'weekly', 'monthly')),
    recurrence_interval INTEGER NOT NULL DEFAULT 1
        CHECK (recurrence_interval >= 1 AND recurrence_interval <= 365),
    recurrence_count INTEGER
        CHECK (recurrence_count IS NULL OR (recurrence_count >= 1 AND recurrence_count <= 366)),
    recurrence_until TIMESTAMPTZ,
    recurrence_remaining INTEGER
        CHECK (recurrence_remaining IS NULL OR recurrence_remaining >= 0),
    recurrence_group_id UUID,
    position INTEGER NOT NULL DEFAULT 0
        CHECK (position >= 0),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A recurring todo always has a due date, and count/until are
    -- mutually exclusive; both rules are also enforced in the module.
    CHECK (recurrence_frequency IS NULL OR due_at IS NOT NULL),
    CHECK (recurrence_count IS NULL OR recurrence_until IS NULL),
    CHECK (recurrence_until IS NULL OR due_at IS NULL OR recurrence_until > due_at),
    CHECK (recurrence_remaining IS NULL OR recurrence_frequency IS NOT NULL),
    CHECK (recurrence_group_id IS NULL OR recurrence_frequency IS NOT NULL),
    CHECK (status <> 'done' OR completed_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_todos_owner_id
    ON todos (owner_id);
CREATE INDEX IF NOT EXISTS idx_todos_list_id
    ON todos (list_id);
CREATE INDEX IF NOT EXISTS idx_todos_due_at
    ON todos (due_at);
CREATE INDEX IF NOT EXISTS idx_todos_status
    ON todos (status);
CREATE INDEX IF NOT EXISTS idx_todos_priority
    ON todos (priority);
CREATE INDEX IF NOT EXISTS idx_todos_completed_at
    ON todos (completed_at);
CREATE INDEX IF NOT EXISTS idx_todos_recurrence_group_id
    ON todos (recurrence_group_id);

-- ---------------------------------------------------------------------------
-- One checkable subtask. Exactly one level: a subtask has no
-- children of its own. (The in-memory store embeds subtasks in the
-- todo; this table is the normalized persistence target.)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS todo_subtasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    todo_id UUID NOT NULL REFERENCES todos (id) ON DELETE CASCADE,
    title VARCHAR NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE,
    position INTEGER NOT NULL DEFAULT 0
        CHECK (position >= 0),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (done = FALSE OR completed_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_todo_subtasks_todo_id
    ON todo_subtasks (todo_id);

-- ---------------------------------------------------------------------------
-- One share grant: `shared_with_user_id` may read (or read + edit)
-- the list. Ownership stays with `todo_lists.owner_id`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS todo_shares (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id UUID NOT NULL REFERENCES todo_lists (id) ON DELETE CASCADE,
    owner_id VARCHAR NOT NULL,
    shared_with_user_id VARCHAR NOT NULL,
    permission VARCHAR NOT NULL DEFAULT 'read'
        CHECK (permission IN ('read', 'edit')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (shared_with_user_id <> owner_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_todo_shares_list_user
    ON todo_shares (list_id, shared_with_user_id);
CREATE INDEX IF NOT EXISTS idx_todo_shares_shared_with_user_id
    ON todo_shares (shared_with_user_id);

-- ---------------------------------------------------------------------------
-- Completion log (append-only): who completed or reopened a todo,
-- and when. Streaks are computed from this table.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS todo_completion_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    todo_id UUID NOT NULL REFERENCES todos (id) ON DELETE CASCADE,
    user_id VARCHAR NOT NULL,
    action VARCHAR NOT NULL
        CHECK (action IN ('completed', 'reopened')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_todo_completion_log_todo_id
    ON todo_completion_log (todo_id);
CREATE INDEX IF NOT EXISTS idx_todo_completion_log_user_id
    ON todo_completion_log (user_id);
CREATE INDEX IF NOT EXISTS idx_todo_completion_log_created_at
    ON todo_completion_log (created_at);

-- ---------------------------------------------------------------------------
-- One due-date reminder. `due_at` is denormalized (see the header):
-- a reminder always carries a due date.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS todo_reminders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    todo_id UUID NOT NULL REFERENCES todos (id) ON DELETE CASCADE,
    owner_id VARCHAR NOT NULL,
    offset_minutes BIGINT NOT NULL
        CHECK (offset_minutes >= 0 AND offset_minutes <= 43200),
    remind_at TIMESTAMPTZ NOT NULL,
    due_at TIMESTAMPTZ NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sent', 'cancelled')),
    scheduler_job_id VARCHAR,
    scheduler_status VARCHAR NOT NULL DEFAULT 'not_scheduled'
        CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (remind_at <= due_at),
    CHECK (status <> 'sent' OR sent_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_todo_reminders_todo_id
    ON todo_reminders (todo_id);
CREATE INDEX IF NOT EXISTS idx_todo_reminders_owner_id
    ON todo_reminders (owner_id);
CREATE INDEX IF NOT EXISTS idx_todo_reminders_remind_at
    ON todo_reminders (remind_at);
CREATE INDEX IF NOT EXISTS idx_todo_reminders_status
    ON todo_reminders (status);
