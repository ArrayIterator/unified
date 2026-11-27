// Diesel schema for the timetracker domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// TimeTracker (migration 000032_timetracker; batch 2 notes sec. 4).

diesel::table! {
    timetracker_timers (id) {
        id -> Uuid,
        user_id -> Text,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        description -> Nullable<Text>,
        billable -> Bool,
        status -> Varchar,
        started_at -> Timestamptz,
        paused_total_secs -> Int8,
        paused_at -> Nullable<Timestamptz>,
        idle_total_secs -> Int8,
        stopped_at -> Nullable<Timestamptz>,
        entry_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_entries (id) {
        id -> Uuid,
        user_id -> Text,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        timer_id -> Nullable<Uuid>,
        description -> Text,
        started_at -> Timestamptz,
        ended_at -> Timestamptz,
        duration_secs -> Int8,
        billable -> Bool,
        approval -> Varchar,
        approved_by -> Nullable<Text>,
        approved_at -> Nullable<Timestamptz>,
        rejection_note -> Nullable<Text>,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_activity (id) {
        id -> Uuid,
        user_id -> Text,
        timer_id -> Nullable<Uuid>,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        activity_score -> Int4,
        idle_secs -> Int8,
        interval_start -> Timestamptz,
        interval_end -> Timestamptz,
        triggered_auto_pause -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_app_usage (id) {
        id -> Uuid,
        user_id -> Text,
        application_name -> Text,
        domain -> Nullable<Text>,
        window_title_hash -> Nullable<Text>,
        category -> Varchar,
        duration_secs -> Int8,
        interval_start -> Timestamptz,
        interval_end -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_screenshots (id) {
        id -> Uuid,
        user_id -> Text,
        timer_id -> Nullable<Uuid>,
        project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>,
        storage_target -> Text,
        storage_key -> Text,
        content_sha256 -> Text,
        content_type -> Varchar,
        size_bytes -> Int8,
        blurred -> Bool,
        captured_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_rates (id) {
        id -> Uuid,
        user_id -> Text,
        currency -> Varchar,
        rate -> Numeric,
        effective_from -> Timestamptz,
        effective_to -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_timesheets (id) {
        id -> Uuid,
        user_id -> Text,
        period_start -> Timestamptz,
        period_end -> Timestamptz,
        status -> Varchar,
        entry_ids -> Array<Uuid>,
        submitted_at -> Nullable<Timestamptz>,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        review_note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
