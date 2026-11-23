// Diesel schema for the inbox domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Smart inbox (migration 000054_inbox). The self-reference
// `inbox_threads.merged_into` gets no `joinable!` (a table cannot
// join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    inbox_threads (id) {
        id -> Uuid,
        source_channel -> Varchar,
        source_id -> Varchar,
        subject -> Varchar,
        customer_id -> Nullable<Varchar>,
        customer_label -> Nullable<Varchar>,
        participants -> Jsonb,
        department_id -> Nullable<Varchar>,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        priority -> Varchar,
        unread -> Bool,
        snoozed_until -> Nullable<Timestamptz>,
        snooze_scheduler_status -> Varchar,
        sla_due_at -> Nullable<Timestamptz>,
        sla_breached -> Bool,
        merged_into -> Nullable<Uuid>,
        last_activity_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inbox_assignments (id) {
        id -> Uuid,
        thread_id -> Uuid,
        assignee_id -> Varchar,
        assigned_by -> Varchar,
        active -> Bool,
        assigned_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inbox_canned_replies (id) {
        id -> Uuid,
        department_id -> Nullable<Varchar>,
        title -> Varchar,
        body -> Text,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inbox_thread_reads (thread_id, user_id) {
        thread_id -> Uuid,
        user_id -> Varchar,
        read_at -> Timestamptz,
    }
}
