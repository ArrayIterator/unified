// Diesel schema for the hosting domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Dashboards and reseller links (2026-11-22-000006_dashboards_reseller).

diesel::table! {
    reseller_links (reseller_id, client_id) {
        reseller_id -> Uuid,
        client_id -> Uuid,
        verified -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    view_as_sessions (admin_id) {
        admin_id -> Uuid,
        target_user_id -> Uuid,
        reason -> Text,
        started_at -> Timestamptz,
        expires_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    dashboard_settings (key) {
        key -> Varchar,
        admin_prefix -> Varchar,
        user_prefix -> Varchar,
        view_as_ttl_minutes -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// Hosting (2026-11-22-000007_hosting).

diesel::table! {
    hosting_plans (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        disk_gb -> Int8,
        cpu_cores -> Float8,
        ram_mb -> Int8,
        bandwidth_gb -> Int8,
        price -> Numeric,
        currency -> Varchar,
        billing_cycle -> Varchar,
        store_product_id -> Nullable<Varchar>,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hosting_accounts (id) {
        id -> Uuid,
        plan_id -> Uuid,
        domain -> Varchar,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        ssl_state -> Varchar,
        disk_used_mb -> Int8,
        bandwidth_used_mb -> Int8,
        store_order_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    hosting_order_claims (order_id, plan_id) {
        order_id -> Varchar,
        plan_id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
