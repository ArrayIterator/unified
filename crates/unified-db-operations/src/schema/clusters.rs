// Diesel schema for the clusters domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Clusters (2026-11-22-000008_clusters).

diesel::table! {
    clusters (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        kind -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_nodes (id) {
        id -> Uuid,
        cluster_id -> Uuid,
        display_name -> Varchar,
        base_url -> Varchar,
        role -> Varchar,
        status -> Varchar,
        secret_hash -> Varchar,
        secret_fingerprint -> Varchar,
        health -> Varchar,
        consecutive_failures -> Int4,
        last_seen_at -> Nullable<Timestamptz>,
        last_latency_ms -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_plans (id) {
        id -> Uuid,
        cluster_id -> Uuid,
        target_node_id -> Uuid,
        local_node_id -> Varchar,
        direction -> Varchar,
        domains -> Jsonb,
        schedule -> Varchar,
        enabled -> Bool,
        peer_secret_hash -> Varchar,
        peer_secret_fingerprint -> Nullable<Varchar>,
        last_run_at -> Nullable<Timestamptz>,
        last_success_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_items (domain, key) {
        domain -> Varchar,
        key -> Varchar,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_sync_logs (id) {
        id -> Uuid,
        plan_id -> Nullable<Uuid>,
        cluster_id -> Uuid,
        node_id -> Nullable<Uuid>,
        direction -> Varchar,
        domain -> Varchar,
        status -> Varchar,
        items_sent -> Int8,
        items_received -> Int8,
        items_applied -> Int8,
        items_skipped -> Int8,
        conflicts -> Jsonb,
        errors -> Jsonb,
        dry_run -> Bool,
        started_at -> Timestamptz,
        finished_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cluster_health_checks (id) {
        id -> Uuid,
        node_id -> Uuid,
        cluster_id -> Uuid,
        status -> Varchar,
        latency_ms -> Nullable<Int8>,
        error -> Nullable<Text>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
