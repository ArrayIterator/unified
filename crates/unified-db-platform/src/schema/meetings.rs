// Diesel schema for the meetings domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Meetings (2026-11-22-000027_meetings).

diesel::table! {
    meeting_rooms (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        host_id -> Varchar,
        status -> Varchar,
        access_model -> Varchar,
        product_id -> Nullable<Varchar>,
        max_participants -> Int4,
        waiting_room_enabled -> Bool,
        join_token -> Varchar,
        starts_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        recurrence_rule -> Nullable<Varchar>,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        started_at -> Nullable<Timestamptz>,
        ended_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_participants (room_id, user_id) {
        room_id -> Uuid,
        user_id -> Varchar,
        role -> Varchar,
        status -> Varchar,
        joined_at -> Nullable<Timestamptz>,
        left_at -> Nullable<Timestamptz>,
        last_seen -> Timestamptz,
        audio_muted -> Bool,
        video_muted -> Bool,
        sharing_screen -> Bool,
        hand_raised -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_messages (id) {
        id -> Uuid,
        room_id -> Uuid,
        author_id -> Varchar,
        body -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_recordings (id) {
        id -> Uuid,
        room_id -> Uuid,
        started_by -> Varchar,
        status -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Nullable<Varchar>,
        storage_url -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        duration_secs -> Nullable<Int8>,
        started_at -> Timestamptz,
        stopped_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
