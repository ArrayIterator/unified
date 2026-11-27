// web3 -- Chain, record, request and provider-facing value types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Chain kind

/// The chain family a network belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainKind {
    Evm,
    Stellar,
    Cardano,
    Generic,
}

impl ChainKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Evm => "evm",
            Self::Stellar => "stellar",
            Self::Cardano => "cardano",
            Self::Generic => "generic",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "evm" => Some(Self::Evm),
            "stellar" => Some(Self::Stellar),
            "cardano" => Some(Self::Cardano),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }
}

// Records (in-memory store; persistence targets in migration 000066)

/// One configured chain network. `api_key_encrypted` is write-only:
/// it is never serialized, never logged, and only its presence is
/// exposed through `api_key_configured`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainNetwork {
    pub id: String,
    pub name: String,
    pub kind: ChainKind,
    pub endpoint_url: String,
    pub chain_id: Option<u64>,
    pub native_asset_symbol: String,
    pub explorer_url_template: Option<String>,
    pub enabled: bool,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    #[serde(skip_serializing, default)]
    pub api_key_encrypted: Option<String>,
    #[serde(default)]
    pub api_key_configured: bool,
    /// True only when an admin explicitly allowed a localhost or
    /// private endpoint for this network (audit-logged on creation
    /// and when newly allowed by an update).
    pub allow_private_endpoint: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One address watched for incoming and outgoing transactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedAddress {
    pub id: String,
    pub network_id: String,
    pub address: String,
    pub label: Option<String>,
    pub user_id: String,
    pub company_id: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One observed on-chain transaction, deduplicated by
/// (`network_id`, `tx_hash`). Amounts are string decimals: `amount_raw`
/// is the exact chain-native amount (wei, stroops, lovelace, or the
/// asset's smallest unit) and `amount` is the human-readable decimal
/// derived from it when the asset decimals are known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainTxRecord {
    pub id: String,
    pub network_id: String,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub amount: Option<String>,
    pub asset: String,
    /// `pending`, `confirmed`, or `failed`.
    pub status: String,
    pub confirmations: i64,
    pub block_number: Option<u64>,
    /// `incoming`, `outgoing`, or `observed`, relative to the watched
    /// address that found the transaction.
    pub direction: String,
    pub watched_address_id: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One on-chain anchor: only the SHA-256 hash of a Unified record is
/// anchored. Record contents never leave the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRecord {
    pub id: String,
    pub record_type: String,
    pub record_id: String,
    pub content_sha256: String,
    pub network_id: String,
    /// `pending`, `submitted`, `confirmed`, or `failed`.
    pub status: String,
    pub tx_hash: Option<String>,
    pub confirmations: i64,
    pub block_number: Option<u64>,
    pub requested_by: String,
    pub company_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One read-only wallet link. The server can observe this address,
/// never spend from it. `challenge_nonce_hash` is the SHA-256 of the
/// outstanding challenge nonce; the raw nonce lives only in the
/// in-memory challenge store and is never persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletLink {
    pub id: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub network_id: String,
    pub address: String,
    pub label: Option<String>,
    /// `unverified`, `pending`, `verified`, `lookup_only`, or `failed`.
    pub verification_status: String,
    #[serde(skip_serializing, default)]
    pub challenge_nonce_hash: Option<String>,
    pub challenge_expires_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One balance snapshot for a linked wallet, refreshed on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub id: String,
    pub wallet_id: String,
    pub network_id: String,
    pub address: String,
    pub asset: String,
    pub amount_raw: String,
    pub amount: Option<String>,
    pub decimals: u32,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Module settings. The anchoring service key is write-only
/// (encrypted, never serialized); only its presence is exposed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Web3Settings {
    /// Default-deny gate for the public ledger endpoint.
    pub public_ledger_enabled: bool,
    /// Extra hosts an admin allowlisted for provider endpoints.
    pub allowed_private_hosts: Vec<String>,
    /// Extra hosts an admin allowlisted for provider endpoints.
    pub allowed_extra_hosts: Vec<String>,
    /// Allowlisted anchoring relay URL, or `None` when anchoring
    /// submission is not configured.
    pub anchoring_relay_url: Option<String>,
    #[serde(skip_serializing, default)]
    pub anchoring_key_encrypted: Option<String>,
    #[serde(default)]
    pub anchoring_key_configured: bool,
    pub max_poll_transactions: u32,
}

