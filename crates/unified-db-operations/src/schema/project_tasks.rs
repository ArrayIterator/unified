// Diesel schema for the project_tasks domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    project_tasks (id) {
        id -> Uuid,
        project_id -> Uuid,
        board_id -> Nullable<Uuid>,
        parent_id -> Nullable<Uuid>,
        epic_id -> Nullable<Uuid>,
        title -> Text,
        description -> Text,
        task_type -> Text,
        status -> Text,
        priority -> Text,
        assignee_id -> Nullable<Text>,
        reporter_id -> Text,
        estimate_points -> Nullable<Float8>,
        estimate_hours -> Nullable<Float8>,
        labels -> Jsonb,
        custom_values -> Jsonb,
        start_date -> Nullable<Timestamptz>,
        due_date -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        sprint_id -> Nullable<Uuid>,
        phase_id -> Nullable<Uuid>,
        cycle_id -> Nullable<Uuid>,
        pi_id -> Nullable<Uuid>,
        milestone_id -> Nullable<Uuid>,
        process_group -> Nullable<Text>,
        knowledge_area -> Nullable<Text>,
        waste_flag -> Nullable<Bool>,
        pair_partner_id -> Nullable<Text>,
        test_first -> Bool,
        dmaic_phase -> Nullable<Text>,
        appetite_weeks -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_dependencies (id) {
        id -> Uuid,
        task_id -> Uuid,
        depends_on_task_id -> Uuid,
        kind -> Text,
        lag_days -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_comments (id) {
        id -> Uuid,
        task_id -> Uuid,
        author_id -> Text,
        body -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_watchers (id) {
        id -> Uuid,
        task_id -> Uuid,
        user_id -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    project_task_attachments (id) {
        id -> Uuid,
        task_id -> Uuid,
        target -> Text,
        key -> Text,
        stored_key -> Text,
        filename -> Text,
        content_type -> Nullable<Text>,
        size_bytes -> Nullable<Int8>,
        uploaded_by -> Text,
        uploaded_at -> Timestamptz,
    }
}

diesel::table! {
    project_time_entries (id) {
        id -> Uuid,
        task_id -> Uuid,
        project_id -> Uuid,
        user_id -> Text,
        description -> Text,
        hours -> Float8,
        billable -> Bool,
        billed -> Bool,
        invoice_id -> Nullable<Text>,
        started_at -> Timestamptz,
        ended_at -> Nullable<Timestamptz>,
        timer_running -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_automation_rules (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        trigger -> Text,
        conditions -> Jsonb,
        actions -> Jsonb,
        enabled -> Bool,
        run_count -> Int8,
        last_run_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_templates (id) {
        id -> Uuid,
        name -> Text,
        description -> Text,
        methodology -> Text,
        boards -> Jsonb,
        #[sql_name = "columns"]
        columns_ -> Jsonb,
        statuses -> Jsonb,
        phases -> Jsonb,
        custom_fields -> Jsonb,
        is_system -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_activity (id) {
        id -> Uuid,
        project_id -> Uuid,
        actor_id -> Text,
        action -> Text,
        target_type -> Text,
        target_id -> Nullable<Text>,
        details -> Jsonb,
        created_at -> Timestamptz,
    }
}
