// Diesel schema for the storage_plans domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Storage product (migration 000057_storage_product_byo);
// storage product notes sec. 3. No `joinable!` entries (no FKs, per notes).

diesel::table! {
    storage_plans (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Varchar,
        quota_bytes -> Int8,
        max_files -> Int8,
        included_provider_kinds -> Array<Text>,
        product_id -> Nullable<Varchar>,
        billing_cycle -> Nullable<Varchar>,
        is_public -> Bool,
        sort_order -> Int8,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_plan_assignments (id) {
        id -> Uuid,
        plan_id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        status -> Varchar,
        assigned_by -> Varchar,
        assigned_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_usage_metering (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        plan_id -> Nullable<Uuid>,
        period -> Varchar,
        bytes_stored -> Int8,
        files_stored -> Int8,
        reported_by -> Varchar,
        reported_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_connections (id) {
        id -> Uuid,
        name -> Varchar,
        provider_kind -> Varchar,
        s3_compatible -> Bool,
        endpoint -> Varchar,
        region -> Varchar,
        container -> Varchar,
        path -> Varchar,
        sftp_host -> Varchar,
        sftp_port -> Int4,
        sftp_username -> Varchar,
        sftp_auth_kind -> Varchar,
        sftp_keyfile -> Varchar,
        allowed_hosts -> Array<Text>,
        credentials_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        path_prefix -> Varchar,
        owner_user -> Varchar,
        company_id -> Nullable<Varchar>,
        status -> Varchar,
        status_reason -> Nullable<Varchar>,
        allow_read -> Bool,
        allow_write -> Bool,
        allow_delete -> Bool,
        max_bytes -> Nullable<Int8>,
        max_files -> Nullable<Int8>,
        used_bytes -> Int8,
        used_files -> Int8,
        encryption_required -> Bool,
        allowed_extensions -> Array<Text>,
        allowed_mime_types -> Array<Text>,
        terms_version -> Nullable<Varchar>,
        terms_accepted_at -> Nullable<Timestamptz>,
        last_validated_at -> Nullable<Timestamptz>,
        last_validation_ok -> Nullable<Bool>,
        last_validation_error -> Nullable<Varchar>,
        last_health_at -> Nullable<Timestamptz>,
        last_health_ok -> Nullable<Bool>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_terms (id) {
        id -> Uuid,
        version -> Varchar,
        title -> Varchar,
        body -> Text,
        is_current -> Bool,
        reacceptance_deadline -> Nullable<Timestamptz>,
        enabled -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_terms_acceptances (id) {
        id -> Uuid,
        terms_id -> Uuid,
        terms_version -> Varchar,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        connection_id -> Nullable<Uuid>,
        accepted_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_objects (connection_id, key) {
        connection_id -> Uuid,
        key -> Varchar,
        size_bytes -> Int8,
        mime -> Varchar,
        owner_user -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_byo_health_checks (id) {
        id -> Uuid,
        connection_id -> Uuid,
        ok -> Bool,
        latency_ms -> Int8,
        detail -> Varchar,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
