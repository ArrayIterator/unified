// Diesel schema for the calendar domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Calendar (migration 000101_calendar).
// ---------------------------------------------------------------------------

diesel::table! {
    calendar_feeds (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Nullable<Text>,
        user_id -> Text,
        token_hash -> Text,
        modules -> Jsonb,
        label -> Nullable<Text>,
        expires_at -> Timestamptz,
        revoked -> Bool,
        created_at -> Timestamptz,
        last_used_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    calendar_settings (id) {
        id -> Uuid,
        tenant_id -> Text,
        scope -> Text,
        owner_id -> Text,
        weekend_days -> Jsonb,
        working_hours_start -> Text,
        working_hours_end -> Text,
        default_view -> Text,
        timezone -> Text,
        holiday_country -> Text,
        create_link_templates -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calendar_sources (id) {
        id -> Uuid,
        tenant_id -> Text,
        user_id -> Text,
        #[sql_name = "module"]
        module_ -> Text,
        visible -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
