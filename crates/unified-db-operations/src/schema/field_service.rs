// Diesel schema for the field_service domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Field service (migration 000080_field_service). No `joinable!`
// entries: the migration declares no foreign keys (all references
// are plain ids by design).

diesel::table! {
    field_equipment (id) {
        id -> Uuid,
        asset_id -> Nullable<Uuid>,
        equipment_type_id -> Uuid,
        name -> Text,
        serial_number -> Nullable<Text>,
        location -> Nullable<Text>,
        install_date -> Nullable<Date>,
        warranty_end -> Nullable<Date>,
        status -> Text,
        usage_reading -> Nullable<Float8>,
        usage_unit -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_maintenance_plans (id) {
        id -> Uuid,
        equipment_id -> Uuid,
        template_id -> Nullable<Uuid>,
        name -> Text,
        description -> Nullable<Text>,
        interval_value -> Int8,
        interval_unit -> Text,
        last_service_at -> Nullable<Timestamptz>,
        usage_at_last_service -> Nullable<Float8>,
        next_due_at -> Nullable<Timestamptz>,
        next_due_usage -> Nullable<Float8>,
        is_active -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_work_orders (id) {
        id -> Uuid,
        equipment_id -> Uuid,
        plan_id -> Nullable<Uuid>,
        project_id -> Nullable<Text>,
        title -> Text,
        description -> Nullable<Text>,
        order_type -> Text,
        priority -> Text,
        status -> Text,
        assignee_id -> Nullable<Text>,
        scheduled_at -> Nullable<Timestamptz>,
        due_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        verified_at -> Nullable<Timestamptz>,
        closed_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_work_order_items (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        title -> Text,
        position -> Int8,
        is_done -> Bool,
        done_by -> Nullable<Text>,
        done_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_work_order_parts (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        stock_id -> Nullable<Uuid>,
        part_name -> Text,
        quantity -> Int8,
        unit -> Text,
        status -> Text,
        consumed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_dispatch_assignments (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        technician_id -> Text,
        sequence -> Int8,
        status -> Text,
        assigned_by -> Text,
        assigned_at -> Timestamptz,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        released_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    field_signoffs (id) {
        id -> Uuid,
        work_order_id -> Uuid,
        customer_name -> Text,
        signature_ref -> Text,
        signed_at -> Timestamptz,
        notes -> Nullable<Text>,
        survey_distribution_id -> Nullable<Uuid>,
        follow_up_status -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
