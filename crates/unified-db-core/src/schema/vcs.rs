// Diesel schema for the vcs domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    vcs_connections (id) {
        id -> Uuid,
        provider -> Varchar,
        owner -> Varchar,
        repo -> Varchar,
        host -> Nullable<Varchar>,
        software_id -> Nullable<Uuid>,
        enabled -> Bool,
        sync_status -> Varchar,
        last_synced_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_version_drafts (id) {
        id -> Uuid,
        connection_id -> Uuid,
        software_id -> Nullable<Uuid>,
        provider -> Varchar,
        version -> Varchar,
        tag -> Varchar,
        title -> Nullable<Varchar>,
        changelog -> Text,
        published_at -> Nullable<Timestamptz>,
        status -> Varchar,
        source -> Varchar,
        assets -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_sync_history (id) {
        id -> Uuid,
        connection_id -> Uuid,
        provider -> Varchar,
        status -> Varchar,
        releases_found -> Int4,
        drafts_created -> Int4,
        error -> Nullable<Text>,
        rate_limit_remaining -> Nullable<Int4>,
        started_at -> Timestamptz,
        finished_at -> Timestamptz,
        duration_ms -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    vcs_provider_settings (provider) {
        provider -> Varchar,
        token_encrypted -> Nullable<Text>,
        webhook_secret_encrypted -> Nullable<Text>,
        api_base_override -> Nullable<Varchar>,
        gitlab_hosts -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
