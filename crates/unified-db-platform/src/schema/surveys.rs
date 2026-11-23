// Diesel schema for the surveys domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Surveys (migration 000055_surveys). The self-reference
// `surveys.supersedes` gets no `joinable!` (a table cannot join
// to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    surveys (id) {
        id -> Uuid,
        family_id -> Uuid,
        version -> Int4,
        supersedes -> Nullable<Uuid>,
        title -> Varchar,
        description -> Nullable<Text>,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_questions (id) {
        id -> Uuid,
        survey_id -> Uuid,
        position -> Int4,
        question_type -> Varchar,
        text -> Varchar,
        required -> Bool,
        options -> Jsonb,
        min_value -> Nullable<Int4>,
        max_value -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_distributions (id) {
        id -> Uuid,
        survey_id -> Uuid,
        trigger -> Varchar,
        department_id -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        agent_id -> Nullable<Varchar>,
        responsible_user_id -> Nullable<Varchar>,
        scheduled_at -> Nullable<Timestamptz>,
        expires_in_days -> Int4,
        status -> Varchar,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        sent_count -> Int8,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_responses (id) {
        id -> Uuid,
        survey_id -> Uuid,
        distribution_id -> Nullable<Uuid>,
        recipient_id -> Varchar,
        recipient_email -> Nullable<Varchar>,
        agent_id -> Nullable<Varchar>,
        department_id -> Nullable<Varchar>,
        product_id -> Nullable<Varchar>,
        token -> Varchar,
        token_expires_at -> Timestamptz,
        submitted_at -> Nullable<Timestamptz>,
        follow_up_status -> Varchar,
        follow_up_task_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    survey_answers (id) {
        id -> Uuid,
        response_id -> Uuid,
        question_id -> Uuid,
        score -> Nullable<Int4>,
        text -> Nullable<Text>,
        choice -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}
