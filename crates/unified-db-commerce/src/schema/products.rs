// Diesel schema for the products domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Product Management (2026-11-22-000028_products).

diesel::table! {
    product_taxonomy (id) {
        id -> Uuid,
        kind -> Varchar,
        slug -> Varchar,
        name -> Varchar,
        description -> Text,
        parent_id -> Nullable<Uuid>,
        sort_order -> Int8,
        depth -> Int8,
        path -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    products (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        description -> Text,
        kind -> Varchar,
        status -> Varchar,
        pricing_model -> Varchar,
        billing_period -> Nullable<Varchar>,
        price -> Numeric,
        price_currency -> Varchar,
        renewal_price -> Nullable<Numeric>,
        renewal_price_currency -> Nullable<Varchar>,
        trial_days -> Int4,
        vendor_company_id -> Nullable<Uuid>,
        group_id -> Nullable<Uuid>,
        category_id -> Nullable<Uuid>,
        software_id -> Nullable<Uuid>,
        license_template_name -> Nullable<Varchar>,
        license_seats -> Int4,
        license_download_limit -> Int4,
        license_duration_days -> Nullable<Int8>,
        license_requires_license -> Bool,
        support_department_id -> Nullable<Uuid>,
        support_department_name -> Nullable<Varchar>,
        bundle_rule -> Varchar,
        bundle_discount_bps -> Nullable<Int8>,
        listed -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_options (id) {
        id -> Uuid,
        product_id -> Uuid,
        name -> Varchar,
        values -> Jsonb,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_variants (id) {
        id -> Uuid,
        product_id -> Uuid,
        sku -> Varchar,
        label -> Varchar,
        option_values -> Jsonb,
        price_delta -> Numeric,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_bundle_items (id) {
        id -> Uuid,
        bundle_product_id -> Uuid,
        child_product_id -> Uuid,
        quantity -> Int4,
        price_override -> Nullable<Numeric>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_price_tiers (id) {
        id -> Uuid,
        product_id -> Uuid,
        slug -> Varchar,
        label -> Varchar,
        price -> Numeric,
        price_currency -> Varchar,
        renewal_price -> Nullable<Numeric>,
        renewal_price_currency -> Nullable<Varchar>,
        billing_period -> Nullable<Varchar>,
        trial_days -> Int4,
        seats -> Int4,
        per_seat -> Bool,
        download_limit -> Int4,
        features -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_regional_prices (id) {
        id -> Uuid,
        product_id -> Uuid,
        region -> Varchar,
        price -> Numeric,
        price_currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_usage_prices (id) {
        id -> Uuid,
        product_id -> Uuid,
        metric -> Varchar,
        unit_price -> Numeric,
        currency -> Varchar,
        included_units -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_tax_rules (id) {
        id -> Uuid,
        country -> Varchar,
        name -> Varchar,
        rate_bps -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_inventory (id) {
        id -> Uuid,
        product_id -> Uuid,
        variant_id -> Nullable<Uuid>,
        quantity -> Int8,
        reserved -> Int8,
        low_stock_threshold -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_files (id) {
        id -> Uuid,
        product_id -> Uuid,
        software_id -> Nullable<Uuid>,
        software_version -> Nullable<Varchar>,
        release_id -> Nullable<Uuid>,
        file_id -> Nullable<Uuid>,
        filename -> Nullable<Varchar>,
        checksum_sha256 -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_versions (id) {
        id -> Uuid,
        product_id -> Uuid,
        version -> Varchar,
        software_id -> Nullable<Uuid>,
        release_id -> Nullable<Uuid>,
        changelog -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_tags (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    product_tag_assignments (product_id, tag_id) {
        product_id -> Uuid,
        tag_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
