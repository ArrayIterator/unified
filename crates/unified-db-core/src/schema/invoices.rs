// Diesel schema for the invoices domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    invoices (id) {
        id -> Uuid,
        number -> Varchar,
        order_id -> Nullable<Varchar>,
        user_id -> Varchar,
        company_id -> Nullable<Uuid>,
        subtotal -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        overdue -> Bool,
        tax_inclusive -> Bool,
        amount_paid -> Numeric,
        credit_total -> Numeric,
        balance -> Numeric,
        issue_date -> Nullable<Timestamptz>,
        due_date -> Nullable<Timestamptz>,
        payment_terms_days -> Nullable<Int4>,
        paid_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        reminders_sent -> Jsonb,
        reference -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_items (id) {
        id -> Uuid,
        invoice_id -> Uuid,
        name -> Varchar,
        quantity -> Int4,
        unit_price -> Numeric,
        line_total -> Numeric,
        tax_rate -> Float8,
        tax_amount -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_payments (id) {
        id -> Uuid,
        invoice_id -> Uuid,
        amount -> Numeric,
        method -> Varchar,
        reference -> Nullable<Varchar>,
        recorded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    credit_notes (id) {
        id -> Uuid,
        invoice_id -> Uuid,
        number -> Varchar,
        amount -> Numeric,
        reason -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
