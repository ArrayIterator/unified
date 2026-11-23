// Diesel schema for the expenses domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Expenses (migration 000088_expenses). Money is stored in minor
// units (BIGINT), so no NUMERIC columns and serde derives apply.
// ---------------------------------------------------------------------------

diesel::table! {
    expense_categories (id) {
        id -> Uuid,
        name -> Text,
        code -> Text,
        company_id -> Nullable<Text>,
        receipt_required -> Bool,
        receipt_required_above_minor -> Nullable<Int8>,
        per_expense_limit_minor -> Nullable<Int8>,
        per_day_limit_minor -> Nullable<Int8>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_policies (id) {
        id -> Uuid,
        name -> Text,
        company_id -> Nullable<Text>,
        currency -> Text,
        mileage_rate_minor -> Nullable<Int8>,
        mileage_unit -> Text,
        per_diem_rate_minor -> Nullable<Int8>,
        max_report_total_minor -> Nullable<Int8>,
        approval_chain_id -> Nullable<Text>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_reports (id) {
        id -> Uuid,
        employee_id -> Text,
        company_id -> Nullable<Text>,
        policy_id -> Nullable<Uuid>,
        title -> Text,
        notes -> Nullable<Text>,
        status -> Text,
        currency -> Text,
        total_minor -> Int8,
        violations -> Array<Text>,
        approval_request_id -> Nullable<Text>,
        submitted_at -> Nullable<Timestamptz>,
        decided_at -> Nullable<Timestamptz>,
        decided_by -> Nullable<Text>,
        decision_reason -> Nullable<Text>,
        reimbursement_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expenses (id) {
        id -> Uuid,
        report_id -> Uuid,
        kind -> Text,
        category_id -> Uuid,
        expense_date -> Date,
        amount_minor -> Int8,
        currency -> Text,
        description -> Text,
        project_id -> Nullable<Text>,
        client_id -> Nullable<Text>,
        distance -> Nullable<Float8>,
        days -> Nullable<Int8>,
        rate_minor_applied -> Nullable<Int8>,
        violations -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_receipts (id) {
        id -> Uuid,
        expense_id -> Uuid,
        storage_target -> Text,
        storage_key -> Text,
        file_name -> Text,
        content_type -> Nullable<Text>,
        uploaded_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    expense_reimbursements (id) {
        id -> Uuid,
        report_id -> Uuid,
        employee_id -> Text,
        amount_minor -> Int8,
        currency -> Text,
        status -> Text,
        payable_reference -> Text,
        failure_reason -> Nullable<Text>,
        paid_at -> Nullable<Timestamptz>,
        attempts -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    expense_adjustments (id) {
        id -> Uuid,
        report_id -> Uuid,
        kind -> Text,
        amount_minor -> Int8,
        currency -> Text,
        reason -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    expense_currency_rates (id) {
        id -> Uuid,
        base_currency -> Text,
        target_currency -> Text,
        rate -> Float8,
        source -> Text,
        created_at -> Timestamptz,
    }
}
