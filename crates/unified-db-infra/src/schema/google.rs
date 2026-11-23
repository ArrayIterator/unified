// Diesel schema for the google domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    google_accounts (account_id) {
        account_id -> Varchar,
        email -> Varchar,
        name -> Nullable<Varchar>,
        access_token_encrypted -> Text,
        refresh_token_encrypted -> Text,
        expires_at -> Timestamptz,
        scopes -> Jsonb,
        linked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    google_drive_links (id) {
        id -> Uuid,
        name -> Varchar,
        account_id -> Varchar,
        email -> Varchar,
        folder_id -> Nullable<Varchar>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
