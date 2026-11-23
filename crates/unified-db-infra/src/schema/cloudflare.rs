// Diesel schema for the cloudflare domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Cloudflare (2026-11-22-000022_cloudflare).
// ---------------------------------------------------------------------------

diesel::table! {
    cloudflare_settings (key) {
        key -> Varchar,
        api_token_encrypted -> Text,
        account_id -> Nullable<Varchar>,
        default_zone_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cloudflare_purge_history (id) {
        id -> Uuid,
        zone_id -> Varchar,
        kind -> Varchar,
        targets -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cloudflare_detections (id) {
        id -> Uuid,
        domain -> Varchar,
        detected -> Bool,
        methods -> Jsonb,
        ssl_mode -> Nullable<Varchar>,
        end_to_end_encrypted -> Nullable<Bool>,
        flexible_warning -> Nullable<Text>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    cloudflare_ip_ranges (key) {
        key -> Varchar,
        v4 -> Jsonb,
        v6 -> Jsonb,
        fetched_at -> Nullable<Timestamptz>,
        source_v4 -> Varchar,
        source_v6 -> Varchar,
        fetch_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
