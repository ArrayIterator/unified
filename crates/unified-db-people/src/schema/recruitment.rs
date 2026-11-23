// Diesel schema for the recruitment domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Recruitment (migration 000051_recruitment). `Job` and `Offer`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    jobs (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        department_id -> Nullable<Varchar>,
        location -> Nullable<Varchar>,
        job_type -> Varchar,
        salary_min -> Nullable<Numeric>,
        salary_max -> Nullable<Numeric>,
        currency -> Bpchar,
        status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    applicants (id) {
        id -> Uuid,
        job_id -> Uuid,
        name -> Varchar,
        email -> Varchar,
        phone -> Nullable<Varchar>,
        source -> Nullable<Varchar>,
        resume_storage_target -> Nullable<Varchar>,
        resume_storage_key -> Nullable<Varchar>,
        consent_contact -> Bool,
        consent_at -> Nullable<Timestamptz>,
        stage -> Varchar,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    applicant_stage_history (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        from_stage -> Nullable<Varchar>,
        to_stage -> Varchar,
        moved_by -> Varchar,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    interviews (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        scheduled_at -> Timestamptz,
        #[sql_name = "type"]
        type_ -> Varchar,
        interviewers -> Jsonb,
        notes -> Nullable<Text>,
        outcome -> Nullable<Varchar>,
        outcome_notes -> Nullable<Text>,
        status -> Varchar,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    evaluations (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        interviewer_id -> Varchar,
        interview_id -> Nullable<Uuid>,
        scores -> Jsonb,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    offers (id) {
        id -> Uuid,
        applicant_id -> Uuid,
        job_id -> Uuid,
        salary -> Numeric,
        currency -> Bpchar,
        start_date -> Nullable<Date>,
        terms -> Nullable<Text>,
        status -> Varchar,
        created_by -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
