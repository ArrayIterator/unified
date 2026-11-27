// Diesel schema for the manufacturing domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Manufacturing / MRP (migration 000086_manufacturing).

diesel::table! {
    mfg_boms (id) {
        id -> Uuid,
        product_id -> Varchar,
        name -> Varchar,
        code -> Varchar,
        description -> Nullable<Text>,
        status -> Varchar,
        active_version_id -> Nullable<Uuid>,
        latest_version -> Int4,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_bom_versions (id) {
        id -> Uuid,
        bom_id -> Uuid,
        version -> Int4,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_bom_version_lines (id) {
        id -> Uuid,
        bom_version_id -> Uuid,
        component_product_id -> Varchar,
        quantity -> Numeric,
        unit -> Varchar,
        scrap_allowance_percent -> Numeric,
        notes -> Nullable<Text>,
    }
}

diesel::table! {
    mfg_work_centers (id) {
        id -> Uuid,
        name -> Varchar,
        code -> Varchar,
        capacity_hours_per_day -> Numeric,
        cost_per_hour -> Nullable<Numeric>,
        currency -> Nullable<Bpchar>,
        status -> Varchar,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_bom_routing_steps (id) {
        id -> Uuid,
        bom_version_id -> Uuid,
        sequence -> Int4,
        name -> Varchar,
        work_center_id -> Uuid,
        duration_minutes -> Numeric,
        description -> Nullable<Text>,
    }
}

diesel::table! {
    mfg_work_orders (id) {
        id -> Uuid,
        number -> Varchar,
        bom_id -> Uuid,
        bom_version_id -> Uuid,
        bom_version -> Int4,
        product_id -> Varchar,
        quantity_planned -> Numeric,
        quantity_produced -> Numeric,
        quantity_scrapped -> Numeric,
        status -> Varchar,
        planned_start -> Nullable<Timestamptz>,
        planned_end -> Nullable<Timestamptz>,
        actual_start -> Nullable<Timestamptz>,
        actual_end -> Nullable<Timestamptz>,
        company_id -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        inspection_status -> Nullable<Varchar>,
        inspection_notes -> Nullable<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_work_order_requirements (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        component_product_id -> Varchar,
        quantity_required -> Numeric,
        unit -> Varchar,
        quantity_reserved -> Numeric,
        quantity_consumed -> Numeric,
        quantity_short -> Numeric,
    }
}

diesel::table! {
    mfg_scrap_reasons (id) {
        id -> Uuid,
        code -> Varchar,
        label -> Varchar,
        active -> Bool,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_units (id) {
        id -> Uuid,
        code -> Varchar,
        label -> Varchar,
        active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_production_runs (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        quantity_produced -> Numeric,
        quantity_scrapped -> Numeric,
        scrap_reason_id -> Nullable<Uuid>,
        work_center_id -> Nullable<Uuid>,
        operator_id -> Varchar,
        status -> Varchar,
        notes -> Nullable<Text>,
        started_at -> Timestamptz,
        completed_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_reservations (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        component_product_id -> Varchar,
        quantity_reserved -> Numeric,
        quantity_consumed -> Numeric,
        unit -> Varchar,
        status -> Varchar,
        stock_unknown -> Bool,
        confirmation_reference -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mfg_stock_notices (id) {
        id -> Uuid,
        kind -> Varchar,
        work_order_id -> Nullable<Uuid>,
        reservation_id -> Nullable<Uuid>,
        component_product_id -> Nullable<Varchar>,
        quantity -> Nullable<Numeric>,
        message -> Nullable<Text>,
        received_at -> Timestamptz,
    }
}
