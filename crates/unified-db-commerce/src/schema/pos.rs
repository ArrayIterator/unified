// Diesel schema for the pos domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Point of sale (migration 000076_pos). Every table except
// `pos_registers` carries NUMERIC columns, so those six pairs skip
// ALL serde derives per the repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    pos_registers (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        location -> Nullable<Varchar>,
        status -> Varchar,
        cashiers -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_shifts (id) {
        id -> Uuid,
        register_id -> Uuid,
        cashier_id -> Varchar,
        status -> Varchar,
        opening_float -> Numeric,
        currency -> Bpchar,
        expected_cash -> Nullable<Numeric>,
        counted_cash -> Nullable<Numeric>,
        variance -> Nullable<Numeric>,
        notes -> Nullable<Text>,
        opened_at -> Timestamptz,
        closed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_sales (id) {
        id -> Uuid,
        number -> Varchar,
        register_id -> Uuid,
        shift_id -> Nullable<Uuid>,
        cashier_id -> Varchar,
        company_id -> Nullable<Varchar>,
        discount_type -> Nullable<Varchar>,
        discount_value -> Numeric,
        discount_total -> Numeric,
        subtotal -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        currency -> Bpchar,
        tax_inclusive -> Bool,
        status -> Varchar,
        amount_paid -> Numeric,
        change_due -> Numeric,
        refunded_amount -> Numeric,
        customer_name -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        completed_at -> Nullable<Timestamptz>,
        voided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_sale_items (id) {
        id -> Uuid,
        sale_id -> Uuid,
        product_id -> Varchar,
        name -> Varchar,
        barcode -> Nullable<Varchar>,
        sku -> Nullable<Varchar>,
        unit_price -> Numeric,
        quantity -> Int4,
        refunded_quantity -> Int4,
        discount_type -> Nullable<Varchar>,
        discount_value -> Numeric,
        discount_amount -> Numeric,
        line_subtotal -> Numeric,
        tax_rate -> Numeric,
        tax_amount -> Numeric,
        line_total -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pos_payments (id) {
        id -> Uuid,
        sale_id -> Uuid,
        method_id -> Varchar,
        method_code -> Varchar,
        amount -> Numeric,
        tendered -> Numeric,
        reference -> Nullable<Varchar>,
        recorded_by -> Varchar,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    pos_refunds (id) {
        id -> Uuid,
        sale_id -> Uuid,
        shift_id -> Nullable<Uuid>,
        amount -> Numeric,
        method_id -> Varchar,
        method_code -> Varchar,
        items -> Jsonb,
        reason -> Text,
        refunded_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pos_receipts (id) {
        id -> Uuid,
        number -> Varchar,
        sale_id -> Uuid,
        sale_number -> Varchar,
        register_id -> Uuid,
        register_code -> Nullable<Varchar>,
        cashier_id -> Varchar,
        store_name -> Varchar,
        store_address -> Nullable<Varchar>,
        subtotal -> Numeric,
        discount_total -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        currency -> Bpchar,
        tax_breakdown -> Jsonb,
        amount_paid -> Numeric,
        change_due -> Numeric,
        print_lines -> Jsonb,
        footer -> Nullable<Text>,
        issued_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}
