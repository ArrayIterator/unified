// Diesel schema for the hr domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Human Resource Management (2026-11-22-000037_hr).

diesel::table! {
    hr_employees (id) {
        id -> Uuid,
        user_id -> Varchar,
        employee_code -> Varchar,
        position_id -> Nullable<Uuid>,
        department_id -> Nullable<Varchar>,
        department_verified -> Bool,
        manager_id -> Nullable<Uuid>,
        status -> Varchar,
        hire_date -> Date,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_positions (id) {
        id -> Uuid,
        title -> Varchar,
        department_id -> Nullable<Varchar>,
        level -> Nullable<Varchar>,
        salary_band_min -> Nullable<Numeric>,
        salary_band_max -> Nullable<Numeric>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_contracts (id) {
        id -> Uuid,
        employee_id -> Uuid,
        contract_type -> Varchar,
        start_date -> Date,
        end_date -> Nullable<Date>,
        salary_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_types (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        default_days_per_year -> Numeric,
        paid -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_balances (id) {
        id -> Uuid,
        employee_id -> Uuid,
        leave_type_id -> Uuid,
        year -> Int4,
        entitled_days -> Numeric,
        used_days -> Numeric,
        carried_over_days -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_leave_requests (id) {
        id -> Uuid,
        employee_id -> Uuid,
        leave_type_id -> Uuid,
        start_date -> Date,
        end_date -> Date,
        days -> Numeric,
        status -> Varchar,
        reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        review_note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hr_attendance_records (id) {
        id -> Uuid,
        employee_id -> Uuid,
        user_id -> Varchar,
        work_date -> Date,
        minutes_worked -> Int4,
        source -> Varchar,
        entry_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
