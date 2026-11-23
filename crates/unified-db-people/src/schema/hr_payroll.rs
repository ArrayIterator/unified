// Diesel schema for the hr_payroll domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    hr_payroll_runs (id) {
        id -> Uuid,
        period -> Varchar,
        currency -> Varchar,
        status -> Varchar,
        total_gross -> Numeric,
        total_net -> Numeric,
        total_employees -> Int4,
        finalized_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_payroll_items (id) {
        id -> Uuid,
        payroll_run_id -> Uuid,
        employee_id -> Uuid,
        gross -> Numeric,
        adjustments -> Numeric,
        deductions -> Numeric,
        net -> Numeric,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_review_cycles (id) {
        id -> Uuid,
        name -> Varchar,
        period_start -> Date,
        period_end -> Date,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_reviews (id) {
        id -> Uuid,
        cycle_id -> Uuid,
        employee_id -> Uuid,
        reviewer_id -> Varchar,
        rating -> Nullable<Int2>,
        goals -> Jsonb,
        reviewer_notes -> Text,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_checklists (id) {
        id -> Uuid,
        employee_id -> Uuid,
        kind -> Varchar,
        status -> Varchar,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_checklist_items (id) {
        id -> Uuid,
        checklist_id -> Uuid,
        title -> Varchar,
        completed -> Bool,
        completed_by -> Nullable<Varchar>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_documents (id) {
        id -> Uuid,
        employee_id -> Uuid,
        title -> Varchar,
        document_type -> Varchar,
        storage_file_id -> Varchar,
        mime_type -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        uploaded_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
