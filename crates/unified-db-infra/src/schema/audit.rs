// Diesel schema for the audit domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Audit (2026-11-22-000024_audit).

diesel::table! {
    audit_entries (id) {
        id -> Uuid,
        seq -> Int8,
        actor -> Varchar,
        action -> Varchar,
        resource -> Varchar,
        metadata -> Jsonb,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// Gateway (2026-11-22-000025_gateway).

diesel::table! {
    gateway_routing_rules (id) {
        id -> Uuid,
        name -> Varchar,
        priority -> Int4,
        enabled -> Bool,
        host -> Nullable<Varchar>,
        path_prefix -> Nullable<Varchar>,
        path_regex -> Nullable<Varchar>,
        headers -> Jsonb,
        upstream -> Jsonb,
        strategy -> Varchar,
        require_tls -> Bool,
        rate_limit_override -> Nullable<Varchar>,
        health_check -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gateway_request_log (id) {
        id -> Uuid,
        request_id -> Varchar,
        method -> Varchar,
        host -> Varchar,
        path -> Varchar,
        status -> Int4,
        duration_ms -> Int8,
        upstream -> Nullable<Varchar>,
        backend_id -> Nullable<Uuid>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
