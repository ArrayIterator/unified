// Diesel schema for the school_fees domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// School fees (migration 000096_school_fees). Ids are TEXT (not UUID)
// and all money amounts are NUMERIC, so these structs skip serde
// derives (PgNumeric has no serde implementation).

diesel::table! {
    school_fee_types (id) {
        id -> Text,
        code -> Text,
        name -> Text,
        description -> Nullable<Text>,
        category -> Text,
        billing_period -> Text,
        default_amount_minor -> Numeric,
        currency -> Bpchar,
        auto_assign_on_enrolment -> Bool,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_schedules (id) {
        id -> Text,
        student_user_id -> Text,
        program_code -> Text,
        level_code -> Text,
        academic_period_id -> Text,
        fee_type_id -> Text,
        amount_minor -> Numeric,
        currency -> Bpchar,
        due_date -> Timestamptz,
        status -> Text,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_charges (id) {
        id -> Text,
        schedule_id -> Text,
        student_user_id -> Text,
        fee_type_id -> Text,
        academic_period_id -> Text,
        period_label -> Text,
        amount_minor -> Numeric,
        discount_minor -> Numeric,
        total_minor -> Numeric,
        amount_paid_minor -> Numeric,
        currency -> Bpchar,
        invoice_id -> Nullable<Text>,
        discount_ids -> Jsonb,
        due_date -> Timestamptz,
        status -> Text,
        paid_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_discounts (id) {
        id -> Text,
        student_user_id -> Text,
        fee_type_id -> Nullable<Text>,
        kind -> Text,
        percent_basis_points -> Int8,
        amount_minor -> Numeric,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        approved -> Bool,
        approved_by -> Nullable<Text>,
        reason -> Nullable<Text>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_receipts (id) {
        id -> Text,
        number -> Text,
        charge_id -> Text,
        student_user_id -> Text,
        amount_minor -> Numeric,
        currency -> Bpchar,
        method -> Nullable<Text>,
        reference -> Nullable<Text>,
        issued_by -> Text,
        issued_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_guardians (id) {
        id -> Text,
        guardian_user_id -> Text,
        student_user_id -> Text,
        relationship -> Text,
        active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    school_fee_reminders (charge_id, offset_days, recipient_user_id) {
        charge_id -> Text,
        offset_days -> Int4,
        recipient_user_id -> Text,
        sent_at -> Timestamptz,
    }
}
