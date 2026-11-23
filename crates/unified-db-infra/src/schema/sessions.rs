// Diesel schema for the sessions domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    export_schedules (id) {
        id -> Uuid,
        name -> Varchar,
        entity -> Varchar,
        format -> Varchar,
        fields -> Jsonb,
        filters -> Jsonb,
        redact_pii -> Bool,
        redact_names -> Bool,
        pii_allowed -> Bool,
        schedule -> Varchar,
        enabled -> Bool,
        company_id -> Nullable<Uuid>,
        actor -> Varchar,
        last_run_at -> Nullable<Timestamptz>,
        last_export_id -> Nullable<Uuid>,
        last_error -> Nullable<Text>,
        run_count -> Int8,
        scheduling -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    import_sessions (id) {
        id -> Uuid,
        entity -> Varchar,
        format -> Varchar,
        status -> Varchar,
        total_chunks -> Nullable<Int4>,
        received_chunks -> Jsonb,
        total_rows -> Int8,
        valid_rows -> Int8,
        invalid_rows -> Int8,
        row_errors -> Jsonb,
        mapping -> Jsonb,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
