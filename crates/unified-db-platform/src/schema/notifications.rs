// Diesel schema for the notifications domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Search, notifications, and saved views (2026-11-22-000023_search_notifications_saved_views).
// ---------------------------------------------------------------------------

diesel::table! {
    search_documents (kind, id) {
        kind -> Varchar,
        id -> Varchar,
        title -> Varchar,
        snippet -> Text,
        url -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notifications (id) {
        id -> Uuid,
        user_id -> Varchar,
        title -> Varchar,
        body -> Text,
        kind -> Varchar,
        read -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_preferences (user_id) {
        user_id -> Varchar,
        channels -> Jsonb,
        categories -> Jsonb,
        channel_by_category -> Jsonb,
        digest -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_templates (id) {
        id -> Uuid,
        event -> Varchar,
        locale -> Varchar,
        title_template -> Text,
        body_template -> Text,
        channels -> Jsonb,
        email_connection_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
