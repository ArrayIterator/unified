// Diesel schema for the properties domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Properties (migration 000061_properties; batch 5 notes §c).
// ---------------------------------------------------------------------------

diesel::table! {
    properties (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        latitude -> Nullable<Float8>,
        longitude -> Nullable<Float8>,
        address -> Nullable<Varchar>,
        total_area_sqm -> Nullable<Numeric>,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        cover_storage_target -> Nullable<Varchar>,
        cover_storage_key -> Nullable<Varchar>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_plans (id) {
        id -> Uuid,
        property_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        geometry -> Nullable<Jsonb>,
        geometry_hash -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_blocks (id) {
        id -> Uuid,
        plan_id -> Uuid,
        property_id -> Uuid,
        title -> Varchar,
        notes -> Nullable<Text>,
        geometry -> Jsonb,
        geometry_hash -> Varchar,
        area_sqm -> Nullable<Numeric>,
        length_m -> Nullable<Numeric>,
        price -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        assigned_customer_id -> Nullable<Text>,
        click_behavior -> Varchar,
        click_link_kind -> Nullable<Varchar>,
        click_link_target -> Nullable<Text>,
        order_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_block_history (id) {
        id -> Uuid,
        block_id -> Uuid,
        property_id -> Uuid,
        plan_id -> Uuid,
        from_status -> Varchar,
        to_status -> Varchar,
        actor_id -> Text,
        reason -> Nullable<Text>,
        order_id -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}
