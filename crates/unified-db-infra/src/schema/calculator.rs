// Diesel schema for the calculator domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Calculator Enterprise (2026-11-22-000100_calculator).

diesel::table! {
    calculator_saved_calculations (id) {
        id -> Uuid,
        company_id -> Uuid,
        calculator_type -> Varchar,
        title -> Varchar,
        inputs -> Jsonb,
        results -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_reports (id) {
        id -> Uuid,
        calculation_id -> Uuid,
        company_id -> Uuid,
        title -> Varchar,
        share_token -> Varchar,
        visibility -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_history (id) {
        id -> Uuid,
        company_id -> Uuid,
        calculator_type -> Varchar,
        inputs -> Jsonb,
        results -> Jsonb,
        actor -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    calculator_currency_rates (id) {
        id -> Uuid,
        base_currency -> Bpchar,
        target_currency -> Bpchar,
        rate -> Float8,
        provider -> Varchar,
        fetched_at -> Timestamptz,
        max_age_seconds -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
