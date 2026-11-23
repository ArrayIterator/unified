// Diesel schema for the saved_views domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    saved_views (id) {
        id -> Uuid,
        user_id -> Varchar,
        module_id -> Varchar,
        name -> Varchar,
        filter -> Jsonb,
        sort -> Nullable<Varchar>,
        order -> Nullable<Varchar>,
        visibility -> Varchar,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    timeline_entries (id) {
        id -> Uuid,
        seq -> Int8,
        actor -> Varchar,
        action -> Varchar,
        entity_type -> Varchar,
        entity_id -> Varchar,
        summary -> Text,
        source -> Varchar,
        details -> Jsonb,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
