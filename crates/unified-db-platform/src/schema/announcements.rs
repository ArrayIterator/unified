// Diesel schema for the announcements domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    announcements (id) {
        id -> Uuid,
        title -> Varchar,
        body -> Text,
        audience -> Varchar,
        status -> Varchar,
        publish_at -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        pinned -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        published_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    changelog_entries (id) {
        id -> Uuid,
        version -> Varchar,
        date -> Varchar,
        category -> Varchar,
        title -> Varchar,
        body -> Text,
        link -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    announcement_read_receipts (announcement_id, user_id) {
        announcement_id -> Uuid,
        user_id -> Varchar,
        read_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
