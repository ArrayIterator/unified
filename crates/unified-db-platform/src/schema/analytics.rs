// Diesel schema for the analytics domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    analytics_sales (id) {
        id -> Uuid,
        order_id -> Nullable<Varchar>,
        kind -> Varchar,
        total -> Numeric,
        currency -> Varchar,
        products -> Jsonb,
        company_id -> Nullable<Varchar>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_license_snapshots (license_id) {
        license_id -> Varchar,
        license_key -> Nullable<Varchar>,
        product -> Varchar,
        status -> Varchar,
        last_event_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_metric_samples (id) {
        id -> Uuid,
        kind -> Varchar,
        source -> Nullable<Varchar>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_health_samples (id) {
        id -> Uuid,
        node_id -> Varchar,
        cluster_id -> Nullable<Varchar>,
        status -> Varchar,
        latency_ms -> Nullable<Int8>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_reports (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        metrics -> Jsonb,
        dimensions -> Jsonb,
        range_preset -> Nullable<Varchar>,
        start -> Nullable<Timestamptz>,
        end -> Nullable<Timestamptz>,
        filters -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_report_runs (id) {
        id -> Uuid,
        definition_id -> Uuid,
        status -> Varchar,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        row_count -> Int8,
        error -> Nullable<Text>,
        rows -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    analytics_scheduled_reports (id) {
        id -> Uuid,
        definition_id -> Uuid,
        schedule -> Varchar,
        enabled -> Bool,
        next_run_at -> Nullable<Timestamptz>,
        last_run_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
