// Diesel schema for the storage_targets domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    storage_targets (name) {
        name -> Varchar,
        kind -> Varchar,
        config -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Planner and analytics (2026-11-22-000015_planner_analytics).
// ---------------------------------------------------------------------------

diesel::table! {
    capacity_pools (id) {
        id -> Uuid,
        name -> Varchar,
        provider_connection_id -> Nullable<Varchar>,
        cluster_id -> Nullable<Varchar>,
        capacity_cpu_cores -> Float8,
        capacity_ram_gb -> Float8,
        capacity_disk_gb -> Float8,
        capacity_bandwidth_tb -> Float8,
        notes -> Nullable<Text>,
        last_sync_at -> Nullable<Timestamptz>,
        last_server_count -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pool_usage_samples (id) {
        id -> Uuid,
        pool_id -> Uuid,
        used_cpu_cores -> Float8,
        used_ram_gb -> Float8,
        used_disk_gb -> Float8,
        used_bandwidth_tb -> Float8,
        source -> Varchar,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    allocation_plans (id) {
        id -> Uuid,
        pool_id -> Uuid,
        name -> Varchar,
        company_id -> Nullable<Varchar>,
        project_name -> Nullable<Varchar>,
        reserved_cpu_cores -> Float8,
        reserved_ram_gb -> Float8,
        reserved_disk_gb -> Float8,
        reserved_bandwidth_tb -> Float8,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
