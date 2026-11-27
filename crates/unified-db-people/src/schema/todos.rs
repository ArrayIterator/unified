// Diesel schema for the todos domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Todos (migration 000035_todos; batch 2 notes sec. 7). The notes write
// `Integer`/`BigInt`; normalised to the file's `Int4`/`Int8` style.

diesel::table! {
    todo_lists (id) {
        id -> Uuid,
        owner_id -> Varchar,
        name -> Varchar,
        color -> Varchar,
        icon -> Varchar,
        archived -> Bool,
        sort_order -> Int4,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todos (id) {
        id -> Uuid,
        list_id -> Uuid,
        owner_id -> Varchar,
        title -> Varchar,
        notes -> Nullable<Text>,
        due_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        priority -> Varchar,
        status -> Varchar,
        tags -> Jsonb,
        subtasks -> Jsonb,
        attachments -> Jsonb,
        recurrence_frequency -> Nullable<Varchar>,
        recurrence_interval -> Int4,
        recurrence_count -> Nullable<Int4>,
        recurrence_until -> Nullable<Timestamptz>,
        recurrence_remaining -> Nullable<Int4>,
        recurrence_group_id -> Nullable<Uuid>,
        position -> Int4,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_subtasks (id) {
        id -> Uuid,
        todo_id -> Uuid,
        title -> Varchar,
        done -> Bool,
        position -> Int4,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_shares (id) {
        id -> Uuid,
        list_id -> Uuid,
        owner_id -> Varchar,
        shared_with_user_id -> Varchar,
        permission -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_completion_log (id) {
        id -> Uuid,
        todo_id -> Uuid,
        user_id -> Varchar,
        action -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_reminders (id) {
        id -> Uuid,
        todo_id -> Uuid,
        owner_id -> Varchar,
        offset_minutes -> Int8,
        remind_at -> Timestamptz,
        due_at -> Timestamptz,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
