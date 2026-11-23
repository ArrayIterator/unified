// Diesel schema for the domain_verification domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    domain_verification_settings (id) {
        id -> Uuid,
        version -> Int8,
        record_type -> Text,
        host_template -> Text,
        value_template -> Text,
        token_length -> Int4,
        token_alphabet -> Text,
        verify_timeout_seconds -> Int4,
        max_attempts -> Int4,
        retry_delay_seconds -> Int4,
        token_ttl_seconds -> Int8,
        rate_limit_window_seconds -> Int8,
        updated_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    domain_verification_tokens (id) {
        id -> Uuid,
        domain -> Text,
        alias_id -> Nullable<Uuid>,
        tenant_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        token_hash -> Text,
        record_type -> Text,
        host -> Text,
        expected_value -> Text,
        settings_version -> Int8,
        status -> Text,
        issued_at -> Timestamptz,
        expires_at -> Timestamptz,
        verified_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    domain_verification_attempts (id) {
        id -> Uuid,
        token_id -> Uuid,
        domain -> Text,
        alias_id -> Nullable<Uuid>,
        attempt_number -> Int4,
        result -> Text,
        record_found -> Nullable<Text>,
        looked_up -> Jsonb,
        error -> Nullable<Text>,
        checked_at -> Timestamptz,
    }
}

diesel::table! {
    domain_cert_modes (id) {
        id -> Uuid,
        domain -> Varchar,
        mode -> Varchar,
        provider -> Varchar,
        fingerprint_sha256 -> Varchar,
        installed_at -> Nullable<Timestamptz>,
        previous_fingerprint -> Nullable<Varchar>,
        status -> Varchar,
        last_error -> Nullable<Text>,
        updated_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
