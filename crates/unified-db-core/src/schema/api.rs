// Diesel schema for the api domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// API Management (2026-11-22-000004_api_management).

diesel::table! {
    api_versions (id) {
        id -> Varchar,
        number -> Int4,
        enabled -> Bool,
        deprecated -> Bool,
        sunset_date -> Nullable<Timestamptz>,
        rate_limit_per_minute -> Int4,
        auth_required -> Bool,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_endpoints (id) {
        id -> Uuid,
        method -> Varchar,
        path -> Varchar,
        version -> Varchar,
        description -> Text,
        rate_limit -> Int4,
        enabled -> Bool,
        rules -> Jsonb,
        owner_module -> Varchar,
        auth_required -> Bool,
        source -> Varchar,
        version_support -> Jsonb,
        execution_count -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_prefix_settings (key) {
        key -> Varchar,
        prefix -> Varchar,
        migrate_internal -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_keys (id) {
        id -> Uuid,
        name -> Varchar,
        owner_id -> Nullable<Uuid>,
        prefix -> Varchar,
        last4 -> Varchar,
        key_hash -> Varchar,
        scopes -> Jsonb,
        rate_tier -> Varchar,
        requests_per_minute -> Int4,
        status -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        grace_until -> Nullable<Timestamptz>,
        rotated_from -> Nullable<Uuid>,
        rotated_to -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        last_used_at -> Nullable<Timestamptz>,
        usage_count -> Int8,
    }
}

diesel::table! {
    api_access_rules (id) {
        id -> Uuid,
        method -> Varchar,
        path -> Varchar,
        version -> Nullable<Varchar>,
        allowed -> Bool,
        public_access -> Bool,
        actor -> Varchar,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    api_access_audit (id) {
        id -> Uuid,
        actor -> Varchar,
        action -> Varchar,
        target -> Varchar,
        detail -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
