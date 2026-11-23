// Diesel schema for the webinars domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Webinars (migration 000081_webinars).
// ---------------------------------------------------------------------------

diesel::table! {
    webinars (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        host_id -> Varchar,
        co_host_ids -> Array<Varchar>,
        panelist_ids -> Array<Varchar>,
        status -> Varchar,
        access_model -> Varchar,
        product_id -> Nullable<Varchar>,
        capacity -> Int4,
        requires_approval -> Bool,
        allow_waitlist -> Bool,
        chat_enabled -> Bool,
        qa_enabled -> Bool,
        polls_enabled -> Bool,
        recording_enabled -> Bool,
        meeting_room_id -> Nullable<Uuid>,
        meeting_link_status -> Varchar,
        chat_channel_id -> Nullable<Varchar>,
        survey_id -> Nullable<Varchar>,
        survey_distribution_id -> Nullable<Varchar>,
        starts_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        recurrence_rule -> Nullable<Varchar>,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        reminder_scheduler_status -> Varchar,
        started_at -> Nullable<Timestamptz>,
        ended_at -> Nullable<Timestamptz>,
        follow_up_sent_at -> Nullable<Timestamptz>,
        follow_up_count -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_registrations (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        user_id -> Varchar,
        status -> Varchar,
        payment_status -> Varchar,
        registered_at -> Timestamptz,
        approved_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        promoted_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_polls (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        question -> Varchar,
        options -> Array<Text>,
        allow_multiple -> Bool,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        closed_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_poll_votes (id) {
        id -> Uuid,
        poll_id -> Uuid,
        webinar_id -> Uuid,
        user_id -> Varchar,
        option_indexes -> Array<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_questions (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        author_id -> Varchar,
        body -> Text,
        upvoted_by -> Array<Varchar>,
        answered -> Bool,
        answered_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_attendance (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        user_id -> Varchar,
        joined_at -> Timestamptz,
        left_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webinar_recordings (id) {
        id -> Uuid,
        webinar_id -> Uuid,
        meeting_recording_id -> Nullable<Uuid>,
        title -> Nullable<Varchar>,
        status -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Nullable<Varchar>,
        storage_url -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        duration_secs -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
