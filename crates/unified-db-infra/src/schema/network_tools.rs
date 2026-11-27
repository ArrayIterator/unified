// Diesel schema for the network_tools domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Network Tools (migration 000070_network_tools) + Licensing SaaS
// (migration 000069_licensing_saas); batch 7 notes sec. 2. The notes
// write `BigInt`; normalised to the file's `Int8` style.

diesel::table! {
    network_tool_saved_checks (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        name -> Text,
        tool -> Text,
        target -> Text,
        parameters -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_runs (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        tool -> Text,
        target -> Text,
        duration_ms -> Int8,
        outcome -> Text,
        summary -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_uptime_monitors (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        name -> Text,
        url -> Text,
        interval_seconds -> Int4,
        expected_status -> Int4,
        failure_threshold -> Int4,
        consecutive_failures -> Int4,
        enabled -> Bool,
        last_status -> Nullable<Text>,
        last_checked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_uptime_events (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        status -> Text,
        http_status -> Nullable<Int4>,
        latency_ms -> Nullable<Int8>,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    network_tool_cert_watches (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        domain -> Text,
        enabled -> Bool,
        last_days_remaining -> Nullable<Int4>,
        last_checked_at -> Nullable<Timestamptz>,
        alerted_thresholds -> Array<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
