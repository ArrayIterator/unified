// Diesel schema for the licensing domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    license_products (id) {
        id -> Uuid,
        product -> Varchar,
        display_name -> Nullable<Varchar>,
        owner_id -> Nullable<Uuid>,
        key_prefix -> Nullable<Varchar>,
        segments -> Int4,
        key_format -> Varchar,
        key_pattern -> Nullable<Varchar>,
        default_max_activations -> Int4,
        default_validity_days -> Nullable<Int8>,
        grace_period_days -> Int8,
        renewal_reminder_days -> Array<Int8>,
        offline_max_days -> Int8,
        allowed_distribution_channels -> Array<Text>,
        api_allowed_endpoints -> Array<Text>,
        api_rate_limit_per_minute -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    license_distributions (id) {
        id -> Uuid,
        product -> Varchar,
        provider -> Varchar,
        repo -> Varchar,
        git_ref -> Varchar,
        webhook_secret_hash -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    license_releases (id) {
        id -> Uuid,
        product_id -> Uuid,
        product -> Varchar,
        version -> Varchar,
        file_name -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Varchar,
        sha256_hex -> Varchar,
        size_bytes -> Int8,
        download_count -> Int8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    license_events (id) {
        id -> Uuid,
        license_id -> Uuid,
        event -> Varchar,
        actor -> Nullable<Varchar>,
        details -> Jsonb,
        prev_hash -> Varchar,
        hash -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    license_offline_tokens (token_id) {
        token_id -> Uuid,
        license_id -> Uuid,
        product -> Varchar,
        expires_at -> Timestamptz,
        issued_at -> Timestamptz,
        revoked -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    license_activation_history (id) {
        id -> Uuid,
        license_id -> Uuid,
        action -> Varchar,
        actor -> Nullable<Varchar>,
        instance_id -> Varchar,
        hostname -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}
