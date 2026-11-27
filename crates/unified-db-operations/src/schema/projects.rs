// Diesel schema for the projects domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Project Management (2026-11-22-000031_projects).

diesel::table! {
    projects (id) {
        id -> Uuid,
        name -> Text,
        description -> Text,
        methodology -> Text,
        status -> Text,
        visibility -> Text,
        owner_id -> Text,
        company_id -> Nullable<Uuid>,
        client_id -> Nullable<Text>,
        start_date -> Nullable<Timestamptz>,
        due_date -> Nullable<Timestamptz>,
        color -> Nullable<Text>,
        icon -> Nullable<Text>,
        labels -> Jsonb,
        custom_field_definitions -> Jsonb,
        archived -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_members (id) {
        id -> Uuid,
        project_id -> Uuid,
        user_id -> Text,
        role -> Text,
        joined_at -> Timestamptz,
    }
}

diesel::table! {
    project_boards (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        kind -> Text,
        wip_limit -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_board_columns (id) {
        id -> Uuid,
        board_id -> Uuid,
        name -> Text,
        position -> Int4,
        status_key -> Text,
        wip_limit -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_milestones (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        description -> Text,
        due_date -> Nullable<Timestamptz>,
        reached -> Bool,
        reached_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_sprints (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        goal -> Text,
        status -> Text,
        start_date -> Nullable<Timestamptz>,
        end_date -> Nullable<Timestamptz>,
        velocity_planned -> Nullable<Float8>,
        velocity_actual -> Nullable<Float8>,
        standup_notes -> Jsonb,
        review_notes -> Nullable<Text>,
        retrospective_notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_phases (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        kind -> Text,
        position -> Int4,
        status -> Text,
        gate_status -> Text,
        gate_decision -> Nullable<Text>,
        start_date -> Nullable<Timestamptz>,
        end_date -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_cycles (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        appetite_weeks -> Int4,
        status -> Text,
        started_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        cooldown_weeks -> Int4,
        pitch -> Nullable<Text>,
        betting_decision -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_program_increments (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        status -> Text,
        start_date -> Nullable<Timestamptz>,
        end_date -> Nullable<Timestamptz>,
        objectives -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_business_cases (id) {
        id -> Uuid,
        project_id -> Uuid,
        title -> Text,
        justification -> Text,
        benefits -> Jsonb,
        costs -> Jsonb,
        risks -> Jsonb,
        status -> Text,
        exception_report -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
