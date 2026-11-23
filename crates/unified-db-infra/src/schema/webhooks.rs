// Diesel schema for the webhooks domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Webhooks, import, and export (2026-11-22-000018_webhooks_import_export).
// ---------------------------------------------------------------------------

diesel::table! {
    webhook_endpoints (id) {
        id -> Uuid,
        name -> Varchar,
        url -> Varchar,
        events -> Jsonb,
        secret -> Varchar,
        active -> Bool,
        paused -> Bool,
        max_attempts -> Int4,
        backoff_base_secs -> Int8,
        timeout_secs -> Int8,
        delivery_count -> Int8,
        last_delivery_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    webhook_deliveries (id) {
        id -> Uuid,
        endpoint_id -> Uuid,
        event -> Varchar,
        payload -> Jsonb,
        payload_hash -> Varchar,
        status -> Varchar,
        attempts -> Int4,
        max_attempts -> Int4,
        next_retry_at -> Nullable<Timestamptz>,
        status_code -> Nullable<Int4>,
        duration_ms -> Nullable<Int8>,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        delivered_at -> Nullable<Timestamptz>,
    }
}
