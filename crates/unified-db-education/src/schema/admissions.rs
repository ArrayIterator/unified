// Diesel schema for the admissions domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Admissions (migration 000093_admissions).
// ---------------------------------------------------------------------------

diesel::table! {
    admission_periods (id) {
        id -> Uuid,
        academic_year -> Varchar,
        term -> Varchar,
        name -> Varchar,
        open_date -> Date,
        close_date -> Date,
        status -> Varchar,
        registration_fee_amount -> Nullable<Numeric>,
        fee_currency -> Varchar,
        registration_form_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_period_quotas (id) {
        id -> Uuid,
        period_id -> Uuid,
        program_id -> Varchar,
        level -> Varchar,
        seats -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_stage_definitions (id) {
        id -> Uuid,
        period_id -> Uuid,
        stage_key -> Varchar,
        name -> Varchar,
        position -> Int4,
        weight -> Numeric,
        pass_score -> Nullable<Numeric>,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_required_documents (id) {
        id -> Uuid,
        period_id -> Uuid,
        document_key -> Varchar,
        title -> Varchar,
        description -> Nullable<Varchar>,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_applications (id) {
        id -> Uuid,
        period_id -> Uuid,
        applicant_user_id -> Varchar,
        full_name -> Varchar,
        email -> Varchar,
        phone -> Nullable<Varchar>,
        program_id -> Varchar,
        level -> Varchar,
        form_submission_id -> Nullable<Varchar>,
        status -> Varchar,
        total_score -> Nullable<Numeric>,
        fee_amount -> Nullable<Numeric>,
        fee_currency -> Varchar,
        fee_invoice_status -> Varchar,
        fee_invoice_id -> Nullable<Varchar>,
        student_id -> Nullable<Varchar>,
        source -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_application_scores (id) {
        id -> Uuid,
        application_id -> Uuid,
        period_id -> Uuid,
        stage_id -> Uuid,
        stage_key -> Varchar,
        score -> Numeric,
        scored_by -> Varchar,
        note -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_checklist_items (id) {
        id -> Uuid,
        application_id -> Uuid,
        period_id -> Uuid,
        required_document_id -> Uuid,
        document_key -> Varchar,
        title -> Varchar,
        required -> Bool,
        status -> Varchar,
        document_id -> Nullable<Varchar>,
        verified_by -> Nullable<Varchar>,
        verified_at -> Nullable<Timestamptz>,
        rejection_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_waitlist_entries (id) {
        id -> Uuid,
        period_id -> Uuid,
        application_id -> Uuid,
        position -> Int4,
        status -> Varchar,
        promoted_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_results (id) {
        id -> Uuid,
        period_id -> Uuid,
        application_id -> Uuid,
        decision -> Varchar,
        published -> Bool,
        published_at -> Nullable<Timestamptz>,
        decided_by -> Varchar,
        note -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    admission_settings (key) {
        key -> Varchar,
        value -> Varchar,
        description -> Varchar,
        updated_at -> Timestamptz,
    }
}
