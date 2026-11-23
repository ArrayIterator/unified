// Diesel schema for the tasks domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Tasks and system records (2026-11-22-000011_tasks_records).
// ---------------------------------------------------------------------------

diesel::table! {
    tasks (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        assignee_id -> Nullable<Uuid>,
        reporter_id -> Nullable<Uuid>,
        priority -> Varchar,
        status -> Varchar,
        due_date -> Nullable<Timestamptz>,
        company_id -> Nullable<Uuid>,
        project_name -> Nullable<Varchar>,
        tags -> Jsonb,
        estimate_minutes -> Nullable<Int4>,
        spent_minutes -> Int4,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    task_comments (id) {
        id -> Uuid,
        task_id -> Uuid,
        author_id -> Nullable<Uuid>,
        body -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    task_checklist_items (id) {
        id -> Uuid,
        task_id -> Uuid,
        title -> Varchar,
        done -> Bool,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    system_records (id) {
        id -> Uuid,
        seq -> Int8,
        actor -> Varchar,
        action -> Varchar,
        entity_type -> Varchar,
        entity_id -> Varchar,
        summary -> Text,
        details -> Jsonb,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Logs and billing (2026-11-22-000012_logs_billing).
// ---------------------------------------------------------------------------

diesel::table! {
    app_logs (id) {
        id -> Uuid,
        seq -> Int8,
        timestamp -> Timestamptz,
        level -> Varchar,
        kind -> Varchar,
        source -> Varchar,
        message -> Text,
        context -> Jsonb,
        request_id -> Nullable<Varchar>,
        user_id -> Nullable<Varchar>,
        method -> Nullable<Varchar>,
        path -> Nullable<Varchar>,
        status -> Nullable<Int4>,
        duration_ms -> Nullable<Int8>,
        ip -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logs_settings (key) {
        key -> Varchar,
        retention_days -> Int4,
        max_entries -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
