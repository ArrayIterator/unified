// Diesel schema for the saas_packages domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// SaaS packages (migration 000102_saas_packages).
// Ids are TEXT and there are no FKs by design: no joinable!s.

diesel::table! {
    saas_packages (id) {
        id -> Text,
        slug -> Text,
        name -> Text,
        description -> Text,
        status -> Text,
        current_version -> Int4,
        product_id -> Nullable<Text>,
        preset_settings -> Jsonb,
        wizard_steps -> Jsonb,
        created_by -> Text,
        vendor_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_package_modules (package_id, version, module_id) {
        package_id -> Text,
        version -> Int4,
        module_id -> Text,
        required -> Bool,
        sort_order -> Int4,
    }
}

diesel::table! {
    saas_package_purchases (id) {
        id -> Text,
        package_id -> Text,
        package_version -> Int4,
        buyer_id -> Text,
        company_id -> Text,
        status -> Text,
        subdomain -> Nullable<Text>,
        subdomain_pending -> Bool,
        order_id -> Nullable<Text>,
        subscription_id -> Nullable<Text>,
        product_id -> Nullable<Text>,
        enabled_modules -> Jsonb,
        grace_ends_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Text>,
        provision_attempts -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saas_package_provision_steps (id) {
        id -> Text,
        purchase_id -> Text,
        step -> Text,
        status -> Text,
        outcome -> Nullable<Text>,
        detail -> Nullable<Text>,
        attempt -> Int4,
        started_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
    }
}
