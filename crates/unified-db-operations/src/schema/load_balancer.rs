// Diesel schema for the load_balancer domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Load balancer (2026-11-22-000017_load_balancer).

diesel::table! {
    load_balancers (id) {
        id -> Uuid,
        name -> Varchar,
        listen_address -> Varchar,
        listen_port -> Int4,
        tls_mode -> Varchar,
        domain -> Nullable<Varchar>,
        certificate_name -> Nullable<Varchar>,
        strategy -> Varchar,
        health_check -> Jsonb,
        enabled -> Bool,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lb_backends (id) {
        id -> Uuid,
        balancer_id -> Uuid,
        name -> Varchar,
        source_kind -> Varchar,
        source_id -> Nullable<Varchar>,
        address -> Varchar,
        weight -> Int4,
        enabled -> Bool,
        draining -> Bool,
        health -> Varchar,
        last_check_at -> Nullable<Timestamptz>,
        last_latency_ms -> Nullable<Int8>,
        consecutive_failures -> Int4,
        consecutive_successes -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lb_backend_stats (backend_id) {
        backend_id -> Uuid,
        requests_routed -> Int8,
        errors -> Int8,
        total_latency_ms -> Int8,
        latency_samples -> Int8,
        checks_total -> Int8,
        checks_ok -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
