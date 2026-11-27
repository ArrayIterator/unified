-- Meetings tables for the Unified Pentagonal Project
-- (`src/modules/meetings.rs`): Zoom-like video meeting rooms over
-- WebRTC, participants (live list and attendance history), in-meeting
-- chat messages, and recording bookkeeping.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `meeting_rooms.host_id` and `meeting_participants.user_id`
--     (and `meeting_messages.author_id`,
--     `meeting_recordings.started_by`) are plain VARCHAR user ids:
--     the module identifies callers by the `X-User-Id` header and
--     keeps user ids as strings, exactly like `store_orders.user_id`
--     in migration 000002. No foreign key to `users` is stored here.
--   - `meeting_rooms.product_id` is the store product id that grants
--     access, kept as a plain VARCHAR reference: entitlement is
--     checked through the `product.entitlement.check` hook, never by
--     a join from this module. A free room carries no product id.
--   - `meeting_rooms.status` is `scheduled`, `live`, `ended`, or
--     `cancelled` (`RoomStatus`); `access_model` is `free`, `room`,
--     `seat`, or `minutes` (`AccessModel`). `max_participants`
--     defaults to 100 and is validated to 2..500 in the module;
--     `timezone` defaults to `UTC`; `scheduler_status` is
--     `not_scheduled`, `queued`, or `declined`. The `ends_at` CHECK
--     mirrors `validate_calendar`: an end must be after its start.
--   - `meeting_participants` is keyed by (room_id, user_id), the
--     same key the in-memory store uses (`"{room_id}:{user_id}"`).
--     One row is both the live participant list and the attendance
--     history: `left_at` is NULL while the person is still in the
--     room. `role` is `host`, `co_host`, or `participant`
--     (`MeetingRole`, snake_case); `status` is `waiting`, `joined`,
--     `left`, `denied`, or `removed`.
--   - `meeting_messages` carries `updated_at` although the
--     `MeetingMessage` struct has only `created_at`: the project
--     convention is created_at + updated_at on every table, and
--     updated_at normally equals created_at for an immutable chat
--     message.
--   - `meeting_recordings` stores metadata only: the media bytes
--     live in the storage module and are referenced by
--     `storage_target` + `storage_key` (+ an optional signed URL
--     minted at attach time). `status` is `recording`, `stopped`,
--     or `stored`. No second storage system is built here.
--   - Money does not appear in this migration.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- One meeting room.
CREATE TABLE IF NOT EXISTS meeting_rooms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR NOT NULL,
    description TEXT,
    host_id VARCHAR NOT NULL,
    status VARCHAR NOT NULL
        CHECK (status IN ('scheduled', 'live', 'ended', 'cancelled')),
    access_model VARCHAR NOT NULL DEFAULT 'free'
        CHECK (access_model IN ('free', 'room', 'seat', 'minutes')),
    product_id VARCHAR,
    max_participants INTEGER NOT NULL DEFAULT 100
        CHECK (max_participants >= 2 AND max_participants <= 500),
    waiting_room_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    join_token VARCHAR NOT NULL,
    starts_at TIMESTAMPTZ,
    ends_at TIMESTAMPTZ,
    timezone VARCHAR NOT NULL DEFAULT 'UTC',
    recurrence_rule VARCHAR,
    scheduler_job_id VARCHAR,
    scheduler_status VARCHAR NOT NULL DEFAULT 'not_scheduled'
        CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    started_at TIMESTAMPTZ,
    ended_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at IS NULL OR starts_at IS NULL OR ends_at > starts_at)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_meeting_rooms_join_token
    ON meeting_rooms (join_token);
CREATE INDEX IF NOT EXISTS idx_meeting_rooms_host_id
    ON meeting_rooms (host_id);
CREATE INDEX IF NOT EXISTS idx_meeting_rooms_status
    ON meeting_rooms (status);
CREATE INDEX IF NOT EXISTS idx_meeting_rooms_starts_at
    ON meeting_rooms (starts_at);

-- One person's stay in one room: live participant list and
-- attendance history in the same row.
CREATE TABLE IF NOT EXISTS meeting_participants (
    room_id UUID NOT NULL REFERENCES meeting_rooms (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    user_id VARCHAR NOT NULL,
    role VARCHAR NOT NULL DEFAULT 'participant'
        CHECK (role IN ('host', 'co_host', 'participant')),
    status VARCHAR NOT NULL DEFAULT 'waiting'
        CHECK (status IN ('waiting', 'joined', 'left', 'denied', 'removed')),
    joined_at TIMESTAMPTZ,
    left_at TIMESTAMPTZ,
    last_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    audio_muted BOOLEAN NOT NULL DEFAULT FALSE,
    video_muted BOOLEAN NOT NULL DEFAULT FALSE,
    sharing_screen BOOLEAN NOT NULL DEFAULT FALSE,
    hand_raised BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_meeting_participants_user_id
    ON meeting_participants (user_id);
CREATE INDEX IF NOT EXISTS idx_meeting_participants_status
    ON meeting_participants (room_id, status);

-- One in-meeting chat message.
CREATE TABLE IF NOT EXISTS meeting_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    room_id UUID NOT NULL REFERENCES meeting_rooms (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    author_id VARCHAR NOT NULL,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_meeting_messages_room_id
    ON meeting_messages (room_id);
CREATE INDEX IF NOT EXISTS idx_meeting_messages_created_at
    ON meeting_messages (room_id, created_at);

-- One recording (metadata only; bytes live in the storage module).
CREATE TABLE IF NOT EXISTS meeting_recordings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    room_id UUID NOT NULL REFERENCES meeting_rooms (id)
        ON DELETE CASCADE ON UPDATE NO ACTION,
    started_by VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'recording'
        CHECK (status IN ('recording', 'stopped', 'stored')),
    storage_target VARCHAR,
    storage_key VARCHAR,
    storage_url VARCHAR,
    size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    duration_secs BIGINT CHECK (duration_secs IS NULL OR duration_secs >= 0),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    stopped_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_meeting_recordings_room_id
    ON meeting_recordings (room_id);
CREATE INDEX IF NOT EXISTS idx_meeting_recordings_status
    ON meeting_recordings (status);
