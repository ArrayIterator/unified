// Diesel schema for the holidays domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Holidays (migration 000104_holidays).
// No FKs by design (see migration notes): no joinable!s.

diesel::table! {
    holiday_providers (id) {
        id -> Text,
        name -> Text,
        kind -> Text,
        base_url -> Text,
        allowed_hosts -> Jsonb,
        api_key_required -> Bool,
        api_key_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Text>,
        min_interval_secs -> Int8,
        max_requests_per_minute -> Int8,
        calendar_ids -> Jsonb,
        enabled -> Bool,
        builtin -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_countries (code) {
        code -> Text,
        name -> Text,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_sync_configs (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Text,
        provider_id -> Text,
        fallback_provider_ids -> Jsonb,
        countries -> Jsonb,
        years -> Jsonb,
        schedule -> Text,
        feed_url -> Nullable<Text>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_sync_runs (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Text,
        config_id -> Uuid,
        provider_id -> Text,
        country -> Text,
        year -> Int8,
        status -> Text,
        trigger -> Text,
        holidays_found -> Int8,
        holidays_stored -> Int8,
        error -> Nullable<Text>,
        started_at -> Timestamptz,
        finished_at -> Timestamptz,
        fetched_at -> Timestamptz,
    }
}

diesel::table! {
    holidays (id) {
        id -> Uuid,
        country -> Text,
        date -> Date,
        name -> Text,
        local_name -> Nullable<Text>,
        provider_id -> Text,
        source -> Text,
        global -> Bool,
        types -> Jsonb,
        counties -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    holiday_overrides (id) {
        id -> Uuid,
        tenant_id -> Text,
        company_id -> Text,
        country -> Text,
        date -> Date,
        name -> Text,
        override_kind -> Text,
        note -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
