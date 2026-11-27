// Diesel schema for the workflows domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Workflows (migration 000060_workflows). The self-reference
// `workflow_runs.retried_from_run_id` gets no `joinable!` (a
// table cannot join to itself in that form).

diesel::table! {
    workflows (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        trigger -> Jsonb,
        conditions -> Nullable<Jsonb>,
        actions -> Jsonb,
        is_active -> Bool,
        owner_id -> Varchar,
        company_id -> Nullable<Varchar>,
        webhook_token_hash -> Nullable<Varchar>,
        webhook_allowed_hosts -> Jsonb,
        run_count -> Int8,
        success_count -> Int8,
        failure_count -> Int8,
        dropped_count -> Int8,
        last_run_at -> Nullable<Timestamptz>,
        last_run_status -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    workflow_runs (id) {
        id -> Uuid,
        workflow_id -> Uuid,
        trigger_kind -> Varchar,
        trigger_name -> Nullable<Varchar>,
        status -> Varchar,
        payload_summary -> Jsonb,
        dry_run -> Bool,
        depth -> Int4,
        condition_matched -> Bool,
        skipped_reason -> Nullable<Text>,
        retried_from_run_id -> Nullable<Uuid>,
        duration_ms -> Nullable<Int8>,
        error -> Nullable<Text>,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    workflow_run_steps (id) {
        id -> Uuid,
        run_id -> Uuid,
        workflow_id -> Uuid,
        step_index -> Int4,
        action_kind -> Varchar,
        status -> Varchar,
        attempts -> Int4,
        error -> Nullable<Text>,
        detail -> Jsonb,
        duration_ms -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
