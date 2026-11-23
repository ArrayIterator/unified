// Diesel schema for the jobs domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    import_jobs (id) {
        id -> Uuid,
        entity -> Varchar,
        format -> Varchar,
        status -> Varchar,
        total_rows -> Int8,
        valid_rows -> Int8,
        invalid_rows -> Int8,
        row_errors -> Jsonb,
        results -> Jsonb,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    export_jobs (id) {
        id -> Uuid,
        entity -> Varchar,
        format -> Varchar,
        status -> Varchar,
        fields -> Jsonb,
        row_count -> Int8,
        data_provider -> Varchar,
        download_url -> Varchar,
        error -> Nullable<Text>,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        pii_redacted -> Bool,
        redacted_fields -> Jsonb,
        row_limit -> Int8,
        row_offset -> Int8,
        has_more -> Bool,
        rows_excluded_by_tenant_scope -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
