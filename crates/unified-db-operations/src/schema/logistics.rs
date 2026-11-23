// Diesel schema for the logistics domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Logistics (migration 000062_logistics; batch 5 notes §c). The notes
// write `Double`; normalised to the file's `Float8` style (same type).
// ---------------------------------------------------------------------------

diesel::table! {
    logistics_drivers (id) {
        id -> Uuid,
        user_id -> Text,
        license_number -> Varchar,
        phone -> Nullable<Varchar>,
        status -> Varchar,
        current_vehicle_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_vehicles (id) {
        id -> Uuid,
        plate -> Varchar,
        vehicle_type -> Varchar,
        capacity_kg -> Float8,
        status -> Varchar,
        assigned_driver_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        maintenance -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_warehouses (id) {
        id -> Uuid,
        name -> Varchar,
        address -> Varchar,
        capacity -> Int8,
        manager_user_id -> Nullable<Text>,
        default_low_stock_threshold -> Int8,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_shipments (id) {
        id -> Uuid,
        tracking_number -> Varchar,
        tracking_token_hash -> Varchar,
        order_id -> Nullable<Varchar>,
        order_link_status -> Varchar,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        origin_address -> Varchar,
        origin_latitude -> Nullable<Float8>,
        origin_longitude -> Nullable<Float8>,
        destination_address -> Varchar,
        destination_latitude -> Nullable<Float8>,
        destination_longitude -> Nullable<Float8>,
        weight_kg -> Float8,
        length_cm -> Float8,
        width_cm -> Float8,
        height_cm -> Float8,
        zone_origin -> Varchar,
        zone_destination -> Varchar,
        service_level -> Varchar,
        driver_id -> Nullable<Uuid>,
        vehicle_id -> Nullable<Uuid>,
        warehouse_id -> Nullable<Uuid>,
        scheduled_at -> Nullable<Timestamptz>,
        window_start -> Nullable<Timestamptz>,
        window_end -> Nullable<Timestamptz>,
        estimated_delivery_at -> Nullable<Timestamptz>,
        shipping_cost -> Nullable<Numeric>,
        currency -> Varchar,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_shipment_events (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        kind -> Varchar,
        from_status -> Nullable<Varchar>,
        to_status -> Nullable<Varchar>,
        actor -> Text,
        location -> Nullable<Varchar>,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_stock_movements (id) {
        id -> Uuid,
        warehouse_id -> Uuid,
        asset_id -> Uuid,
        direction -> Varchar,
        quantity -> Int8,
        quantity_after -> Int8,
        reason -> Varchar,
        shipment_id -> Nullable<Uuid>,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_routes (id) {
        id -> Uuid,
        name -> Varchar,
        driver_id -> Nullable<Uuid>,
        vehicle_id -> Nullable<Uuid>,
        route_date -> Timestamptz,
        start_latitude -> Nullable<Float8>,
        start_longitude -> Nullable<Float8>,
        status -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_route_stops (id) {
        id -> Uuid,
        route_id -> Uuid,
        shipment_id -> Nullable<Uuid>,
        sequence -> Int8,
        address -> Varchar,
        latitude -> Nullable<Float8>,
        longitude -> Nullable<Float8>,
        status -> Varchar,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_proofs (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        storage_target -> Varchar,
        storage_key -> Varchar,
        content_sha256 -> Varchar,
        signature_storage_target -> Nullable<Varchar>,
        signature_storage_key -> Nullable<Varchar>,
        recipient_name -> Varchar,
        delivered_at -> Timestamptz,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_exceptions (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        exception_type -> Varchar,
        description -> Text,
        status -> Varchar,
        previous_status -> Varchar,
        resolution -> Nullable<Text>,
        actor -> Text,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_rate_tables (id) {
        id -> Uuid,
        name -> Varchar,
        origin_zone -> Nullable<Varchar>,
        destination_zone -> Nullable<Varchar>,
        base_price -> Numeric,
        per_kg -> Numeric,
        per_km -> Numeric,
        currency -> Varchar,
        active -> Bool,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
