// Diesel schema for the ai_policies domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// AI access allowlists (migration 000036_ai_allowlists; batch 2 notes sec. 8).

diesel::table! {
    ai_company_policies (id) {
        id -> Uuid,
        company_id -> Uuid,
        allowed_models -> Nullable<Jsonb>,
        allowed_features -> Nullable<Jsonb>,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        ai_enabled -> Bool,
        disabled_features -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_user_policies (id) {
        id -> Uuid,
        user_id -> Uuid,
        company_id -> Uuid,
        allowed_models -> Nullable<Jsonb>,
        allowed_features -> Nullable<Jsonb>,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        ai_enabled -> Bool,
        disabled_features -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_personal_keys (id) {
        id -> Uuid,
        user_id -> Uuid,
        provider_kind -> Varchar,
        base_url -> Nullable<Varchar>,
        model -> Nullable<Varchar>,
        api_key_encrypted -> Text,
        key_fingerprint -> Varchar,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_user_mcp_servers (id) {
        id -> Uuid,
        user_id -> Uuid,
        name -> Varchar,
        endpoint_url -> Varchar,
        auth_token_encrypted -> Nullable<Text>,
        token_fingerprint -> Nullable<Varchar>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_accounts (id) {
        id -> Uuid,
        user_id -> Varchar,
        label -> Varchar,
        email -> Varchar,
        is_auditor -> Bool,
        auditor_of -> Varchar,
        status -> Varchar,
        expires_at -> Timestamptz,
        password_hash -> Varchar,
        revoked_at -> Nullable<Timestamptz>,
        revoked_by -> Nullable<Varchar>,
        revoke_reason -> Nullable<Text>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_scopes (id) {
        id -> Uuid,
        auditor_id -> Uuid,
        module -> Varchar,
        resource_kind -> Varchar,
        resource_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        allow_export -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_sessions (id) {
        id -> Uuid,
        auditor_id -> Uuid,
        user_id -> Varchar,
        started_at -> Timestamptz,
        last_seen_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        revoked_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// AI gating (migration 000064_ai_gating; batch 5 notes sec. c).

diesel::table! {
    ai_global_settings (key) {
        key -> Varchar,
        enabled -> Bool,
        updated_at -> Timestamptz,
    }
}
