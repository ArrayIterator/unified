// Diesel schema for the departments domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Departments (CORE) (2026-11-22-000026_departments).
// ---------------------------------------------------------------------------

diesel::table! {
    departments (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Text,
        email -> Nullable<Varchar>,
        status -> Varchar,
        is_default -> Bool,
        sla_first_response_minutes -> Int4,
        sla_resolution_minutes -> Int4,
        working_hours -> Jsonb,
        auto_reply -> Jsonb,
        agents -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    department_routing_rules (id) {
        id -> Uuid,
        department_id -> Uuid,
        match_type -> Varchar,
        match_value -> Varchar,
        sort_order -> Int4,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
