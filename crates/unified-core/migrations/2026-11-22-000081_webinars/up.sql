-- Webinar tables for the Unified Pentagonal Project
-- (`src/modules/webinars.rs`): webinars built on top of the meetings
-- video module, with registration (approval + waitlist), live polls,
-- Q&A, attendance sessions, recording references, and replay data.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `webinars.host_id`, `webinar_registrations.user_id`,
--     `webinar_attendance.user_id`, `webinar_polls.created_by`,
--     `webinar_questions.author_id` (and the `upvoted_by` array),
--     and `webinar_recordings` ownership are plain VARCHAR user ids:
--     the module identifies callers by the `X-User-Id` header and
--     keeps user ids as strings, exactly like `store_orders.user_id`
--     in migration 000002 and `meeting_rooms.host_id` in migration
--     000027. No foreign key to `users` is stored here.
--   - `webinars.product_id` is the store product id that grants
--     access, kept as a plain VARCHAR reference: entitlement is
--     checked through the `product.entitlement.check` hook, never by
--     a join from this module. A free webinar carries no product id.
--   - `webinars.status` is `draft`, `scheduled`, `live`, `ended`,
--     or `cancelled` (`WebinarStatus`); `access_model` is `free` or
--     `paid` (`WebinarAccessModel`). `capacity` defaults to 100 and
--     is validated against the configurable defaults in the module
--     (`min_capacity` 1, `max_capacity` 10000 as seeded). The
--     `ends_at` CHECK mirrors `validate_calendar`: an end must be
--     after its start. `meeting_link_status` is `unlinked`,
--     `linked`, `live`, or `ended`.
--   - `webinars.meeting_room_id` references the linked meetings
--     room (`meeting_rooms.id` from migration 000027) with
--     ON DELETE SET NULL: deleting a meetings room must never delete
--     the webinar that used it; the link simply drops.
--   - `webinar_registrations.status` is `registered`, `waitlisted`,
--     `pending_approval`, `cancelled`, or `denied`;
--     `payment_status` is `not_required`, `pending`, or `paid`.
--     The unique partial index on (webinar_id, user_id) allows one
--     active registration per user while keeping cancelled / denied
--     history rows (a user who cancels may register again).
--   - `webinar_polls.options` is a TEXT array; `webinar_poll_votes`
--     stores the chosen option indexes as an INTEGER array, one row
--     per (poll_id, user_id) - voting again replaces the row.
--   - `webinar_questions.upvoted_by` is a VARCHAR array of the
--     user ids that upvoted; the count is derived from it.
--   - `webinar_attendance` is one row per join/leave session:
--     `left_at` is NULL while the person is still in the session.
--     The same user may hold several sessions per webinar.
--   - `webinar_recordings` stores metadata only: the media bytes
--     live in the storage module and are referenced by
--     `storage_target` + `storage_key` (+ an optional URL).
--     `meeting_recording_id` references `meeting_recordings.id`
--     from migration 000027 with ON DELETE SET NULL. `status` is
--     `processing`, `ready`, or `failed`.
--   - Money does not appear in this migration: paid registration
--     only keeps the product reference and the payment status.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One webinar.
CREATE TABLE IF NOT EXISTS webinars (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    description TEXT,
    host_id VARCHAR NOT NULL,
    co_host_ids VARCHAR[] NOT NULL DEFAULT '{}',
    panelist_ids VARCHAR[] NOT NULL DEFAULT '{}',
    status VARCHAR NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'scheduled', 'live', 'ended', 'cancelled')),
    access_model VARCHAR NOT NULL DEFAULT 'free'
        CHECK (access_model IN ('free', 'paid')),
    product_id VARCHAR,
    capacity INTEGER NOT NULL DEFAULT 100
        CHECK (capacity >= 1 AND capacity <= 10000),
    requires_approval BOOLEAN NOT NULL DEFAULT FALSE,
    allow_waitlist BOOLEAN NOT NULL DEFAULT TRUE,
    chat_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    qa_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    polls_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    recording_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    meeting_room_id UUID REFERENCES meeting_rooms (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    meeting_link_status VARCHAR NOT NULL DEFAULT 'unlinked'
        CHECK (meeting_link_status IN ('unlinked', 'linked', 'live', 'ended')),
    chat_channel_id VARCHAR,
    survey_id VARCHAR,
    survey_distribution_id VARCHAR,
    starts_at TIMESTAMPTZ,
    ends_at TIMESTAMPTZ,
    timezone VARCHAR NOT NULL DEFAULT 'UTC',
    recurrence_rule VARCHAR,
    scheduler_job_id VARCHAR,
    scheduler_status VARCHAR NOT NULL DEFAULT 'not_scheduled'
        CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    reminder_scheduler_status VARCHAR NOT NULL DEFAULT 'not_scheduled'
        CHECK (reminder_scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    started_at TIMESTAMPTZ,
    ended_at TIMESTAMPTZ,
    follow_up_sent_at TIMESTAMPTZ,
    follow_up_count BIGINT NOT NULL DEFAULT 0
        CHECK (follow_up_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at IS NULL OR starts_at IS NULL OR ends_at > starts_at),
    CHECK (access_model = 'free' OR product_id IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_webinars_host_id
    ON webinars (host_id);
CREATE INDEX IF NOT EXISTS idx_webinars_status
    ON webinars (status);
CREATE INDEX IF NOT EXISTS idx_webinars_starts_at
    ON webinars (starts_at);
CREATE INDEX IF NOT EXISTS idx_webinars_meeting_room_id
    ON webinars (meeting_room_id);

-- One registration: seat, waitlist slot, or approval request.
CREATE TABLE IF NOT EXISTS webinar_registrations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webinar_id UUID NOT NULL REFERENCES webinars (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'registered'
        CHECK (status IN ('registered', 'waitlisted', 'pending_approval', 'cancelled', 'denied')),
    payment_status VARCHAR NOT NULL DEFAULT 'not_required'
        CHECK (payment_status IN ('not_required', 'pending', 'paid')),
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    approved_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    promoted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_webinar_registrations_active
    ON webinar_registrations (webinar_id, user_id)
    WHERE status IN ('registered', 'waitlisted', 'pending_approval');
CREATE INDEX IF NOT EXISTS idx_webinar_registrations_webinar_id
    ON webinar_registrations (webinar_id);
CREATE INDEX IF NOT EXISTS idx_webinar_registrations_user_id
    ON webinar_registrations (user_id);
CREATE INDEX IF NOT EXISTS idx_webinar_registrations_status
    ON webinar_registrations (webinar_id, status);

-- One live poll.
CREATE TABLE IF NOT EXISTS webinar_polls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webinar_id UUID NOT NULL REFERENCES webinars (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    question VARCHAR NOT NULL,
    options TEXT[] NOT NULL,
    allow_multiple BOOLEAN NOT NULL DEFAULT FALSE,
    status VARCHAR NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'closed')),
    created_by VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (cardinality(options) >= 2)
);

CREATE INDEX IF NOT EXISTS idx_webinar_polls_webinar_id
    ON webinar_polls (webinar_id);
CREATE INDEX IF NOT EXISTS idx_webinar_polls_status
    ON webinar_polls (webinar_id, status);

-- One vote: at most one row per (poll, user); voting again replaces
-- the choice. `option_indexes` holds the chosen option positions.
CREATE TABLE IF NOT EXISTS webinar_poll_votes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    poll_id UUID NOT NULL REFERENCES webinar_polls (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    webinar_id UUID NOT NULL REFERENCES webinars (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    option_indexes INTEGER[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (poll_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_webinar_poll_votes_poll_id
    ON webinar_poll_votes (poll_id);
CREATE INDEX IF NOT EXISTS idx_webinar_poll_votes_webinar_id
    ON webinar_poll_votes (webinar_id);

-- One Q&A question. `upvoted_by` lists the user ids that upvoted;
-- the count is derived from it, so a user can upvote only once.
CREATE TABLE IF NOT EXISTS webinar_questions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webinar_id UUID NOT NULL REFERENCES webinars (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    author_id VARCHAR NOT NULL,
    body TEXT NOT NULL,
    upvoted_by VARCHAR[] NOT NULL DEFAULT '{}',
    answered BOOLEAN NOT NULL DEFAULT FALSE,
    answered_by VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_webinar_questions_webinar_id
    ON webinar_questions (webinar_id);
CREATE INDEX IF NOT EXISTS idx_webinar_questions_answered
    ON webinar_questions (webinar_id, answered);

-- One attendance session: one join/leave pair. `left_at` is NULL
-- while the person is still in the session.
CREATE TABLE IF NOT EXISTS webinar_attendance (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webinar_id UUID NOT NULL REFERENCES webinars (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    left_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (left_at IS NULL OR left_at >= joined_at)
);

CREATE INDEX IF NOT EXISTS idx_webinar_attendance_webinar_id
    ON webinar_attendance (webinar_id);
CREATE INDEX IF NOT EXISTS idx_webinar_attendance_user_id
    ON webinar_attendance (webinar_id, user_id);
CREATE INDEX IF NOT EXISTS idx_webinar_attendance_open
    ON webinar_attendance (webinar_id)
    WHERE left_at IS NULL;

-- One recording reference (metadata only; bytes live in storage).
CREATE TABLE IF NOT EXISTS webinar_recordings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webinar_id UUID NOT NULL REFERENCES webinars (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    meeting_recording_id UUID REFERENCES meeting_recordings (id)
        ON DELETE SET NULL ON UPDATE NO ACTION,
    title VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'processing'
        CHECK (status IN ('processing', 'ready', 'failed')),
    storage_target VARCHAR,
    storage_key VARCHAR,
    storage_url VARCHAR,
    size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    duration_secs BIGINT CHECK (duration_secs IS NULL OR duration_secs >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_webinar_recordings_webinar_id
    ON webinar_recordings (webinar_id);
CREATE INDEX IF NOT EXISTS idx_webinar_recordings_status
    ON webinar_recordings (webinar_id, status);
