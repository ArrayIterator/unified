// Diesel schema for the contracts domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Contracts (migration 000048_contracts). `Contract` carries a
// NUMERIC column, so it skips ALL serde derives per the repo header
// rule. The self-reference `contracts.renewed_from_id` gets no
// `joinable!` (a table cannot join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    contract_templates (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        body -> Text,
        placeholders -> Array<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    contracts (id) {
        id -> Uuid,
        template_id -> Nullable<Uuid>,
        title -> Varchar,
        company_party -> Varchar,
        counterparty_name -> Varchar,
        counterparty_email -> Varchar,
        terms -> Text,
        value -> Numeric,
        currency -> Bpchar,
        signing_order -> Varchar,
        status -> Varchar,
        starts_at -> Nullable<Timestamptz>,
        ends_at -> Nullable<Timestamptz>,
        signed_at -> Nullable<Timestamptz>,
        content_hash -> Nullable<Varchar>,
        void_reason -> Nullable<Text>,
        terminate_reason -> Nullable<Text>,
        renewed_from_id -> Nullable<Uuid>,
        alert_days -> Array<Int4>,
        expiry_alert_status -> Varchar,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    contract_signers (id) {
        id -> Uuid,
        contract_id -> Uuid,
        name -> Varchar,
        email -> Varchar,
        order_index -> Int4,
        status -> Varchar,
        token_hash -> Varchar,
        token_expires_at -> Timestamptz,
        signed_at -> Nullable<Timestamptz>,
        declined_reason -> Nullable<Text>,
        viewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    contract_audit_trail (id) {
        id -> Uuid,
        contract_id -> Uuid,
        signer_id -> Nullable<Uuid>,
        actor -> Varchar,
        action -> Varchar,
        ip_hash -> Varchar,
        user_agent_hash -> Varchar,
        details -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    auto_open_rules (service_id) {
        service_id -> Varchar,
        enabled -> Bool,
        severity -> Varchar,
        auto_resolve_on_recovery -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
