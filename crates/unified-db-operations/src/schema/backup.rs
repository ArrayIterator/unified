// Diesel schema for the backup domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Backup and storage targets (2026-11-22-000009_backup_storage).
// ---------------------------------------------------------------------------

diesel::table! {
    backup_jobs (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        database_name -> Varchar,
        paths -> Jsonb,
        config_keys -> Jsonb,
        target -> Varchar,
        schedule -> Varchar,
        retention_keep_last -> Int4,
        retention_keep_days -> Int4,
        enabled -> Bool,
        scheduling -> Jsonb,
        status -> Varchar,
        checksum -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        storage_key -> Nullable<Varchar>,
        last_run_id -> Nullable<Uuid>,
        started_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    backup_runs (id) {
        id -> Uuid,
        job_id -> Nullable<Uuid>,
        job_name -> Varchar,
        kind -> Varchar,
        target -> Varchar,
        storage_key -> Nullable<Varchar>,
        status -> Varchar,
        checksum -> Nullable<Varchar>,
        size_bytes -> Nullable<Int8>,
        file_count -> Nullable<Int8>,
        started_at -> Timestamptz,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        verified -> Nullable<Bool>,
        last_verified_at -> Nullable<Timestamptz>,
        manifest_verified -> Nullable<Bool>,
        verification_error -> Nullable<Text>,
        last_drill_at -> Nullable<Timestamptz>,
        drill_verified -> Nullable<Bool>,
        encryption -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