impl Default for Web3Settings {
    fn default() -> Self {
        Self {
            public_ledger_enabled: false,
            allowed_private_hosts: Vec::new(),
            allowed_extra_hosts: Vec::new(),
            anchoring_relay_url: None,
            anchoring_key_encrypted: None,
            anchoring_key_configured: false,
            max_poll_transactions: 25,
        }
    }
}

/// One outstanding wallet-verification challenge (in-memory only).
#[derive(Debug, Clone)]
pub(crate) struct WalletChallenge {
    pub(crate) wallet_id: String,
    pub(crate) nonce: String,
    pub(crate) message: String,
    pub(crate) expires_at: DateTime<Utc>,
}

/// One public ledger entry: what was anchored, when, on which chain,
/// and its verification status. Record contents are never included.
#[derive(Debug, Clone, Serialize)]
pub struct LedgerEntry {
    pub anchor_id: String,
    pub record_type: String,
    pub record_id: String,
    pub network_id: String,
    pub tx_hash: Option<String>,
    pub status: String,
    pub confirmations: i64,
    pub anchored_at: DateTime<Utc>,
    pub content_sha256: String,
}

// Request bodies

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNetworkRequest {
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub chain_id: Option<u64>,
    pub native_asset_symbol: String,
    pub explorer_url_template: Option<String>,
    pub enabled: Option<bool>,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    /// Provider API key (Blockfrost, indexer, or verification
    /// endpoint key). Write-only: stored encrypted, never returned.
    pub api_key: Option<String>,
    pub allow_private_endpoint: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateNetworkRequest {
    pub name: Option<String>,
    pub endpoint_url: Option<String>,
    pub chain_id: Option<u64>,
    pub native_asset_symbol: Option<String>,
    pub explorer_url_template: Option<String>,
    pub enabled: Option<bool>,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    /// When present, replaces the stored provider API key. An empty
    /// string clears it.
    pub api_key: Option<String>,
    pub allow_private_endpoint: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateWatchedAddressRequest {
    pub network_id: String,
    pub address: String,
    pub label: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAnchorRequest {
    pub record_type: String,
    pub record_id: String,
    /// SHA-256 hex of the record content, or supply `content` and let
    /// the server hash it. At least one is required.
    pub content_sha256: Option<String>,
    /// Raw record content to hash server-side. The content itself is
    /// never stored and never sent on-chain.
    pub content: Option<String>,
    pub network_id: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerifyAnchorRequest {
    pub record_type: String,
    pub record_id: String,
    pub content_sha256: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinkWalletRequest {
    pub network_id: String,
    pub address: String,
    pub label: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerifyWalletRequest {
    /// EVM: the wallet signature of the challenge message.
    pub signature: Option<String>,
    /// Stellar: the hash of the transaction whose text memo carries
    /// the 28-character memo token derived from the challenge nonce
    /// (the full nonce cannot fit in a Stellar text memo).
    pub proof_tx_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSettingsRequest {
    pub public_ledger_enabled: Option<bool>,
    pub allowed_private_hosts: Option<Vec<String>>,
    pub allowed_extra_hosts: Option<Vec<String>>,
    pub anchoring_relay_url: Option<String>,
    /// Dedicated anchoring-only service key. Write-only: stored
    /// encrypted, never returned. An empty string clears it.
    pub anchoring_key: Option<String>,
    pub max_poll_transactions: Option<u32>,
}

// Provider-facing value types

/// Chain status as reported by the provider.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    pub network_id: String,
    pub kind: String,
    pub latest_block: Option<u64>,
    pub syncing: Option<bool>,
    pub provider_version: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// A balance as reported by the provider.
#[derive(Debug, Clone, Serialize)]
pub struct ChainBalance {
    pub address: String,
    pub asset: String,
    pub amount_raw: String,
    pub decimals: u32,
    pub checked_at: DateTime<Utc>,
}

/// One transaction as reported by the provider.
#[derive(Debug, Clone)]
pub struct ProviderTransaction {
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub asset: String,
    pub status: String,
    pub confirmations: i64,
    pub block_number: Option<u64>,
    pub occurred_at: Option<DateTime<Utc>>,
}

/// A fee estimate as reported by the provider.
#[derive(Debug, Clone, Serialize)]
pub struct FeeEstimate {
    pub network_id: String,
    pub fee_raw: String,
    pub fee_unit: String,
    pub detail: String,
    pub estimated_at: DateTime<Utc>,
}

/// The outcome of an ownership verification attempt.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationOutcome {
    pub verified: bool,
    /// `signature`, `memo_transaction`, or `address_lookup`.
    pub method: String,
    pub detail: String,
}
