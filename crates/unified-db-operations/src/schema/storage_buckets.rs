// Diesel schema for the storage_buckets domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Storage manager (migration 000033_storage_manager; batch 2 notes sec. 5).

diesel::table! {
    storage_buckets (id) {
        id -> Uuid,
        name -> Varchar,
        provider_kind -> Varchar,
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
        encryption_at_rest -> Bool,
        enabled -> Bool,
        company_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_bindings (id) {
        id -> Uuid,
        module_id -> Varchar,
        bucket_id -> Uuid,
        key_prefix -> Varchar,
        company_id -> Nullable<Varchar>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_quotas (id) {
        id -> Uuid,
        scope -> Varchar,
        scope_id -> Varchar,
        bytes_limit -> Nullable<Int8>,
        objects_limit -> Nullable<Int8>,
        soft_bytes_limit -> Nullable<Int8>,
        soft_objects_limit -> Nullable<Int8>,
        used_bytes -> Int8,
        used_objects -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_usage_daily (id) {
        id -> Uuid,
        date -> Date,
        bucket_id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        module_id -> Varchar,
        bytes_total -> Int8,
        objects_total -> Int8,
        bytes_delta -> Int8,
        objects_delta -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_lifecycle_rules (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        retention_days -> Int8,
        auto_delete -> Bool,
        archive_transition -> Bool,
        archive_prefix -> Varchar,
        enabled -> Bool,
        last_run_at -> Nullable<Timestamptz>,
        last_run_summary -> Nullable<Text>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_signed_url_revocations (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        key -> Varchar,
        issued_by -> Varchar,
        expires_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        revoke_reason -> Nullable<Varchar>,
        provider_url_issued -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
