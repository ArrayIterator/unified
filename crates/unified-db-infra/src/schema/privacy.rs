// Diesel schema for the privacy domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Privacy (migration 000066_privacy).
// ---------------------------------------------------------------------------

diesel::table! {
    privacy_field_declarations (id) {
        id -> Uuid,
        module_id -> Text,
        field_name -> Text,
        purpose -> Text,
        required -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_anonymizations (id) {
        id -> Uuid,
        user_id -> Text,
        token -> Text,
        field_tokens -> Jsonb,
        scope_modules -> Array<Text>,
        reason -> Nullable<Text>,
        actor -> Text,
        times_applied -> Int8,
        anonymized_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_visibility_defaults (id) {
        id -> Uuid,
        module_id -> Text,
        visibility -> Text,
        explicit -> Bool,
        configured_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_consents (id) {
        id -> Uuid,
        user_id -> Text,
        consent_type -> Text,
        granted -> Bool,
        granted_at -> Nullable<Timestamptz>,
        withdrawn_at -> Nullable<Timestamptz>,
        expires_at -> Nullable<Timestamptz>,
        source -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_consent_history (id) {
        id -> Uuid,
        consent_id -> Uuid,
        user_id -> Text,
        consent_type -> Text,
        action -> Text,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_erasure_requests (id) {
        id -> Uuid,
        user_id -> Text,
        modules -> Array<Text>,
        reason -> Text,
        status -> Text,
        requested_by -> Text,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        rejection_reason -> Nullable<Text>,
        fulfilled_at -> Nullable<Timestamptz>,
        anonymization_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_notices (id) {
        id -> Uuid,
        module_id -> Text,
        text -> Text,
        version -> Int8,
        effective_from -> Timestamptz,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    privacy_audit_entries (id) {
        id -> Uuid,
        actor -> Text,
        action -> Text,
        resource -> Text,
        details -> Jsonb,
        created_at -> Timestamptz,
    }
}
