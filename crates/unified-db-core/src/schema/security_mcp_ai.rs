// Diesel schema for the security_mcp_ai domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    auth_recovery_codes (user_id, code_hash) {
        user_id -> Varchar,
        code_hash -> Varchar,
        used -> Bool,
        created_at -> Timestamptz,
        used_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Security (2026-11-22-000020_security).
// ---------------------------------------------------------------------------

diesel::table! {
    security_ip_lists (list, cidr) {
        list -> Varchar,
        cidr -> Varchar,
        network -> Varchar,
        prefix -> Int4,
        added_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    security_reputation (ip) {
        ip -> Varchar,
        score -> Int4,
        flagged -> Bool,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    security_failed_auth_clusters (key) {
        key -> Varchar,
        ip -> Nullable<Varchar>,
        account -> Nullable<Varchar>,
        count -> Int8,
        first_seen -> Timestamptz,
        last_seen -> Timestamptz,
        locked -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// MCP servers (2026-11-22-000029_mcp).
// ---------------------------------------------------------------------------

diesel::table! {
    mcp_servers (id) {
        id -> Uuid,
        slug -> Varchar,
        name -> Varchar,
        owner_id -> Uuid,
        company_id -> Nullable<Uuid>,
        site_id -> Nullable<Uuid>,
        product_id -> Nullable<Uuid>,
        status -> Varchar,
        is_public -> Bool,
        allowed_tools -> Jsonb,
        rate_limit_per_minute -> Int4,
        read_only_role -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    mcp_settings (key) {
        key -> Varchar,
        read_only_role -> Nullable<Varchar>,
        default_rate_limit_per_minute -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// AI (2026-11-22-000030_ai).
// ---------------------------------------------------------------------------

diesel::table! {
    ai_provider_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        base_url -> Varchar,
        model -> Varchar,
        api_key_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        enabled -> Bool,
        rate_limit_per_minute -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_usage_records (id) {
        id -> Uuid,
        provider_id -> Uuid,
        provider_kind -> Varchar,
        actor -> Varchar,
        use_case -> Varchar,
        model -> Varchar,
        prompt_tokens -> Nullable<Int4>,
        completion_tokens -> Nullable<Int4>,
        total_tokens -> Nullable<Int4>,
        estimated_cost_usd -> Nullable<Float8>,
        latency_ms -> Int8,
        status -> Varchar,
        error_code -> Nullable<Int4>,
        user_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        denial_reason -> Nullable<Varchar>,
        used_personal_key -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_drafts (id) {
        id -> Uuid,
        kind -> Varchar,
        title -> Nullable<Varchar>,
        subject -> Nullable<Varchar>,
        body -> Text,
        prompt -> Nullable<Text>,
        provider_id -> Nullable<Uuid>,
        provider_kind -> Varchar,
        model -> Varchar,
        actor -> Varchar,
        status -> Varchar,
        ai_generated -> Bool,
        target_type -> Nullable<Varchar>,
        target_id -> Nullable<Varchar>,
        sources -> Jsonb,
        usage_id -> Nullable<Uuid>,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
