// Diesel schema for the procurement domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Procurement (migration 000049_procurement). `PurchaseOrder`,
// `PurchaseOrderLine`, `GoodsReceipt` and `ProcurementBudget`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    vendors (id) {
        id -> Uuid,
        name -> Varchar,
        contact_name -> Nullable<Varchar>,
        email -> Nullable<Varchar>,
        phone -> Nullable<Varchar>,
        address -> Nullable<Varchar>,
        categories -> Array<Text>,
        tags -> Array<Text>,
        status -> Varchar,
        payment_reference -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        rating -> Nullable<Int2>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    purchase_orders (id) {
        id -> Uuid,
        po_number -> Varchar,
        vendor_id -> Uuid,
        title -> Varchar,
        category -> Nullable<Varchar>,
        currency -> Bpchar,
        status -> Varchar,
        subtotal -> Numeric,
        tax_total -> Numeric,
        total -> Numeric,
        promised_date -> Nullable<Date>,
        ordered_at -> Nullable<Timestamptz>,
        received_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Text>,
        requested_by -> Varchar,
        approved_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    purchase_order_lines (id) {
        id -> Uuid,
        purchase_order_id -> Uuid,
        description -> Text,
        quantity -> Numeric,
        unit_price -> Numeric,
        tax_rate -> Numeric,
        line_total -> Numeric,
        received_quantity -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    purchase_order_approvals (id) {
        id -> Uuid,
        purchase_order_id -> Uuid,
        approver_id -> Varchar,
        approver_role -> Varchar,
        decision -> Varchar,
        reason -> Nullable<Text>,
        decided_at -> Timestamptz,
    }
}

diesel::table! {
    goods_receipts (id) {
        id -> Uuid,
        purchase_order_id -> Uuid,
        line_id -> Uuid,
        quantity_received -> Numeric,
        quality_note -> Nullable<Text>,
        received_by -> Varchar,
        received_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    procurement_budgets (id) {
        id -> Uuid,
        category -> Nullable<Varchar>,
        vendor_id -> Nullable<Uuid>,
        period_start -> Date,
        period_end -> Date,
        budget_amount -> Numeric,
        currency -> Bpchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
