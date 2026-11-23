// Diesel models for the web3 domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Web3 (migration 000067_web3).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_networks)]
pub struct Web3Network {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub chain_id: Option<i64>,
    pub native_asset_symbol: String,
    pub explorer_url_template: Option<String>,
    pub enabled: bool,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub allow_private_endpoint: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_networks)]
pub struct NewWeb3Network {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub chain_id: Option<i64>,
    pub native_asset_symbol: String,
    pub explorer_url_template: Option<String>,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    pub api_key_encrypted: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_watched_addresses)]
pub struct Web3WatchedAddress {
    pub id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_watched_addresses)]
pub struct NewWeb3WatchedAddress {
    pub id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = web3_transactions)]
pub struct Web3Transaction {
    pub id: Uuid,
    pub network_id: Uuid,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
    pub asset: String,
    pub status: String,
    pub confirmations: i64,
    pub block_number: Option<i64>,
    pub direction: String,
    pub watched_address_id: Option<Uuid>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = web3_transactions)]
pub struct NewWeb3Transaction {
    pub id: Uuid,
    pub network_id: Uuid,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
    pub asset: String,
    pub watched_address_id: Option<Uuid>,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_anchors)]
pub struct Web3Anchor {
    pub id: Uuid,
    pub record_type: String,
    pub record_id: String,
    pub content_sha256: String,
    pub network_id: Uuid,
    pub status: String,
    pub tx_hash: Option<String>,
    pub confirmations: i64,
    pub block_number: Option<i64>,
    pub requested_by: String,
    pub company_id: Option<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_anchors)]
pub struct NewWeb3Anchor {
    pub id: Uuid,
    pub record_type: String,
    pub record_id: String,
    pub content_sha256: String,
    pub network_id: Uuid,
    pub tx_hash: Option<String>,
    pub requested_by: String,
    pub company_id: Option<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_wallets)]
pub struct Web3Wallet {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub verification_status: String,
    pub challenge_nonce_hash: Option<String>,
    pub challenge_expires_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_wallets)]
pub struct NewWeb3Wallet {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub challenge_nonce_hash: Option<String>,
    pub challenge_expires_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = web3_balances)]
pub struct Web3Balance {
    pub id: Uuid,
    pub wallet_id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub asset: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
    pub decimals: i32,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = web3_balances)]
pub struct NewWeb3Balance {
    pub id: Uuid,
    pub wallet_id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub asset: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_settings)]
pub struct Web3Setting {
    pub id: Uuid,
    pub scope: String,
    pub public_ledger_enabled: bool,
    pub allowed_private_hosts: Vec<String>,
    pub allowed_extra_hosts: Vec<String>,
    pub anchoring_relay_url: Option<String>,
    #[serde(skip_serializing)]
    pub anchoring_key_encrypted: Option<String>,
    pub max_poll_transactions: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_settings)]
pub struct NewWeb3Setting {
    pub id: Uuid,
    pub anchoring_relay_url: Option<String>,
    pub anchoring_key_encrypted: Option<String>,
}
