-- Smart Inbox tables for the Unified Pentagonal Project
-- (`src/modules/inbox.rs`): one unified queue over email, chat,
-- tickets, and social mentions. The inbox stores only thread
-- references - the owning modules keep the message stores - plus
-- assignment history, canned replies, and per-agent read receipts.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/inbox.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `inbox_threads.source_id` is a plain VARCHAR reference, not
--     a foreign key: source modules do not share one id type
--     (tickets use string ids, chat uses UUIDs) and they own their
--     rows. `source_channel` is `email`, `chat`, `ticket`, or
--     `social`. The partial unique index on
--     (source_channel, source_id) covers live threads only, so a
--     merged thread never blocks re-registration of its source.
--   - `inbox_threads.customer_id`, `department_id`, and
--     `assignee_id` are plain VARCHAR user / department ids: the
--     module identifies callers by the `X-User-Id` header, exactly
--     like `store_orders.user_id` in migration 000002. No foreign
--     key to `users` is stored here.
--   - `inbox_threads.unread` is a coarse queue-level flag (set on
--     new activity, cleared when any agent marks the thread read);
--     the per-agent read state lives in `inbox_thread_reads` and
--     is authoritative for the `unread` value returned per caller.
--   - `inbox_threads.sla_due_at` / `sla_breached` are a snapshot
--     copied from the tickets module's own SLA fields; the tickets
--     module owns SLA computation. They are only meaningful for
--     `ticket` threads and are NULL / false otherwise.
--   - `inbox_threads.merged_into` points at the live target thread
--     of an explicit merge (ON DELETE SET NULL); merged threads are
--     hidden from the queue by default. `status` is `open`,
--     `pending`, `resolved`, or `closed`; `priority` is `low`,
--     `normal`, `high`, or `urgent`.
--   - `inbox_assignments` is the assignment history: only one row
--     per thread is `active` at a time (the module deactivates the
--     previous row on re-assignment); the current assignee is
--     mirrored on `inbox_threads.assignee_id`.
--   - `inbox_canned_replies.department_id` is NULL for replies
--     available to every department. The body supports
--     `{customer}`, `{subject}`, and `{thread_id}` placeholders,
--     expanded when a reply is requested.
--   - `inbox_thread_reads` is keyed by (thread_id, user_id): one
--     receipt per agent per thread, updated when the agent marks
--     the thread read.

CREATE TABLE IF NOT EXISTS inbox_threads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_channel VARCHAR(20) NOT NULL CHECK (source_channel IN ('email', 'chat', 'ticket', 'social')),
    source_id VARCHAR(255) NOT NULL,
    subject VARCHAR(500) NOT NULL,
    customer_id VARCHAR(255),
    customer_label VARCHAR(255),
    participants JSONB NOT NULL DEFAULT '[]'::jsonb,
    department_id VARCHAR(255),
    assignee_id VARCHAR(255),
    status VARCHAR(20) NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'pending', 'resolved', 'closed')),
    priority VARCHAR(20) NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
    unread BOOLEAN NOT NULL DEFAULT TRUE,
    snoozed_until TIMESTAMPTZ,
    snooze_scheduler_status VARCHAR(20) NOT NULL DEFAULT 'not_scheduled' CHECK (snooze_scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    sla_due_at TIMESTAMPTZ,
    sla_breached BOOLEAN NOT NULL DEFAULT FALSE,
    merged_into UUID REFERENCES inbox_threads (id) ON DELETE SET NULL,
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS inbox_threads_source_live_idx
    ON inbox_threads (source_channel, source_id) WHERE merged_into IS NULL;
CREATE INDEX IF NOT EXISTS inbox_threads_status_idx ON inbox_threads (status);
CREATE INDEX IF NOT EXISTS inbox_threads_assignee_idx ON inbox_threads (assignee_id);
CREATE INDEX IF NOT EXISTS inbox_threads_department_idx ON inbox_threads (department_id);
CREATE INDEX IF NOT EXISTS inbox_threads_channel_idx ON inbox_threads (source_channel);
CREATE INDEX IF NOT EXISTS inbox_threads_sla_due_idx ON inbox_threads (sla_due_at) WHERE sla_due_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS inbox_threads_last_activity_idx ON inbox_threads (last_activity_at DESC);
CREATE INDEX IF NOT EXISTS inbox_threads_merged_into_idx ON inbox_threads (merged_into) WHERE merged_into IS NOT NULL;

CREATE TABLE IF NOT EXISTS inbox_assignments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    thread_id UUID NOT NULL REFERENCES inbox_threads (id) ON DELETE CASCADE,
    assignee_id VARCHAR(255) NOT NULL,
    assigned_by VARCHAR(255) NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS inbox_assignments_thread_idx ON inbox_assignments (thread_id);
CREATE INDEX IF NOT EXISTS inbox_assignments_assignee_idx ON inbox_assignments (assignee_id);
CREATE INDEX IF NOT EXISTS inbox_assignments_active_idx ON inbox_assignments (thread_id) WHERE active;

CREATE TABLE IF NOT EXISTS inbox_canned_replies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    department_id VARCHAR(255),
    title VARCHAR(200) NOT NULL,
    body TEXT NOT NULL,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS inbox_canned_replies_department_idx ON inbox_canned_replies (department_id);

CREATE TABLE IF NOT EXISTS inbox_thread_reads (
    thread_id UUID NOT NULL REFERENCES inbox_threads (id) ON DELETE CASCADE,
    user_id VARCHAR(255) NOT NULL,
    read_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (thread_id, user_id)
);

CREATE INDEX IF NOT EXISTS inbox_thread_reads_user_idx ON inbox_thread_reads (user_id);
