// Diesel schema for the web3 domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Web3 (migration 000067_web3). `Web3Transaction` and `Web3Balance`
// carry NUMERIC columns, so they skip ALL serde derives per the
// repo header rule. `api_key_encrypted` / `anchoring_key_encrypted`
// are write-only ciphertext and get `#[serde(skip_serializing)]`
// on the read structs.
// ---------------------------------------------------------------------------

diesel::table! {
    web3_networks (id) {
        id -> Uuid,
        name -> Text,
        kind -> Text,
        endpoint_url -> Text,
        chain_id -> Nullable<Int8>,
        native_asset_symbol -> Text,
        explorer_url_template -> Nullable<Text>,
        enabled -> Bool,
        indexer_url -> Nullable<Text>,
        verify_endpoint -> Nullable<Text>,
        api_key_encrypted -> Nullable<Text>,
        allow_private_endpoint -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_watched_addresses (id) {
        id -> Uuid,
        network_id -> Uuid,
        address -> Text,
        label -> Nullable<Text>,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        last_checked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_transactions (id) {
        id -> Uuid,
        network_id -> Uuid,
        tx_hash -> Text,
        from_address -> Text,
        to_address -> Text,
        amount_raw -> Text,
        amount -> Nullable<Numeric>,
        asset -> Text,
        status -> Text,
        confirmations -> Int8,
        block_number -> Nullable<Int8>,
        direction -> Text,
        watched_address_id -> Nullable<Uuid>,
        occurred_at -> Nullable<Timestamptz>,
        first_seen_at -> Timestamptz,
        last_checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_anchors (id) {
        id -> Uuid,
        record_type -> Text,
        record_id -> Text,
        content_sha256 -> Text,
        network_id -> Uuid,
        status -> Text,
        tx_hash -> Nullable<Text>,
        confirmations -> Int8,
        block_number -> Nullable<Int8>,
        requested_by -> Text,
        company_id -> Nullable<Uuid>,
        submitted_at -> Nullable<Timestamptz>,
        confirmed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_wallets (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        network_id -> Uuid,
        address -> Text,
        label -> Nullable<Text>,
        verification_status -> Text,
        challenge_nonce_hash -> Nullable<Text>,
        challenge_expires_at -> Nullable<Timestamptz>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_balances (id) {
        id -> Uuid,
        wallet_id -> Uuid,
        network_id -> Uuid,
        address -> Text,
        asset -> Text,
        amount_raw -> Text,
        amount -> Nullable<Numeric>,
        decimals -> Int4,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    web3_settings (id) {
        id -> Uuid,
        scope -> Text,
        public_ledger_enabled -> Bool,
        allowed_private_hosts -> Array<Text>,
        allowed_extra_hosts -> Array<Text>,
        anchoring_relay_url -> Nullable<Text>,
        anchoring_key_encrypted -> Nullable<Text>,
        max_poll_transactions -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
