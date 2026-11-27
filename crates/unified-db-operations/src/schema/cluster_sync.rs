// Diesel schema for the cluster_sync domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Cluster sync reliability (migration 000082_cluster_sync_reliability).

diesel::table! {
    cluster_sync_conflicts (id) {
        id -> Uuid,
        log_id -> Nullable<Uuid>,
        plan_id -> Nullable<Uuid>,
        cluster_id -> Uuid,
        node_id -> Nullable<Uuid>,
        domain -> Varchar,
        key -> Varchar,
        local_updated_at -> Nullable<Timestamptz>,
        remote_updated_at -> Nullable<Timestamptz>,
        winner -> Varchar,
        reason -> Text,
        local_payload -> Nullable<Jsonb>,
        remote_payload -> Nullable<Jsonb>,
        status -> Varchar,
        resolution -> Nullable<Varchar>,
        resolved_by -> Nullable<Varchar>,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_retries (id) {
        id -> Uuid,
        plan_id -> Uuid,
        cluster_id -> Uuid,
        node_id -> Nullable<Uuid>,
        domain -> Varchar,
        direction -> Varchar,
        attempt -> Int4,
        max_attempts -> Int4,
        next_retry_at -> Timestamptz,
        last_error -> Nullable<Text>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_idempotency_keys (scope, key) {
        scope -> Varchar,
        key -> Varchar,
        status -> Int4,
        body -> Jsonb,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_settings (key) {
        key -> Varchar,
        value -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_domains (domain) {
        domain -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
