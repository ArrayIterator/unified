// Diesel schema for the assets domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Assets (migration 000050_assets). `Asset` carries NUMERIC columns,
// so it skips ALL serde derives per the repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    assets (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        asset_type -> Varchar,
        serial_number -> Nullable<Varchar>,
        purchase_date -> Nullable<Date>,
        purchase_price -> Numeric,
        currency -> Bpchar,
        vendor -> Nullable<Varchar>,
        location -> Nullable<Varchar>,
        status -> Varchar,
        warranty_end -> Nullable<Date>,
        license_expires_at -> Nullable<Date>,
        license_seats -> Nullable<Int8>,
        depreciation_method -> Nullable<Varchar>,
        useful_life_years -> Nullable<Int8>,
        salvage_value -> Numeric,
        depreciation_status -> Varchar,
        depreciation_schedule -> Nullable<Jsonb>,
        depreciation_error -> Nullable<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_assignments (id) {
        id -> Uuid,
        asset_id -> Uuid,
        user_id -> Varchar,
        assigned_by -> Varchar,
        assigned_at -> Timestamptz,
        returned_at -> Nullable<Timestamptz>,
        return_reason -> Nullable<Text>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock (id) {
        id -> Uuid,
        asset_id -> Uuid,
        quantity_on_hand -> Int8,
        reorder_threshold -> Int8,
        location -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock_adjustments (id) {
        id -> Uuid,
        stock_id -> Uuid,
        asset_id -> Uuid,
        delta -> Int8,
        quantity_after -> Int8,
        reason -> Text,
        adjusted_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock_takes (id) {
        id -> Uuid,
        title -> Varchar,
        notes -> Nullable<Text>,
        status -> Varchar,
        started_by -> Varchar,
        started_at -> Timestamptz,
        closed_at -> Nullable<Timestamptz>,
        closed_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    asset_stock_take_lines (id) {
        id -> Uuid,
        stock_take_id -> Uuid,
        asset_id -> Uuid,
        stock_id -> Uuid,
        expected_quantity -> Int8,
        counted_quantity -> Nullable<Int8>,
        counted_by -> Nullable<Varchar>,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
