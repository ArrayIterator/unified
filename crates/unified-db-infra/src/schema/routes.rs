// Diesel schema for the routes domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Named routes (2026-11-22-000021_routes).
// ---------------------------------------------------------------------------

diesel::table! {
    named_routes (name) {
        name -> Varchar,
        default_path -> Varchar,
        current_path -> Varchar,
        source -> Varchar,
        description -> Text,
        config_override -> Nullable<Varchar>,
        settings_override -> Nullable<Varchar>,
        previous_path -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    route_redirects (route) {
        route -> Varchar,
        old_path -> Varchar,
        new_path -> Varchar,
        changed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
