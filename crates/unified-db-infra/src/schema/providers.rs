// Diesel schema for the providers domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Providers (2026-11-22-000010_providers).

diesel::table! {
    provider_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint -> Nullable<Varchar>,
        region -> Nullable<Varchar>,
        project_id -> Nullable<Varchar>,
        credentials -> Jsonb,
        custom_config -> Nullable<Jsonb>,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    provider_servers (id) {
        id -> Uuid,
        connection_id -> Uuid,
        remote_id -> Varchar,
        name -> Varchar,
        item_kind -> Varchar,
        status -> Varchar,
        raw_status -> Nullable<Varchar>,
        region -> Nullable<Varchar>,
        public_ip -> Nullable<Varchar>,
        private_ip -> Nullable<Varchar>,
        cpu_cores -> Nullable<Int4>,
        memory_mb -> Nullable<Int8>,
        disk_gb -> Nullable<Int8>,
        image -> Nullable<Varchar>,
        remote_created_at -> Nullable<Varchar>,
        fetched_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// Panels (2026-11-22-000013_panels).

diesel::table! {
    panel_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        endpoint -> Nullable<Varchar>,
        credentials -> Jsonb,
        generic_config -> Nullable<Jsonb>,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        last_synced_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    panel_synced_records (connection_id, collection, remote_id) {
        connection_id -> Uuid,
        collection -> Varchar,
        remote_id -> Varchar,
        name -> Varchar,
        status -> Varchar,
        hash -> Varchar,
        last_synced_at -> Timestamptz,
        direction -> Varchar,
        conflict -> Bool,
        local_changed -> Bool,
        prefer_local -> Bool,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
