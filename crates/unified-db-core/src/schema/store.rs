// Diesel schema for the store domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    store_products (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        price -> Numeric,
        currency -> Varchar,
        kind -> Varchar,
        license_product -> Bool,
        file_id -> Nullable<Uuid>,
        slug -> Nullable<Varchar>,
        software_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        status -> Varchar,
        company_id -> Nullable<Uuid>,
        pricing_model -> Varchar,
        billing_period -> Nullable<Varchar>,
        renewal_price -> Nullable<Numeric>,
        trial_days -> Int4,
        updates_years -> Nullable<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_pricing_tiers (id) {
        id -> Uuid,
        product_id -> Uuid,
        slug -> Varchar,
        label -> Varchar,
        price -> Numeric,
        renewal_price -> Nullable<Numeric>,
        billing_period -> Nullable<Varchar>,
        trial_days -> Int4,
        seats -> Int4,
        download_limit -> Int4,
        features -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_orders (id) {
        id -> Uuid,
        user_id -> Varchar,
        subtotal -> Numeric,
        discount_total -> Numeric,
        total -> Numeric,
        currency -> Varchar,
        coupon_code -> Nullable<Varchar>,
        company_id -> Nullable<Uuid>,
        trial_until -> Nullable<Timestamptz>,
        status -> Varchar,
        provider -> Nullable<Varchar>,
        reference -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_order_items (id) {
        id -> Uuid,
        order_id -> Uuid,
        product_id -> Nullable<Uuid>,
        name -> Varchar,
        unit_price -> Numeric,
        quantity -> Int4,
        line_total -> Numeric,
        license_product -> Bool,
        file_id -> Nullable<Uuid>,
        tier_id -> Nullable<Uuid>,
        tier_name -> Nullable<Varchar>,
        software_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        company_id -> Nullable<Uuid>,
        pricing_model -> Varchar,
        billing_period -> Nullable<Varchar>,
        seats -> Int4,
        expires_at -> Nullable<Timestamptz>,
        download_limit -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_entitlements (id) {
        id -> Uuid,
        license_id -> Nullable<Uuid>,
        license_key -> Nullable<Varchar>,
        order_id -> Nullable<Uuid>,
        user_id -> Varchar,
        product_id -> Nullable<Uuid>,
        tier_id -> Nullable<Uuid>,
        software_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        release_id -> Nullable<Uuid>,
        download_limit -> Int4,
        downloads_used -> Int4,
        status -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_downloads (id) {
        id -> Uuid,
        license_id -> Nullable<Uuid>,
        release_id -> Nullable<Uuid>,
        software_slug -> Nullable<Varchar>,
        order_id -> Nullable<Uuid>,
        downloaded_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_catalog (slug) {
        slug -> Varchar,
        name -> Varchar,
        kind -> Varchar,
        product_id -> Nullable<Uuid>,
        product_name -> Varchar,
        latest_version -> Varchar,
        release_id -> Nullable<Uuid>,
        file_id -> Nullable<Uuid>,
        changelog -> Text,
        requires_wp -> Nullable<Varchar>,
        tested_wp -> Nullable<Varchar>,
        requires_php -> Nullable<Varchar>,
        license_required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_issued_licenses (license_id) {
        license_id -> Uuid,
        license_key -> Varchar,
        product -> Varchar,
        order_id -> Nullable<Uuid>,
        user_id -> Nullable<Varchar>,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
