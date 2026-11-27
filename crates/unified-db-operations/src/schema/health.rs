// Diesel schema for the health domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    storage_health_checks (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        ok -> Bool,
        latency_ms -> Int8,
        detail -> Varchar,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// Advisor + Risk (2026-11-22-000039_advisor_risk).

diesel::table! {
    advisor_suggestions (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        rule_id -> Varchar,
        title -> Varchar,
        body -> Text,
        recommended_action -> Text,
        severity -> Varchar,
        severity_reason -> Text,
        status -> Varchar,
        source_type -> Varchar,
        source_id -> Varchar,
        source_ids -> Jsonb,
        figures -> Jsonb,
        dedupe_key -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        snoozed_until -> Nullable<Timestamptz>,
        acknowledged_by -> Nullable<Varchar>,
        acknowledged_at -> Nullable<Timestamptz>,
        dismissed_by -> Nullable<Varchar>,
        dismissed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    advisor_rule_runs (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        rule_id -> Varchar,
        ran_at -> Timestamptz,
        suggestions_created -> Int4,
        suggestions_updated -> Int4,
        inputs_summary -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
