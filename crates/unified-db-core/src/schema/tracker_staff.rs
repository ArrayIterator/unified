// Diesel schema for the tracker_staff domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    timetracker_settings (id) {
        id -> Uuid,
        scope -> Text,
        idle_threshold_secs -> Int8,
        retention_days -> Int8,
        overtime_hours_per_week -> Numeric,
        overtime_multiplier -> Numeric,
        low_activity_threshold -> Int8,
        storage_target -> Text,
        default_currency -> Varchar,
        auto_approve_timer_entries -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_user_settings (id) {
        id -> Uuid,
        user_id -> Text,
        screenshots_enabled -> Bool,
        blur_screenshots -> Bool,
        idle_threshold_secs -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_teams (id) {
        id -> Uuid,
        company_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        manager_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_team_members (id) {
        id -> Uuid,
        team_id -> Uuid,
        user_id -> Text,
        role -> Varchar,
        joined_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_staff_settings (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Uuid,
        role -> Varchar,
        manager_id -> Nullable<Text>,
        screenshots_enabled -> Nullable<Bool>,
        blur_screenshots -> Nullable<Bool>,
        idle_threshold_secs -> Nullable<Int8>,
        activity_level -> Nullable<Varchar>,
        timezone -> Nullable<Text>,
        working_hours_start -> Nullable<Text>,
        working_hours_end -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_staff_invites (id) {
        id -> Uuid,
        company_id -> Uuid,
        email -> Text,
        role -> Varchar,
        team_id -> Nullable<Uuid>,
        invited_by -> Text,
        token_hash -> Text,
        status -> Varchar,
        expires_at -> Timestamptz,
        accepted_at -> Nullable<Timestamptz>,
        accepted_user_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timetracker_buyer_settings (id) {
        id -> Uuid,
        company_id -> Uuid,
        screenshots_enabled -> Nullable<Bool>,
        screenshot_interval_secs -> Nullable<Int8>,
        activity_level -> Nullable<Varchar>,
        idle_timeout_secs -> Nullable<Int8>,
        blur_screenshots -> Nullable<Bool>,
        allowed_apps -> Array<Text>,
        blocked_apps -> Array<Text>,
        allowed_websites -> Array<Text>,
        blocked_websites -> Array<Text>,
        default_hourly_rate -> Nullable<Numeric>,
        timezone -> Nullable<Text>,
        working_hours_start -> Nullable<Text>,
        working_hours_end -> Nullable<Text>,
        working_days -> Array<Int8>,
        retention_days -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
