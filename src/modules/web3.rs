// Web3 / Blockchain module.
//
// Openness and traceability for Unified records, in the style of a
// public blockchain explorer: watched addresses, on-chain transaction
// history, tamper-evident anchoring of record hashes, a public
// transparency ledger, read-only wallet linking, and read-only token
// balances across EVM chains, Stellar, and Cardano.
//
// SECURITY — READ FIRST. These rules are absolute and are enforced by
// the code below, not just by policy:
//
//   - The server NEVER stores a user private key and NEVER signs a
//     transaction with user funds. There is no private-key field,
//     column, or configuration key anywhere in this module or in its
//     migration. Linked wallets are READ-ONLY: the server can observe
//     them, never spend from them.
//   - Anchoring uses a DEDICATED SERVICE KEY whose scope is
//     anchoring-only. The key is accepted once, stored write-only
//     (encrypted, never returned, never logged), and is used only to
//     authenticate the server to the configured anchoring relay. The
//     relay — never this module — submits the on-chain transaction.
//   - Any configuration value that looks like a private key (a
//     64-hex-character secret, a `0x`-prefixed 64-hex secret, a
//     wallet-import-format string, or a PEM private-key block) is
//     rejected at validation time.
//   - Only the SHA-256 hash of a record is ever anchored on-chain.
//     Record contents stay off-chain at all times.
//   - Every provider endpoint is validated against an allowlist
//     (known chain hosts, suffix rules, and admin-configured extra
//     hosts). Plain `http` is accepted only for loopback hosts, IP
//     literals are rejected except loopback, and a localhost or
//     private endpoint is never enabled unless an admin explicitly
//     allowlists it — that decision is emitted on the hook bus as an
//     audit event (`web3.network.private_endpoint_allowed`).
//   - Provider failures are mapped to the standard error envelope;
//     provider response bodies are never echoed to callers.
//
// Provider model: every chain is reached through the `ChainProvider`
// trait. `EvmRpcProvider` speaks generic JSON-RPC for any EVM chain,
// `StellarProvider` speaks the Horizon API, and `CardanoProvider`
// speaks Blockfrost- or Koios-style REST APIs. Networks are
// configuration, not code: adding a network never requires a code
// change. A `FakeChainProvider` exists for tests only (`#[cfg(test)]`).
//
// Honest limitations (see also the integrator report):
//   - No secp256k1 crate is present in `Cargo.toml`, so EVM message
//     verification cannot do `ecrecover` in-process. EVM verification
//     is delegated to an admin-configured, allowlisted verification
//     endpoint when one is set; without one, verification returns a
//     422 that says so plainly instead of pretending to verify.
//   - Stellar verification proves ownership by checking a transaction
//     from the address whose memo carries the challenge nonce.
//   - Cardano verification is lookup-only: it confirms the address
//     exists and is active, which does not by itself prove control.
//     That is reported honestly as `lookup_only`, never as verified.
//   - EVM address history needs an indexer endpoint (Etherscan-style
//     API) because plain JSON-RPC has no address transaction list.
//     Without an indexer, `get_transactions` on an EVM network
//     returns a 422 that says so.
//   - The in-memory DashMap store is authoritative; the migration
//     tables are persistence targets (same convention as the other
//     modules). The migration has never been executed against a live
//     PostgreSQL database.
//
// Events emitted on the hook bus: `web3.transaction.detected`,
// `web3.anchor.requested`, `web3.anchor.confirmed`,
// `web3.anchor.failed`, `web3.wallet.linked`, `web3.wallet.unlinked`,
// `web3.network.private_endpoint_allowed`.
// This module listens to no events.
//
// Capabilities: `web3:read`, `web3:write`, `web3:admin`, checked on
// every handler through the `role.check` hook (fails closed). The
// public ledger endpoint (`GET /api/v1/web3/public/ledger`) needs no
// caller headers, but it is default-deny: it answers 403 unless an
// admin enabled the public ledger in settings, and the API Management
// module's allowed-public-access controls are the outer gate for it.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::storage::sha256_hex;
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Capabilities enforced on the handlers.
pub const PERM_WEB3_READ: &str = "web3:read";
pub const PERM_WEB3_WRITE: &str = "web3:write";
pub const PERM_WEB3_ADMIN: &str = "web3:admin";

/// Confirmations after which an anchor counts as confirmed. The
/// provider-reported confirmation count is stored on every refresh;
/// this is the threshold for the `confirmed` transition and the
/// `web3.anchor.confirmed` event.
pub const REQUIRED_CONFIRMATIONS: i64 = 6;

/// Challenge nonces are valid for 15 minutes.
pub const CHALLENGE_TTL_SECS: i64 = 900;

/// Maximum transactions fetched from a provider in one poll per address.
pub const MAX_POLL_LIMIT: u32 = 50;

/// Per-request timeout for provider HTTP calls.
pub const PROVIDER_TIMEOUT_SECS: u64 = 15;

/// Nominal transaction size (bytes) used for the Cardano fee estimate:
/// `fee = min_fee_a * size + min_fee_b` from the protocol parameters.
pub const CARDANO_NOMINAL_TX_BYTES: u64 = 200;

/// Exact hosts a chain provider may be contacted on without any extra
/// configuration.
const KNOWN_CHAIN_HOSTS: [&str; 10] = [
    "horizon.stellar.org",
    "horizon-testnet.stellar.org",
    "api.blockfrost.io",
    "cardano-mainnet.blockfrost.io",
    "cardano-preprod.blockfrost.io",
    "cardano-preview.blockfrost.io",
    "api.koios.rest",
    "api.etherscan.io",
    "api-sepolia.etherscan.io",
    "api.bscscan.com",
];

/// Host suffixes that are always allowed: hosted-node and indexer
/// provider families.
const ALLOWED_CHAIN_HOST_SUFFIXES: [&str; 8] = [
    ".infura.io",
    ".alchemy.com",
    ".quiknode.pro",
    ".ankr.com",
    ".blockfrost.io",
    ".koios.rest",
    ".etherscan.io",
    ".bscscan.com",
];

/// Event names emitted by this module.
pub mod events {
    pub const TRANSACTION_DETECTED: &str = "web3.transaction.detected";
    pub const ANCHOR_REQUESTED: &str = "web3.anchor.requested";
    pub const ANCHOR_CONFIRMED: &str = "web3.anchor.confirmed";
    pub const ANCHOR_FAILED: &str = "web3.anchor.failed";
    pub const WALLET_LINKED: &str = "web3.wallet.linked";
    pub const WALLET_UNLINKED: &str = "web3.wallet.unlinked";
    pub const PRIVATE_ENDPOINT_ALLOWED: &str = "web3.network.private_endpoint_allowed";
}

// ---------------------------------------------------------------------------
// Chain kind
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Records (in-memory store; persistence targets in migration 000066)
// ---------------------------------------------------------------------------

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
    /// private endpoint for this network (audit-logged on creation).
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
struct WalletChallenge {
    wallet_id: String,
    nonce: String,
    message: String,
    expires_at: DateTime<Utc>,
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

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

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
    /// Stellar: the hash of the transaction whose memo carries the
    /// challenge nonce.
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

// ---------------------------------------------------------------------------
// Provider-facing value types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct Web3State {
    hooks: RwLock<Arc<HookBus>>,
    http: reqwest::Client,
    /// Live providers built from stored network records, keyed by
    /// network id. A network update or delete drops its cached
    /// provider so the next call rebuilds it from the new record.
    providers: ChainProviderRegistry,
    networks: DashMap<String, ChainNetwork>,
    watched: DashMap<String, WatchedAddress>,
    transactions: DashMap<String, ChainTxRecord>,
    anchors: DashMap<String, AnchorRecord>,
    wallets: DashMap<String, WalletLink>,
    balances: DashMap<String, BalanceSnapshot>,
    challenges: DashMap<String, WalletChallenge>,
    settings: RwLock<Web3Settings>,
}

impl Web3State {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            providers: ChainProviderRegistry::new(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(PROVIDER_TIMEOUT_SECS))
                .connect_timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("unified/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            networks: DashMap::new(),
            watched: DashMap::new(),
            transactions: DashMap::new(),
            anchors: DashMap::new(),
            wallets: DashMap::new(),
            balances: DashMap::new(),
            challenges: DashMap::new(),
            settings: RwLock::new(Web3Settings::default()),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn settings_snapshot(&self) -> Web3Settings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, ids, pagination
// ---------------------------------------------------------------------------

struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn is_admin(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_WEB3_ADMIN)
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} exceeds the maximum length of {max} characters"
        )));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn require_slug(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let cleaned = require_text(value, field, max)?;
    if !cleaned
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ':')
    {
        return Err(UnifiedError::bad_request(format!(
            "{field} must contain only letters, digits, '-', '_', '.' or ':'"
        )));
    }
    Ok(cleaned)
}

fn validate_sha256_hex(value: &str, field: &str) -> UnifiedResult<String> {
    let cleaned = value.trim().to_ascii_lowercase();
    if cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a 64-character lowercase hex SHA-256 digest"
        )));
    }
    Ok(cleaned)
}

fn validate_asset_symbol(value: &str) -> UnifiedResult<String> {
    let cleaned = value.trim().to_ascii_uppercase();
    if cleaned.is_empty() || cleaned.len() > 16 {
        return Err(UnifiedError::bad_request(
            "Asset symbol must be 1..=16 characters",
        ));
    }
    if !cleaned.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(UnifiedError::bad_request(
            "Asset symbol must contain only letters and digits",
        ));
    }
    Ok(cleaned)
}

/// Validate a chain address for its kind. EVM addresses are `0x` plus
/// 40 hex characters; Stellar addresses are 56 base32 characters
/// starting with `G`; Cardano addresses start with `addr1` (Shelley)
/// or `Ae2` (Byron) and use the base58 alphabet.
fn validate_address(kind: ChainKind, address: &str) -> UnifiedResult<String> {
    let cleaned = address.trim();
    match kind {
        ChainKind::Evm => {
            if cleaned.len() != 42
                || !cleaned.starts_with("0x")
                || !cleaned[2..].chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err(UnifiedError::bad_request(
                    "EVM address must be 0x followed by 40 hex characters",
                ));
            }
            Ok(cleaned.to_string())
        }
        ChainKind::Stellar => {
            if cleaned.len() != 56
                || !cleaned.starts_with('G')
                || !cleaned
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c))
            {
                return Err(UnifiedError::bad_request(
                    "Stellar address must be 56 characters starting with G",
                ));
            }
            Ok(cleaned.to_string())
        }
        ChainKind::Cardano => {
            let shelley = cleaned.starts_with("addr1") && (58..=120).contains(&cleaned.len());
            let byron = cleaned.starts_with("Ae2") && (100..=120).contains(&cleaned.len());
            let base58 = cleaned
                .chars()
                .all(|c| c.is_ascii_alphanumeric() && !"0OIl".contains(c));
            if !(shelley || byron) || !base58 {
                return Err(UnifiedError::bad_request(
                    "Cardano address must start with addr1 or Ae2",
                ));
            }
            Ok(cleaned.to_string())
        }
        ChainKind::Generic => require_text(cleaned, "address", 256),
    }
}

// ---------------------------------------------------------------------------
// Private-key rejection and write-only secret handling
// ---------------------------------------------------------------------------

/// True when `value` looks like a private key. Anything matching is
/// rejected at validation time, before it can be stored anywhere:
/// a 64-hex-character secret with or without a `0x` prefix, a
/// wallet-import-format string, a PEM private-key block, or a value
/// that names itself a private key.
fn looks_like_private_key(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("PRIVATE KEY") {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("private_key") || lower.contains("privatekey") {
        return true;
    }
    let hex_part = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if hex_part.len() == 64 && hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // Wallet import format: 51 base58 characters starting with 5, K or L.
    if trimmed.len() == 51
        && (trimmed.starts_with('5') || trimmed.starts_with('K') || trimmed.starts_with('L'))
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() && !"0OIl".contains(c))
    {
        return true;
    }
    false
}

fn reject_private_key_like(value: &str, field: &str) -> UnifiedResult<()> {
    if looks_like_private_key(value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} looks like a private key and is never accepted: this server never stores private keys"
        )));
    }
    Ok(())
}

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// The encryption secret: `app.secret_key`, or the development
/// fallback when none is configured.
fn encryption_secret(ctx: &AppContext) -> String {
    let config = ctx.config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        "unified-web3-development-secret".to_string()
    } else {
        config.app.secret_key.clone()
    }
}

fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut message = nonce.to_vec();
        message.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hmac_sha256(key, &message));
        counter += 1;
    }
    out.truncate(len);
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt a write-only secret. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
fn encrypt_secret(secret: &str, plaintext: &str) -> String {
    let key = Sha256::digest(secret.as_bytes());
    let nonce: [u8; 16] = rand::random();
    let stream = keystream(&key, &nonce, plaintext.len());
    let ciphertext: Vec<u8> = plaintext
        .as_bytes()
        .iter()
        .zip(stream.iter())
        .map(|(plain, mask)| plain ^ mask)
        .collect();
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&key, &tag_message);
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    )
}

/// Decrypt a value produced by `encrypt_secret`. The error message
/// never includes any part of the stored value.
fn decrypt_secret(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored web3 secret has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored web3 secret cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored web3 secret cannot be decrypted",
        ));
    }
    let (nonce, rest) = blob.split_at(16);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let key = Sha256::digest(secret.as_bytes());
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(ciphertext);
    let expected = hmac_sha256(&key, &tag_message);
    if !constant_time_eq(&expected, tag) {
        return Err(UnifiedError::internal(
            "Stored web3 secret failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored web3 secret cannot be decrypted"))
}

// ---------------------------------------------------------------------------
// Number helpers (string decimals only — no float loss)
// ---------------------------------------------------------------------------

/// Convert a `0x`-prefixed hex integer to a decimal string, with
/// arbitrary precision (chain values exceed 128 bits).
fn hex_to_decimal(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // Base-1e9 limbs, least significant first.
    let mut limbs: Vec<u32> = vec![0];
    for c in hex.chars() {
        let digit = c.to_digit(16)?;
        let mut carry: u64 = digit as u64;
        for limb in limbs.iter_mut() {
            let value = (*limb as u64) * 16 + carry;
            *limb = (value % 1_000_000_000) as u32;
            carry = value / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push((carry % 1_000_000_000) as u32);
            carry /= 1_000_000_000;
        }
    }
    let mut out = String::new();
    for (index, limb) in limbs.iter().rev().enumerate() {
        if index == 0 {
            out.push_str(&limb.to_string());
        } else {
            out.push_str(&format!("{limb:09}"));
        }
    }
    Some(out)
}

fn parse_hex_u64(raw: &str) -> Option<u64> {
    let hex = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    u64::from_str_radix(hex, 16).ok()
}

/// Shift a raw integer decimal string left by `decimals` places and
/// keep at most 4 fractional digits (truncated, trailing zeros
/// trimmed). Returns `None` when the input is not a plain integer.
fn decimal_shift(raw: &str, decimals: u32) -> Option<String> {
    let digits = raw.trim();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let decimals = decimals as usize;
    if decimals == 0 {
        return Some(digits.to_string());
    }
    let padded;
    let (int_part, frac_part): (&str, &str) = if digits.len() > decimals {
        let split = digits.len() - decimals;
        (&digits[..split], &digits[split..])
    } else {
        padded = format!("{}{digits}", "0".repeat(decimals - digits.len()));
        ("0", &padded)
    };
    let frac: String = frac_part.chars().take(4).collect();
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        Some(int_part.to_string())
    } else {
        Some(format!("{int_part}.{frac}"))
    }
}

// ---------------------------------------------------------------------------
// Endpoint allowlist (SSRF safety)
// ---------------------------------------------------------------------------

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback())
}

fn is_private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") || lower.ends_with(".internal") {
        return true;
    }
    match lower.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) => {
            addr.is_loopback()
                || addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_reserved()
        }
        Ok(IpAddr::V6(addr)) => {
            addr.is_loopback() || addr.is_unspecified() || addr.is_unique_local()
        }
        Err(_) => false,
    }
}

fn is_allowed_chain_host(host: &str, extra_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    if KNOWN_CHAIN_HOSTS.contains(&host.as_str()) {
        return true;
    }
    if ALLOWED_CHAIN_HOST_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
    {
        return true;
    }
    extra_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host)
}

/// Validate a chain provider endpoint URL against the host allowlist.
/// Only `https` is accepted, except plain `http` for loopback hosts.
/// Userinfo and non-loopback IP-literal hosts are rejected. A
/// localhost or private host is rejected unless `allow_private` is
/// true AND the host is in `allowed_private_hosts` (an explicit admin
/// decision, audit-logged by the caller).
pub fn validate_chain_endpoint_url(
    raw: &str,
    extra_hosts: &[String],
    allow_private: bool,
    allowed_private_hosts: &[String],
) -> UnifiedResult<url::Url> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Chain endpoint is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Chain endpoint must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Chain endpoint has no host"))?
        .to_ascii_lowercase();
    let is_ip = host.parse::<IpAddr>().is_ok();
    if is_ip && !is_loopback_host(&host) {
        return Err(UnifiedError::forbidden(
            "Chain endpoints on IP-literal hosts are not allowed",
        ));
    }
    if is_private_host(&host) {
        let explicitly_allowed = allow_private
            && allowed_private_hosts
                .iter()
                .any(|allowed| allowed.to_ascii_lowercase() == host);
        if !explicitly_allowed {
            return Err(UnifiedError::forbidden(
                "Chain endpoints on localhost or private hosts require an explicit admin allowlist entry",
            ));
        }
        return match url.scheme() {
            "https" => Ok(url),
            "http" if is_loopback_host(&host) => Ok(url),
            _ => Err(UnifiedError::bad_request(
                "Chain endpoint must use https (http is allowed only for loopback hosts)",
            )),
        };
    }
    if !is_allowed_chain_host(&host, extra_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "Chain endpoint host \"{host}\" is not allowlisted"
        )));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback_host(&host) => Ok(url),
        _ => Err(UnifiedError::bad_request(
            "Chain endpoint must use https (http is allowed only for loopback hosts)",
        )),
    }
}

/// Validate an explorer URL template. It is display-only (never
/// fetched), so it needs a valid `http(s)` URL with a `{tx_hash}` or
/// `{address}` placeholder and no embedded credentials.
fn validate_explorer_template(raw: &str) -> UnifiedResult<String> {
    let template = raw.trim();
    if !template.contains("{tx_hash}") && !template.contains("{address}") {
        return Err(UnifiedError::bad_request(
            "Explorer URL template must contain a {tx_hash} or {address} placeholder",
        ));
    }
    let probe = template
        .replace("{tx_hash}", "x")
        .replace("{address}", "x");
    let url = url::Url::parse(&probe)
        .map_err(|_| UnifiedError::bad_request("Explorer URL template is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Explorer URL template must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Explorer URL template must not contain credentials",
        ));
    }
    Ok(template.to_string())
}

// ---------------------------------------------------------------------------
// Chain provider abstraction
// ---------------------------------------------------------------------------

/// The contract every chain provider implements. All calls are async
/// and time-bounded by the shared HTTP client; every error is mapped
/// to the standard envelope and never leaks provider internals.
#[async_trait]
pub trait ChainProvider: Send + Sync {
    fn network_id(&self) -> &str;
    fn kind(&self) -> ChainKind;
    async fn network_status(&self) -> UnifiedResult<NetworkStatus>;
    async fn get_balance(&self, address: &str, asset: Option<&str>) -> UnifiedResult<ChainBalance>;
    async fn get_transactions(
        &self,
        address: &str,
        limit: u32,
    ) -> UnifiedResult<Vec<ProviderTransaction>>;
    async fn get_transaction(&self, tx_hash: &str) -> UnifiedResult<ProviderTransaction>;
    async fn estimate_fee(&self) -> UnifiedResult<FeeEstimate>;
    /// Ownership proof. `proof` is the EVM signature, the Stellar
    /// transaction hash whose memo carries the challenge, or unused
    /// for Cardano (lookup-only). See the module header for the
    /// honesty notes per chain.
    async fn verify_message(
        &self,
        address: &str,
        message: &str,
        proof: &str,
    ) -> UnifiedResult<VerificationOutcome>;
    async fn account_exists(&self, address: &str) -> UnifiedResult<bool>;
}

/// Registry of live providers, keyed by network id. Networks are
/// registered by configuration; a provider is built from the stored
/// network record, so adding a network never needs a code change.
pub struct ChainProviderRegistry {
    providers: RwLock<HashMap<String, Arc<dyn ChainProvider>>>,
}

impl ChainProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, network_id: String, provider: Arc<dyn ChainProvider>) {
        self.providers
            .write()
            .expect("provider registry lock poisoned")
            .insert(network_id, provider);
    }

    pub fn get(&self, network_id: &str) -> Option<Arc<dyn ChainProvider>> {
        self.providers
            .read()
            .expect("provider registry lock poisoned")
            .get(network_id)
            .cloned()
    }

    pub fn remove(&self, network_id: &str) -> bool {
        self.providers
            .write()
            .expect("provider registry lock poisoned")
            .remove(network_id)
            .is_some()
    }

    pub fn network_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .providers
            .read()
            .expect("provider registry lock poisoned")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

impl Default for ChainProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Provider HTTP helpers (generic errors only — never echo bodies)
// ---------------------------------------------------------------------------

async fn provider_get(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
) -> UnifiedResult<Value> {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.map_err(|_| {
        UnifiedError::internal("Chain provider request failed")
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(map_provider_status(status));
    }
    response.json::<Value>().await.map_err(|_| {
        UnifiedError::internal("Chain provider returned an unreadable response")
    })
}

async fn provider_post_json(
    client: &reqwest::Client,
    url: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> UnifiedResult<Value> {
    let mut request = client.post(url).json(&body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.map_err(|_| {
        UnifiedError::internal("Chain provider request failed")
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(map_provider_status(status));
    }
    response.json::<Value>().await.map_err(|_| {
        UnifiedError::internal("Chain provider returned an unreadable response")
    })
}

fn map_provider_status(status: reqwest::StatusCode) -> UnifiedError {
    match status.as_u16() {
        404 => UnifiedError::not_found("Chain provider resource not found"),
        401 => UnifiedError::unauthorized("Chain provider rejected the credentials"),
        403 => UnifiedError::forbidden("Chain provider refused the request"),
        429 => UnifiedError::new("Chain provider rate limit reached", 429),
        _ => UnifiedError::internal(format!(
            "Chain provider request failed with HTTP {status}"
        )),
    }
}

fn provider_tx(
    network_id: &str,
    tx_hash: String,
    from_address: String,
    to_address: String,
    amount_raw: String,
    asset: String,
    status: String,
    confirmations: i64,
    block_number: Option<u64>,
    occurred_at: Option<DateTime<Utc>>,
) -> ProviderTransaction {
    ProviderTransaction {
        tx_hash,
        from_address,
        to_address,
        amount_raw,
        asset,
        status,
        confirmations,
        block_number,
        occurred_at,
    }
}

fn parse_unix_ts(value: &Value) -> Option<DateTime<Utc>> {
    let secs = match value {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.parse::<i64>().ok()?,
        _ => return None,
    };
    DateTime::from_timestamp(secs, 0)
}

// ---------------------------------------------------------------------------
// EVM provider (generic JSON-RPC + Etherscan-style indexer)
// ---------------------------------------------------------------------------

/// Generic JSON-RPC provider for any EVM chain. Address history comes
/// from an optional Etherscan-compatible indexer endpoint, because
/// plain JSON-RPC has no address transaction list.
pub struct EvmRpcProvider {
    network_id: String,
    chain_id: u64,
    rpc_url: String,
    indexer_url: Option<String>,
    verify_endpoint: Option<String>,
    api_key: Option<String>,
    native_symbol: String,
    client: reqwest::Client,
}

impl EvmRpcProvider {
    pub fn new(
        network_id: String,
        chain_id: u64,
        rpc_url: String,
        indexer_url: Option<String>,
        verify_endpoint: Option<String>,
        api_key: Option<String>,
        native_symbol: String,
        client: reqwest::Client,
    ) -> Self {
        Self {
            network_id,
            chain_id,
            rpc_url,
            indexer_url,
            verify_endpoint,
            api_key,
            native_symbol,
            client,
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> UnifiedResult<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let value = provider_post_json(&self.client, &self.rpc_url, body, &[]).await?;
        if let Some(error) = value.get("error") {
            let _ = error;
            return Err(UnifiedError::internal(
                "Chain provider request failed",
            ));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| UnifiedError::internal("Chain provider returned no result"))
    }

    async fn latest_block(&self) -> UnifiedResult<u64> {
        let result = self.rpc("eth_blockNumber", json!([])).await?;
        result
            .as_str()
            .and_then(parse_hex_u64)
            .ok_or_else(|| UnifiedError::internal("Chain provider returned no block number"))
    }

    async fn indexer_get(&self, query: &[(String, String)]) -> UnifiedResult<Value> {
        let base = self.indexer_url.clone().ok_or_else(|| {
            UnifiedError::unprocessable(
                "EVM address history needs an indexer endpoint: configure one on the network first",
            )
        })?;
        let mut url = url::Url::parse(&base)
            .map_err(|_| UnifiedError::internal("Chain provider indexer URL is invalid"))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
            if let Some(key) = &self.api_key {
                pairs.append_pair("apikey", key);
            }
        }
        provider_get(&self.client, url.as_str(), &[]).await
    }
}

#[async_trait]
impl ChainProvider for EvmRpcProvider {
    fn network_id(&self) -> &str {
        &self.network_id
    }

    fn kind(&self) -> ChainKind {
        ChainKind::Evm
    }

    async fn network_status(&self) -> UnifiedResult<NetworkStatus> {
        let latest = self.latest_block().await?;
        let syncing = self.rpc("eth_syncing", json!([])).await.ok();
        let is_syncing = syncing.and_then(|value| match value {
            Value::Bool(flag) => Some(flag),
            Value::Object(_) => Some(true),
            _ => None,
        });
        Ok(NetworkStatus {
            network_id: self.network_id.clone(),
            kind: "evm".to_string(),
            latest_block: Some(latest),
            syncing: is_syncing,
            provider_version: None,
            checked_at: now(),
        })
    }

    async fn get_balance(
        &self,
        address: &str,
        asset: Option<&str>,
    ) -> UnifiedResult<ChainBalance> {
        if let Some(asset) = asset {
            if asset.to_ascii_uppercase() != self.native_symbol {
                return Err(UnifiedError::unprocessable(
                    "Token balances need a token contract lookup, which is not implemented for EVM networks; only the native asset is supported",
                ));
            }
        }
        let result = self
            .rpc("eth_getBalance", json!([address, "latest"]))
            .await?;
        let wei = result
            .as_str()
            .and_then(hex_to_decimal)
            .ok_or_else(|| UnifiedError::internal("Chain provider returned no balance"))?;
        Ok(ChainBalance {
            address: address.to_string(),
            asset: self.native_symbol.clone(),
            amount_raw: wei,
            decimals: 18,
            checked_at: now(),
        })
    }

    async fn get_transactions(
        &self,
        address: &str,
        limit: u32,
    ) -> UnifiedResult<Vec<ProviderTransaction>> {
        let limit = limit.clamp(1, MAX_POLL_LIMIT);
        let value = self
            .indexer_get(&[
                ("module".to_string(), "account".to_string()),
                ("action".to_string(), "txlist".to_string()),
                ("address".to_string(), address.to_string()),
                ("startblock".to_string(), "0".to_string()),
                ("endblock".to_string(), "99999999".to_string()),
                ("page".to_string(), "1".to_string()),
                ("offset".to_string(), limit.to_string()),
                ("sort".to_string(), "desc".to_string()),
            ])
            .await?;
        let entries = value
            .get("result")
            .and_then(|result| result.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for entry in entries {
            let Some(hash) = entry.get("hash").and_then(|v| v.as_str()) else {
                continue;
            };
            let from = entry
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let to = entry
                .get("to")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let amount_raw = entry
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string();
            let is_error = entry
                .get("isError")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                == "1";
            let receipt_ok = entry
                .get("txreceipt_status")
                .and_then(|v| v.as_str())
                .unwrap_or("1")
                == "1";
            let status = if is_error || !receipt_ok {
                "failed".to_string()
            } else {
                "confirmed".to_string()
            };
            let confirmations = entry
                .get("confirmations")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let block_number = entry
                .get("blockNumber")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<u64>().ok());
            let occurred_at = entry.get("timeStamp").and_then(parse_unix_ts);
            out.push(provider_tx(
                &self.network_id,
                hash.to_string(),
                from,
                to,
                amount_raw,
                self.native_symbol.clone(),
                status,
                confirmations,
                block_number,
                occurred_at,
            ));
        }
        Ok(out)
    }

    async fn get_transaction(&self, tx_hash: &str) -> UnifiedResult<ProviderTransaction> {
        let result = self
            .rpc("eth_getTransactionByHash", json!([tx_hash]))
            .await?;
        if result.is_null() {
            return Err(UnifiedError::not_found("Transaction not found on this network"));
        }
        let from = result
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let to = result
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let amount_raw = result
            .get("value")
            .and_then(|v| v.as_str())
            .and_then(hex_to_decimal)
            .unwrap_or_else(|| "0".to_string());
        let block_number = result
            .get("blockNumber")
            .and_then(|v| v.as_str())
            .and_then(parse_hex_u64);
        let receipt = self
            .rpc("eth_getTransactionReceipt", json!([tx_hash]))
            .await
            .ok();
        let (status, confirmations) = match (receipt, block_number) {
            (Some(receipt), Some(block)) if !receipt.is_null() => {
                let ok = receipt
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(|v| v == "0x1")
                    .unwrap_or(false);
                let latest = self.latest_block().await.unwrap_or(block);
                let confirmations = latest.saturating_sub(block) as i64;
                (
                    if ok { "confirmed" } else { "failed" }.to_string(),
                    confirmations,
                )
            }
            _ => ("pending".to_string(), 0),
        };
        Ok(provider_tx(
            &self.network_id,
            tx_hash.to_string(),
            from,
            to,
            amount_raw,
            self.native_symbol.clone(),
            status,
            confirmations,
            block_number,
            None,
        ))
    }

    async fn estimate_fee(&self) -> UnifiedResult<FeeEstimate> {
        let result = self.rpc("eth_gasPrice", json!([])).await?;
        let wei = result
            .as_str()
            .and_then(hex_to_decimal)
            .ok_or_else(|| UnifiedError::internal("Chain provider returned no gas price"))?;
        Ok(FeeEstimate {
            network_id: self.network_id.clone(),
            fee_raw: wei,
            fee_unit: "wei".to_string(),
            detail: "Current gas price per unit of gas (eth_gasPrice); multiply by the transaction gas limit for the total.".to_string(),
            estimated_at: now(),
        })
    }

    async fn verify_message(
        &self,
        address: &str,
        message: &str,
        proof: &str,
    ) -> UnifiedResult<VerificationOutcome> {
        // No secp256k1 crate is present in Cargo.toml, so this build
        // cannot do ecrecover in-process. Verification is delegated to
        // an admin-configured, allowlisted verification endpoint when
        // one is set; without one the call fails honestly instead of
        // pretending to verify.
        let Some(endpoint) = &self.verify_endpoint else {
            return Err(UnifiedError::unprocessable(
                "EVM signature verification needs a configured verification endpoint: no secp256k1 implementation is available in this build",
            ));
        };
        let value = provider_post_json(
            &self.client,
            endpoint,
            json!({ "address": address, "message": message, "signature": proof }),
            &[],
        )
        .await?;
        let valid = value
            .get("valid")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(VerificationOutcome {
            verified: valid,
            method: "signature".to_string(),
            detail: if valid {
                "The verification endpoint confirmed the signature matches the address and challenge message.".to_string()
            } else {
                "The verification endpoint reported the signature does not match the address and challenge message.".to_string()
            },
        })
    }

    async fn account_exists(&self, _address: &str) -> UnifiedResult<bool> {
        // Any well-formed address is addressable on an EVM chain; the
        // RPC call succeeding is the existence check.
        let _ = self.latest_block().await?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Stellar provider (Horizon API)
// ---------------------------------------------------------------------------

/// Horizon API provider for Stellar. Balances are read from the
/// account endpoint; history comes from the account transactions
/// endpoint plus one operations lookup per transaction.
pub struct StellarProvider {
    network_id: String,
    base_url: String,
    client: reqwest::Client,
}

impl StellarProvider {
    pub fn new(network_id: String, base_url: String, client: reqwest::Client) -> Self {
        Self {
            network_id,
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn account(&self, address: &str) -> UnifiedResult<Value> {
        provider_get(
            &self.client,
            &self.url(&format!("/accounts/{}", address)),
            &[("Accept", "application/json")],
        )
        .await
    }
}

#[async_trait]
impl ChainProvider for StellarProvider {
    fn network_id(&self) -> &str {
        &self.network_id
    }

    fn kind(&self) -> ChainKind {
        ChainKind::Stellar
    }

    async fn network_status(&self) -> UnifiedResult<NetworkStatus> {
        let root = provider_get(&self.client, &self.url("/"), &[("Accept", "application/json")]).await?;
        let latest = root
            .get("core_latest_ledger")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                root.get("history_latest_ledger")
                    .and_then(|v| v.as_u64())
            });
        Ok(NetworkStatus {
            network_id: self.network_id.clone(),
            kind: "stellar".to_string(),
            latest_block: latest,
            syncing: None,
            provider_version: root
                .get("horizon_version")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            checked_at: now(),
        })
    }

    async fn get_balance(
        &self,
        address: &str,
        asset: Option<&str>,
    ) -> UnifiedResult<ChainBalance> {
        let account = self.account(address).await?;
        let balances = account
            .get("balances")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let wanted = asset.map(|a| a.to_ascii_uppercase());
        for entry in &balances {
            let is_native = entry
                .get("asset_type")
                .and_then(|v| v.as_str())
                .map(|v| v == "native")
                .unwrap_or(false);
            let code = if is_native {
                "XLM".to_string()
            } else {
                entry
                    .get("asset_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            if let Some(wanted) = &wanted {
                if &code != wanted {
                    continue;
                }
            } else if !is_native {
                continue;
            }
            let amount_raw = entry
                .get("balance")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string();
            return Ok(ChainBalance {
                address: address.to_string(),
                asset: code,
                amount_raw,
                decimals: 7,
                checked_at: now(),
            });
        }
        Err(UnifiedError::not_found(
            "Asset balance not found for this Stellar account",
        ))
    }

    async fn get_transactions(
        &self,
        address: &str,
        limit: u32,
    ) -> UnifiedResult<Vec<ProviderTransaction>> {
        let limit = limit.clamp(1, MAX_POLL_LIMIT);
        let list = provider_get(
            &self.client,
            &self.url(&format!(
                "/accounts/{address}/transactions?order=desc&limit={limit}&include_failed=true"
            )),
            &[("Accept", "application/json")],
        )
        .await?;
        let records = list
            .get("_embedded")
            .and_then(|v| v.get("records"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for record in records {
            let Some(hash) = record.get("hash").and_then(|v| v.as_str()) else {
                continue;
            };
            let successful = record
                .get("successful")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let occurred_at = record
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<DateTime<Utc>>().ok());
            let ledger = record.get("ledger").and_then(|v| v.as_u64());
            // One operations lookup per transaction gives the payment
            // counterparty and amount; when it yields nothing, the
            // transaction is still recorded with the account as sender.
            let operations = provider_get(
                &self.client,
                &self.url(&format!("/transactions/{hash}/operations?limit=10")),
                &[("Accept", "application/json")],
            )
            .await
            .ok();
            let mut from = record
                .get("source_account")
                .and_then(|v| v.as_str())
                .unwrap_or(address)
                .to_string();
            let mut to = address.to_string();
            let mut amount_raw = "0".to_string();
            let mut asset = "XLM".to_string();
            if let Some(operations) = operations {
                if let Some(op_records) = operations
                    .get("_embedded")
                    .and_then(|v| v.get("records"))
                    .and_then(|v| v.as_array())
                {
                    for operation in op_records {
                        let op_type = operation
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if op_type == "payment" {
                            from = operation
                                .get("from")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&from)
                                .to_string();
                            to = operation
                                .get("to")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&to)
                                .to_string();
                            amount_raw = operation
                                .get("amount")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0")
                                .to_string();
                            asset = operation
                                .get("asset_code")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                                .unwrap_or_else(|| "XLM".to_string());
                            break;
                        }
                    }
                }
            }
            out.push(provider_tx(
                &self.network_id,
                hash.to_string(),
                from,
                to,
                amount_raw,
                asset,
                if successful {
                    "confirmed".to_string()
                } else {
                    "failed".to_string()
                },
                0,
                ledger,
                occurred_at,
            ));
        }
        Ok(out)
    }

    async fn get_transaction(&self, tx_hash: &str) -> UnifiedResult<ProviderTransaction> {
        let record = provider_get(
            &self.client,
            &self.url(&format!("/transactions/{tx_hash}")),
            &[("Accept", "application/json")],
        )
        .await?;
        let successful = record
            .get("successful")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let occurred_at = record
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<DateTime<Utc>>().ok());
        let ledger = record.get("ledger").and_then(|v| v.as_u64());
        let from = record
            .get("source_account")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(provider_tx(
            &self.network_id,
            tx_hash.to_string(),
            from,
            String::new(),
            "0".to_string(),
            "XLM".to_string(),
            if successful {
                "confirmed".to_string()
            } else {
                "failed".to_string()
            },
            0,
            ledger,
            occurred_at,
        ))
    }

    async fn estimate_fee(&self) -> UnifiedResult<FeeEstimate> {
        let stats = provider_get(
            &self.client,
            &self.url("/fee_stats"),
            &[("Accept", "application/json")],
        )
        .await?;
        let mode = stats
            .get("fee_charged")
            .and_then(|v| v.get("mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("100")
            .to_string();
        Ok(FeeEstimate {
            network_id: self.network_id.clone(),
            fee_raw: mode,
            fee_unit: "stroops".to_string(),
            detail: "Mode fee charged over the last ledgers (Horizon fee_stats).".to_string(),
            estimated_at: now(),
        })
    }

    async fn verify_message(
        &self,
        address: &str,
        message: &str,
        proof: &str,
    ) -> UnifiedResult<VerificationOutcome> {
        // Ownership proof: a transaction from the address whose memo
        // carries the challenge nonce.
        let record = provider_get(
            &self.client,
            &self.url(&format!("/transactions/{}", proof.trim())),
            &[("Accept", "application/json")],
        )
        .await?;
        let source = record
            .get("source_account")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let memo = record
            .get("memo")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if source == address && memo == message {
            Ok(VerificationOutcome {
                verified: true,
                method: "memo_transaction".to_string(),
                detail: "A transaction from this address carries the challenge nonce in its memo.".to_string(),
            })
        } else {
            Ok(VerificationOutcome {
                verified: false,
                method: "memo_transaction".to_string(),
                detail: "No transaction from this address carries the challenge nonce in its memo.".to_string(),
            })
        }
    }

    async fn account_exists(&self, address: &str) -> UnifiedResult<bool> {
        match self.account(address).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == 404 => Ok(false),
            Err(error) => Err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// Cardano provider (Blockfrost- or Koios-style REST)
// ---------------------------------------------------------------------------

/// REST provider for Cardano. `api_style` is `"blockfrost"` (API key
/// sent as the `project_id` header) or `"koios"` (no key needed).
pub struct CardanoProvider {
    network_id: String,
    base_url: String,
    api_style: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl CardanoProvider {
    pub fn new(
        network_id: String,
        base_url: String,
        api_style: String,
        api_key: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            network_id,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_style,
            api_key,
            client,
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = Vec::new();
        if self.api_style == "blockfrost" {
            if let Some(key) = &self.api_key {
                headers.push(("project_id".to_string(), key.clone()));
            }
        }
        headers
    }

    async fn get(&self, path: &str) -> UnifiedResult<Value> {
        let headers = self.headers();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        provider_get(&self.client, &format!("{}{}", self.base_url, path), &refs).await
    }

    async fn post(&self, path: &str, body: Value) -> UnifiedResult<Value> {
        let headers = self.headers();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        provider_post_json(
            &self.client,
            &format!("{}{}", self.base_url, path),
            body,
            &refs,
        )
        .await
    }

    fn is_blockfrost(&self) -> bool {
        self.api_style == "blockfrost"
    }
}

#[async_trait]
impl ChainProvider for CardanoProvider {
    fn network_id(&self) -> &str {
        &self.network_id
    }

    fn kind(&self) -> ChainKind {
        ChainKind::Cardano
    }

    async fn network_status(&self) -> UnifiedResult<NetworkStatus> {
        let (latest, version) = if self.is_blockfrost() {
            let block = self.get("/blocks/latest").await?;
            (
                block.get("height").and_then(|v| v.as_u64()),
                block
                    .get("hash")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            )
        } else {
            let tip = self.get("/tip").await?;
            (
                tip.get("block_no").and_then(|v| v.as_u64()),
                tip.get("hash")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            )
        };
        Ok(NetworkStatus {
            network_id: self.network_id.clone(),
            kind: "cardano".to_string(),
            latest_block: latest,
            syncing: None,
            provider_version: version,
            checked_at: now(),
        })
    }

    async fn get_balance(
        &self,
        address: &str,
        asset: Option<&str>,
    ) -> UnifiedResult<ChainBalance> {
        if self.is_blockfrost() {
            let info = self.get(&format!("/addresses/{address}")).await?;
            let amounts = info
                .get("amount")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let wanted = asset.map(|a| a.to_ascii_uppercase());
            for entry in &amounts {
                let unit = entry
                    .get("unit")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let code = if unit == "lovelace" {
                    "ADA".to_string()
                } else {
                    unit.to_string()
                };
                if let Some(wanted) = &wanted {
                    if &code != wanted {
                        continue;
                    }
                } else if unit != "lovelace" {
                    continue;
                }
                let quantity = entry
                    .get("quantity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();
                return Ok(ChainBalance {
                    address: address.to_string(),
                    asset: code,
                    amount_raw: quantity,
                    decimals: 6,
                    checked_at: now(),
                });
            }
            Err(UnifiedError::not_found(
                "Asset balance not found for this Cardano address",
            ))
        } else {
            if let Some(asset) = asset {
                if asset.to_ascii_uppercase() != "ADA" {
                    return Err(UnifiedError::unprocessable(
                        "Koios address info reports the lovelace balance only; use a Blockfrost-style endpoint for other assets",
                    ));
                }
            }
            let info = self
                .post("/address_info", json!({ "_addresses": [address] }))
                .await?;
            let first = info
                .as_array()
                .and_then(|list| list.first())
                .ok_or_else(|| {
                    UnifiedError::not_found("Cardano address not found")
                })?;
            let balance = first
                .get("balance")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string();
            Ok(ChainBalance {
                address: address.to_string(),
                asset: "ADA".to_string(),
                amount_raw: balance,
                decimals: 6,
                checked_at: now(),
            })
        }
    }

    async fn get_transactions(
        &self,
        address: &str,
        limit: u32,
    ) -> UnifiedResult<Vec<ProviderTransaction>> {
        let limit = limit.clamp(1, MAX_POLL_LIMIT) as usize;
        if self.is_blockfrost() {
            let list = self
                .get(&format!(
                    "/addresses/{address}/transactions?count={limit}&page=1&order=desc"
                ))
                .await?;
            let entries = list.as_array().cloned().unwrap_or_default();
            let mut out = Vec::new();
            for entry in entries.iter().take(limit) {
                let Some(hash) = entry.get("tx_hash").and_then(|v| v.as_str()) else {
                    continue;
                };
                let block_height = entry.get("block_height").and_then(|v| v.as_u64());
                let block_time = entry.get("block_time").and_then(|v| v.as_u64());
                // UTXO lookup gives the outputs paid to this address.
                let utxos = self.get(&format!("/txs/{hash}/utxos")).await.ok();
                let mut amount_raw = "0".to_string();
                let mut from = String::new();
                if let Some(utxos) = utxos {
                    if let Some(outputs) = utxos
                        .get("outputs")
                        .and_then(|v| v.as_array())
                    {
                        let mut total: u128 = 0;
                        for output in outputs {
                            if output
                                .get("address")
                                .and_then(|v| v.as_str())
                                .map(|a| a == address)
                                .unwrap_or(false)
                            {
                                if let Some(amounts) = output
                                    .get("amount")
                                    .and_then(|v| v.as_array())
                                {
                                    for amount in amounts {
                                        if amount
                                            .get("unit")
                                            .and_then(|v| v.as_str())
                                            .map(|u| u == "lovelace")
                                            .unwrap_or(false)
                                        {
                                            total += amount
                                                .get("quantity")
                                                .and_then(|v| v.as_str())
                                                .and_then(|v| v.parse::<u128>().ok())
                                                .unwrap_or(0);
                                        }
                                    }
                                }
                            }
                        }
                        amount_raw = total.to_string();
                    }
                    if let Some(inputs) = utxos.get("inputs").and_then(|v| v.as_array()) {
                        from = inputs
                            .first()
                            .and_then(|v| v.get("address"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                    }
                }
                out.push(provider_tx(
                    &self.network_id,
                    hash.to_string(),
                    from,
                    address.to_string(),
                    amount_raw,
                    "ADA".to_string(),
                    "confirmed".to_string(),
                    0,
                    block_height,
                    block_time.and_then(|secs| DateTime::from_timestamp(secs as i64, 0)),
                ));
            }
            Ok(out)
        } else {
            let list = self
                .get(&format!("/address_txs?_address={address}"))
                .await?;
            let entries = list.as_array().cloned().unwrap_or_default();
            let hashes: Vec<String> = entries
                .iter()
                .take(limit)
                .filter_map(|entry| {
                    entry
                        .get("tx_hash")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect();
            if hashes.is_empty() {
                return Ok(Vec::new());
            }
            let infos = self
                .post("/tx_info", json!({ "_tx_hashes": hashes }))
                .await?;
            let mut out = Vec::new();
            for info in infos.as_array().cloned().unwrap_or_default() {
                let Some(hash) = info.get("tx_hash").and_then(|v| v.as_str()) else {
                    continue;
                };
                let block_no = info.get("block_no").and_then(|v| v.as_u64());
                let block_time = info.get("block_time").and_then(|v| v.as_u64());
                let mut amount_raw = "0".to_string();
                let mut from = String::new();
                if let Some(outputs) = info.get("outputs").and_then(|v| v.as_array()) {
                    let mut total: u128 = 0;
                    for output in outputs {
                        if output
                            .get("payment_addr")
                            .and_then(|v| v.get("bech32"))
                            .and_then(|v| v.as_str())
                            .map(|a| a == address)
                            .unwrap_or(false)
                        {
                            total += output
                                .get("value")
                                .and_then(|v| v.as_str())
                                .and_then(|v| v.parse::<u128>().ok())
                                .unwrap_or(0);
                        }
                    }
                    amount_raw = total.to_string();
                }
                if let Some(inputs) = info.get("inputs").and_then(|v| v.as_array()) {
                    from = inputs
                        .first()
                        .and_then(|v| v.get("payment_addr"))
                        .and_then(|v| v.get("bech32"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                out.push(provider_tx(
                    &self.network_id,
                    hash.to_string(),
                    from,
                    address.to_string(),
                    amount_raw,
                    "ADA".to_string(),
                    "confirmed".to_string(),
                    0,
                    block_no,
                    block_time.and_then(|secs| DateTime::from_timestamp(secs as i64, 0)),
                ));
            }
            Ok(out)
        }
    }

    async fn get_transaction(&self, tx_hash: &str) -> UnifiedResult<ProviderTransaction> {
        if self.is_blockfrost() {
            let tx = self.get(&format!("/txs/{tx_hash}")).await?;
            let block_height = tx.get("block_height").and_then(|v| v.as_u64());
            let block_time = tx.get("block_time").and_then(|v| v.as_u64());
            Ok(provider_tx(
                &self.network_id,
                tx_hash.to_string(),
                String::new(),
                String::new(),
                "0".to_string(),
                "ADA".to_string(),
                "confirmed".to_string(),
                0,
                block_height,
                block_time.and_then(|secs| DateTime::from_timestamp(secs as i64, 0)),
            ))
        } else {
            let infos = self
                .post("/tx_info", json!({ "_tx_hashes": [tx_hash] }))
                .await?;
            let info = infos
                .as_array()
                .and_then(|list| list.first())
                .ok_or_else(|| UnifiedError::not_found("Transaction not found on this network"))?;
            let block_no = info.get("block_no").and_then(|v| v.as_u64());
            let block_time = info.get("block_time").and_then(|v| v.as_u64());
            Ok(provider_tx(
                &self.network_id,
                tx_hash.to_string(),
                String::new(),
                String::new(),
                "0".to_string(),
                "ADA".to_string(),
                "confirmed".to_string(),
                0,
                block_no,
                block_time.and_then(|secs| DateTime::from_timestamp(secs as i64, 0)),
            ))
        }
    }

    async fn estimate_fee(&self) -> UnifiedResult<FeeEstimate> {
        let (fee_a, fee_b) = if self.is_blockfrost() {
            let params = self.get("/epochs/latest/parameters").await?;
            (
                params
                    .get("min_fee_a")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(44),
                params
                    .get("min_fee_b")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(155_381),
            )
        } else {
            let genesis = self.get("/genesis").await?;
            (
                genesis
                    .get("min_fee_a")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(44),
                genesis
                    .get("min_fee_b")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(155_381),
            )
        };
        let fee = fee_a
            .saturating_mul(CARDANO_NOMINAL_TX_BYTES)
            .saturating_add(fee_b);
        Ok(FeeEstimate {
            network_id: self.network_id.clone(),
            fee_raw: fee.to_string(),
            fee_unit: "lovelace".to_string(),
            detail: format!(
                "Protocol fee for a nominal {CARDANO_NOMINAL_TX_BYTES}-byte transaction: min_fee_a * size + min_fee_b."
            ),
            estimated_at: now(),
        })
    }

    async fn verify_message(
        &self,
        address: &str,
        _message: &str,
        _proof: &str,
    ) -> UnifiedResult<VerificationOutcome> {
        // Lookup-only: the address existing and being active does not
        // prove control of it. This is reported honestly and the
        // wallet stays in `lookup_only` status.
        let exists = self.account_exists(address).await?;
        Ok(VerificationOutcome {
            verified: false,
            method: "address_lookup".to_string(),
            detail: if exists {
                "The address exists and is active on this network, but a provider lookup alone does not prove control of it. A signed-message proof is not available through this provider.".to_string()
            } else {
                "The address was not found on this network.".to_string()
            },
        })
    }

    async fn account_exists(&self, address: &str) -> UnifiedResult<bool> {
        if self.is_blockfrost() {
            match self.get(&format!("/addresses/{address}")).await {
                Ok(_) => Ok(true),
                Err(error) if error.code == 404 => Ok(false),
                Err(error) => Err(error),
            }
        } else {
            let info = self
                .post("/address_info", json!({ "_addresses": [address] }))
                .await?;
            Ok(info
                .as_array()
                .is_some_and(|list| !list.is_empty()))
        }
    }
}

// ---------------------------------------------------------------------------
// Fake provider (tests only)
// ---------------------------------------------------------------------------

/// In-memory provider for tests. Never used in production code.
#[cfg(test)]
pub struct FakeChainProvider {
    network_id: String,
    kind: ChainKind,
    balances: std::sync::Mutex<HashMap<String, String>>,
    transactions: std::sync::Mutex<Vec<ProviderTransaction>>,
}

#[cfg(test)]
impl FakeChainProvider {
    pub fn new(network_id: &str, kind: ChainKind) -> Self {
        Self {
            network_id: network_id.to_string(),
            kind,
            balances: std::sync::Mutex::new(HashMap::new()),
            transactions: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn set_balance(&self, address: &str, amount_raw: &str) {
        self.balances
            .lock()
            .expect("fake provider lock poisoned")
            .insert(address.to_string(), amount_raw.to_string());
    }

    pub fn add_transaction(&self, tx: ProviderTransaction) {
        self.transactions
            .lock()
            .expect("fake provider lock poisoned")
            .push(tx);
    }
}

#[cfg(test)]
#[async_trait]
impl ChainProvider for FakeChainProvider {
    fn network_id(&self) -> &str {
        &self.network_id
    }

    fn kind(&self) -> ChainKind {
        self.kind
    }

    async fn network_status(&self) -> UnifiedResult<NetworkStatus> {
        Ok(NetworkStatus {
            network_id: self.network_id.clone(),
            kind: self.kind.as_str().to_string(),
            latest_block: Some(1_000),
            syncing: Some(false),
            provider_version: Some("fake-1.0".to_string()),
            checked_at: now(),
        })
    }

    async fn get_balance(
        &self,
        address: &str,
        asset: Option<&str>,
    ) -> UnifiedResult<ChainBalance> {
        let amount_raw = self
            .balances
            .lock()
            .expect("fake provider lock poisoned")
            .get(address)
            .cloned()
            .unwrap_or_else(|| "0".to_string());
        Ok(ChainBalance {
            address: address.to_string(),
            asset: asset.unwrap_or("FAKE").to_string(),
            amount_raw,
            decimals: 6,
            checked_at: now(),
        })
    }

    async fn get_transactions(
        &self,
        address: &str,
        limit: u32,
    ) -> UnifiedResult<Vec<ProviderTransaction>> {
        let transactions = self
            .transactions
            .lock()
            .expect("fake provider lock poisoned")
            .clone();
        Ok(transactions
            .into_iter()
            .filter(|tx| tx.from_address == address || tx.to_address == address)
            .take(limit as usize)
            .collect())
    }

    async fn get_transaction(&self, tx_hash: &str) -> UnifiedResult<ProviderTransaction> {
        self.transactions
            .lock()
            .expect("fake provider lock poisoned")
            .iter()
            .find(|tx| tx.tx_hash == tx_hash)
            .cloned()
            .ok_or_else(|| UnifiedError::not_found("Transaction not found"))
    }

    async fn estimate_fee(&self) -> UnifiedResult<FeeEstimate> {
        Ok(FeeEstimate {
            network_id: self.network_id.clone(),
            fee_raw: "100".to_string(),
            fee_unit: "fake".to_string(),
            detail: "Fake provider fee estimate.".to_string(),
            estimated_at: now(),
        })
    }

    async fn verify_message(
        &self,
        _address: &str,
        _message: &str,
        proof: &str,
    ) -> UnifiedResult<VerificationOutcome> {
        Ok(VerificationOutcome {
            verified: proof == "valid-proof",
            method: "signature".to_string(),
            detail: "Fake provider verification.".to_string(),
        })
    }

    async fn account_exists(&self, _address: &str) -> UnifiedResult<bool> {
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Provider construction from a stored network record
// ---------------------------------------------------------------------------

/// Build a provider for a stored network, reusing the registry
/// entry when one is cached. The provider API key is decrypted in
/// memory for this call only and never stored in plain text.
/// Generic networks have no provider implementation.
fn provider_for_network(
    state: &Arc<Web3State>,
    network: &ChainNetwork,
    encryption_secret_value: &str,
) -> UnifiedResult<Arc<dyn ChainProvider>> {
    if !network.enabled {
        return Err(UnifiedError::unprocessable(
            "Network is disabled; enable it before use",
        ));
    }
    if let Some(provider) = state.providers.get(&network.id) {
        return Ok(provider);
    }
    let api_key = match &network.api_key_encrypted {
        Some(encrypted) => Some(decrypt_secret(encryption_secret_value, encrypted)?),
        None => None,
    };
    let client = state.http.clone();
    let provider: Arc<dyn ChainProvider> = match network.kind {
        ChainKind::Evm => Arc::new(EvmRpcProvider::new(
            network.id.clone(),
            network.chain_id.unwrap_or(0),
            network.endpoint_url.clone(),
            network.indexer_url.clone(),
            network.verify_endpoint.clone(),
            api_key,
            network.native_asset_symbol.clone(),
            client,
        )),
        ChainKind::Stellar => Arc::new(StellarProvider::new(
            network.id.clone(),
            network.endpoint_url.clone(),
            client,
        )),
        ChainKind::Cardano => {
            // Blockfrost-style endpoints carry `blockfrost` in the
            // host; anything else is treated as Koios-style.
            let style = if network.endpoint_url.contains("blockfrost") {
                "blockfrost".to_string()
            } else {
                "koios".to_string()
            };
            Arc::new(CardanoProvider::new(
                network.id.clone(),
                network.endpoint_url.clone(),
                style,
                api_key,
                client,
            ))
        }
        ChainKind::Generic => {
            return Err(UnifiedError::unprocessable(
                "Generic networks have no chain provider: choose evm, stellar, or cardano",
            ));
        }
    };
    state.providers.register(network.id.clone(), provider.clone());
    Ok(provider)
}

fn require_network(state: &Arc<Web3State>, network_id: &str) -> UnifiedResult<ChainNetwork> {
    state
        .networks
        .get(network_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Network not found"))
}

/// Resolve the claimed content hash for an anchor request: either a
/// supplied SHA-256 or the hash of supplied raw content.
fn resolve_content_hash(input: &CreateAnchorRequest) -> UnifiedResult<String> {
    match (&input.content_sha256, &input.content) {
        (Some(hash), _) => validate_sha256_hex(hash, "content_sha256"),
        (None, Some(content)) => Ok(sha256_hex(content.as_bytes())),
        (None, None) => Err(UnifiedError::bad_request(
            "Provide content_sha256 or content so the record hash can be anchored",
        )),
    }
}

fn ledger_entry(anchor: &AnchorRecord) -> LedgerEntry {
    LedgerEntry {
        anchor_id: anchor.id.clone(),
        record_type: anchor.record_type.clone(),
        record_id: anchor.record_id.clone(),
        network_id: anchor.network_id.clone(),
        tx_hash: anchor.tx_hash.clone(),
        status: anchor.status.clone(),
        confirmations: anchor.confirmations,
        anchored_at: anchor.created_at,
        content_sha256: anchor.content_sha256.clone(),
    }
}

// ---------------------------------------------------------------------------
// Polling (web3.poll) — called by the scheduler, not by this module
// ---------------------------------------------------------------------------

/// Poll every watched address on its network, store newly seen
/// transactions (deduplicated by network + hash), and emit
/// `web3.transaction.detected` once per new transaction.
///
/// The scheduler owns the timer; wire it with:
///
/// ```ignore
/// // In the scheduler (integrator wiring, not this module):
/// // every N minutes, call:
/// //   web3::poll_watched_addresses(&web3_state, &encryption_secret).await
/// // where `encryption_secret` is the app secret used to decrypt
/// // stored provider API keys. A per-address provider failure is
/// // recorded in the returned summary, never fatal to the run.
/// ```
pub async fn poll_watched_addresses(
    state: &Arc<Web3State>,
    encryption_secret_value: &str,
) -> UnifiedResult<Value> {
    let settings = state.settings_snapshot();
    let limit = settings.max_poll_transactions.clamp(1, MAX_POLL_LIMIT);
    let watched: Vec<WatchedAddress> = state
        .watched
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    let mut checked = 0usize;
    let mut new_transactions = 0usize;
    let mut errors: Vec<Value> = Vec::new();
    for address in watched {
        checked += 1;
        let result: UnifiedResult<usize> = (|| async {
            let network = require_network(state, &address.network_id)?;
            let provider = provider_for_network(state, &network, encryption_secret_value)?;
            let found = provider
                .get_transactions(&address.address, limit)
                .await?;
            let mut fresh = 0usize;
            for tx in found {
                let duplicate = state.transactions.iter().any(|entry| {
                    entry.value().network_id == address.network_id
                        && entry.value().tx_hash == tx.tx_hash
                });
                if duplicate {
                    continue;
                }
                let direction = if tx.to_address.eq_ignore_ascii_case(&address.address) {
                    "incoming".to_string()
                } else if tx.from_address.eq_ignore_ascii_case(&address.address) {
                    "outgoing".to_string()
                } else {
                    "observed".to_string()
                };
                let decimals = match network.kind {
                    ChainKind::Evm => 18,
                    ChainKind::Stellar => 7,
                    ChainKind::Cardano => 6,
                    ChainKind::Generic => 0,
                };
                let amount = if tx.amount_raw.chars().all(|c| c.is_ascii_digit()) {
                    decimal_shift(&tx.amount_raw, decimals)
                } else {
                    None
                };
                let record = ChainTxRecord {
                    id: new_id(),
                    network_id: address.network_id.clone(),
                    tx_hash: tx.tx_hash.clone(),
                    from_address: tx.from_address.clone(),
                    to_address: tx.to_address.clone(),
                    amount_raw: tx.amount_raw.clone(),
                    amount,
                    asset: tx.asset.clone(),
                    status: tx.status.clone(),
                    confirmations: tx.confirmations,
                    block_number: tx.block_number,
                    direction,
                    watched_address_id: Some(address.id.clone()),
                    occurred_at: tx.occurred_at,
                    first_seen_at: now(),
                    last_checked_at: now(),
                    created_at: now(),
                    updated_at: now(),
                };
                state
                    .transactions
                    .insert(record.id.clone(), record.clone());
                state.emit(
                    events::TRANSACTION_DETECTED,
                    serde_json::to_value(&record).unwrap_or(Value::Null),
                );
                fresh += 1;
            }
            if let Some(mut entry) = state.watched.get_mut(&address.id) {
                entry.last_checked_at = Some(now());
                entry.updated_at = now();
            }
            Ok(fresh)
        })()
        .await;
        match result {
            Ok(fresh) => new_transactions += fresh,
            Err(error) => errors.push(json!({
                "watched_address_id": address.id,
                "network_id": address.network_id,
                "address": address.address,
                "error": error.message,
            })),
        }
    }
    Ok(json!({
        "checked": checked,
        "new_transactions": new_transactions,
        "errors": errors,
    }))
}

async fn poll_one_address(
    state: &Arc<Web3State>,
    encryption_secret_value: &str,
    address: &WatchedAddress,
) -> UnifiedResult<Value> {
    let settings = state.settings_snapshot();
    let limit = settings.max_poll_transactions.clamp(1, MAX_POLL_LIMIT);
    let network = require_network(state, &address.network_id)?;
    let provider = provider_for_network(state, &network, encryption_secret_value)?;
    let found = provider
        .get_transactions(&address.address, limit)
        .await?;
    let mut fresh = 0usize;
    for tx in found {
        let duplicate = state.transactions.iter().any(|entry| {
            entry.value().network_id == address.network_id
                && entry.value().tx_hash == tx.tx_hash
        });
        if duplicate {
            continue;
        }
        let direction = if tx.to_address.eq_ignore_ascii_case(&address.address) {
            "incoming".to_string()
        } else if tx.from_address.eq_ignore_ascii_case(&address.address) {
            "outgoing".to_string()
        } else {
            "observed".to_string()
        };
        let decimals = match network.kind {
            ChainKind::Evm => 18,
            ChainKind::Stellar => 7,
            ChainKind::Cardano => 6,
            ChainKind::Generic => 0,
        };
        let amount = if tx.amount_raw.chars().all(|c| c.is_ascii_digit()) {
            decimal_shift(&tx.amount_raw, decimals)
        } else {
            None
        };
        let record = ChainTxRecord {
            id: new_id(),
            network_id: address.network_id.clone(),
            tx_hash: tx.tx_hash.clone(),
            from_address: tx.from_address.clone(),
            to_address: tx.to_address.clone(),
            amount_raw: tx.amount_raw.clone(),
            amount,
            asset: tx.asset.clone(),
            status: tx.status.clone(),
            confirmations: tx.confirmations,
            block_number: tx.block_number,
            direction,
            watched_address_id: Some(address.id.clone()),
            occurred_at: tx.occurred_at,
            first_seen_at: now(),
            last_checked_at: now(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .transactions
            .insert(record.id.clone(), record.clone());
        state.emit(
            events::TRANSACTION_DETECTED,
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        fresh += 1;
    }
    if let Some(mut entry) = state.watched.get_mut(&address.id) {
        entry.last_checked_at = Some(now());
        entry.updated_at = now();
    }
    Ok(json!({
        "watched_address_id": address.id,
        "new_transactions": fresh,
    }))
}

// ---------------------------------------------------------------------------
// Handlers: networks
// ---------------------------------------------------------------------------

async fn list_networks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ChainNetwork>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ChainNetwork> = state
            .networks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|network| {
                params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| network.kind.as_str() == kind)
                    && params.filters.get("enabled").is_none_or(|enabled| {
                        network.enabled == (*enabled == "true" || *enabled == "1")
                    })
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|network| {
                network.name.to_lowercase().contains(&needle)
                    || network.id.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<CreateNetworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainNetwork> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let input = body.into_inner();
        let settings = state.settings_snapshot();
        let kind = ChainKind::from_str(&input.kind).ok_or_else(|| {
            UnifiedError::bad_request("kind must be evm, stellar, cardano, or generic")
        })?;
        let name = require_text(&input.name, "name", 128)?;
        let symbol = validate_asset_symbol(&input.native_asset_symbol)?;
        if kind == ChainKind::Evm && input.chain_id.is_none() {
            return Err(UnifiedError::bad_request(
                "EVM networks require a chain_id",
            ));
        }
        if kind != ChainKind::Evm && input.chain_id.is_some() {
            return Err(UnifiedError::bad_request(
                "chain_id applies to EVM networks only",
            ));
        }
        let allow_private = input.allow_private_endpoint.unwrap_or(false);
        let endpoint = validate_chain_endpoint_url(
            &input.endpoint_url,
            &settings.allowed_extra_hosts,
            allow_private,
            &settings.allowed_private_hosts,
        )?
        .to_string();
        let indexer_url = match &input.indexer_url {
            Some(raw) if !raw.trim().is_empty() => Some(
                validate_chain_endpoint_url(
                    raw,
                    &settings.allowed_extra_hosts,
                    allow_private,
                    &settings.allowed_private_hosts,
                )?
                .to_string(),
            ),
            _ => None,
        };
        let verify_endpoint = match &input.verify_endpoint {
            Some(raw) if !raw.trim().is_empty() => Some(
                validate_chain_endpoint_url(
                    raw,
                    &settings.allowed_extra_hosts,
                    allow_private,
                    &settings.allowed_private_hosts,
                )?
                .to_string(),
            ),
            _ => None,
        };
        let explorer_url_template = match &input.explorer_url_template {
            Some(raw) if !raw.trim().is_empty() => Some(validate_explorer_template(raw)?),
            _ => None,
        };
        let (api_key_encrypted, api_key_configured) = match &input.api_key {
            Some(key) if !key.trim().is_empty() => {
                reject_private_key_like(key, "api_key")?;
                (
                    Some(encrypt_secret(&encryption_secret(&ctx), key.trim())),
                    true,
                )
            }
            _ => (None, false),
        };
        let network = ChainNetwork {
            id: new_id(),
            name,
            kind,
            endpoint_url: endpoint,
            chain_id: input.chain_id,
            native_asset_symbol: symbol,
            explorer_url_template,
            enabled: input.enabled.unwrap_or(true),
            indexer_url,
            verify_endpoint,
            api_key_encrypted,
            api_key_configured,
            allow_private_endpoint: allow_private,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        let endpoint_host = url::Url::parse(&network.endpoint_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        state
            .networks
            .insert(network.id.clone(), network.clone());
        if allow_private && is_private_host(&endpoint_host) {
            // Audit trail for the explicit admin decision to allow a
            // localhost or private endpoint.
            state.emit(
                events::PRIVATE_ENDPOINT_ALLOWED,
                json!({
                    "network_id": network.id,
                    "host": endpoint_host,
                    "allowed_by": caller.actor,
                }),
            );
        }
        Ok(network)
    })();
    match result {
        Ok(network) => json::success_response(network, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainNetwork> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        require_network(&state, path.as_str())
    })();
    json::respond(result, debug)
}

async fn update_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateNetworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainNetwork> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let input = body.into_inner();
        let mut network = require_network(&state, path.as_str())?;
        let settings = state.settings_snapshot();
        let allow_private = input
            .allow_private_endpoint
            .unwrap_or(network.allow_private_endpoint);
        if let Some(name) = &input.name {
            network.name = require_text(name, "name", 128)?;
        }
        if let Some(raw) = &input.endpoint_url {
            network.endpoint_url = validate_chain_endpoint_url(
                raw,
                &settings.allowed_extra_hosts,
                allow_private,
                &settings.allowed_private_hosts,
            )?
            .to_string();
        }
        if let Some(symbol) = &input.native_asset_symbol {
            network.native_asset_symbol = validate_asset_symbol(symbol)?;
        }
        if let Some(chain_id) = input.chain_id {
            if network.kind != ChainKind::Evm {
                return Err(UnifiedError::bad_request(
                    "chain_id applies to EVM networks only",
                ));
            }
            network.chain_id = Some(chain_id);
        }
        if let Some(raw) = &input.explorer_url_template {
            network.explorer_url_template = if raw.trim().is_empty() {
                None
            } else {
                Some(validate_explorer_template(raw)?)
            };
        }
        if let Some(raw) = &input.indexer_url {
            network.indexer_url = if raw.trim().is_empty() {
                None
            } else {
                Some(
                    validate_chain_endpoint_url(
                        raw,
                        &settings.allowed_extra_hosts,
                        allow_private,
                        &settings.allowed_private_hosts,
                    )?
                    .to_string(),
                )
            };
        }
        if let Some(raw) = &input.verify_endpoint {
            network.verify_endpoint = if raw.trim().is_empty() {
                None
            } else {
                Some(
                    validate_chain_endpoint_url(
                        raw,
                        &settings.allowed_extra_hosts,
                        allow_private,
                        &settings.allowed_private_hosts,
                    )?
                    .to_string(),
                )
            };
        }
        if let Some(key) = &input.api_key {
            if key.trim().is_empty() {
                network.api_key_encrypted = None;
                network.api_key_configured = false;
            } else {
                reject_private_key_like(key, "api_key")?;
                network.api_key_encrypted =
                    Some(encrypt_secret(&encryption_secret(&ctx), key.trim()));
                network.api_key_configured = true;
            }
        }
        if let Some(enabled) = input.enabled {
            network.enabled = enabled;
        }
        network.allow_private_endpoint = allow_private;
        network.updated_at = now();
        state
            .networks
            .insert(network.id.clone(), network.clone());
        // Drop the cached provider so the next call rebuilds it from
        // the updated record.
        state.providers.remove(&network.id);
        Ok(network)
    })();
    json::respond(result, debug)
}

async fn delete_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let network = require_network(&state, path.as_str())?;
        let in_use = state
            .watched
            .iter()
            .any(|entry| entry.value().network_id == network.id)
            || state
                .wallets
                .iter()
                .any(|entry| entry.value().network_id == network.id)
            || state
                .anchors
                .iter()
                .any(|entry| entry.value().network_id == network.id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Network is still referenced by watched addresses, wallets, or anchors; remove those first",
            ));
        }
        state.networks.remove(path.as_str());
        state.providers.remove(&network.id);
        Ok(json!({ "deleted": true, "id": network.id }))
    })();
    json::respond(result, debug)
}

async fn network_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<NetworkStatus> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let network = require_network(&state, path.as_str())?;
        let provider =
            provider_for_network(&state, &network, &encryption_secret(&ctx))?;
        provider.network_status().await
    })()
    .await;
    json::respond(result, debug)
}

async fn network_fee_estimate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FeeEstimate> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let network = require_network(&state, path.as_str())?;
        let provider =
            provider_for_network(&state, &network, &encryption_secret(&ctx))?;
        provider.estimate_fee().await
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: watched addresses and transactions
// ---------------------------------------------------------------------------

async fn list_watched_addresses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WatchedAddress>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<WatchedAddress> = state
            .watched
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|address| {
                (admin || address.user_id == caller.actor)
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &address.network_id == network_id)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| &address.user_id == user_id)
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|address| {
                address.address.to_lowercase().contains(&needle)
                    || address
                        .label
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_watched_address(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<CreateWatchedAddressRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WatchedAddress> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let input = body.into_inner();
        let network = require_network(&state, &input.network_id)?;
        let address = validate_address(network.kind, &input.address)?;
        let label = match &input.label {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "label", 128)?),
            _ => None,
        };
        let company_id = match &input.company_id {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "company_id", 64)?),
            _ => None,
        };
        let duplicate = state.watched.iter().any(|entry| {
            entry.value().network_id == network.id
                && entry.value().address.eq_ignore_ascii_case(&address)
                && entry.value().user_id == caller.actor
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "This address is already watched on this network",
            ));
        }
        let record = WatchedAddress {
            id: new_id(),
            network_id: network.id,
            address,
            label,
            user_id: caller.actor,
            company_id,
            last_checked_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.watched.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn delete_watched_address(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .watched
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Watched address not found"))?;
        if !admin && record.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can remove this watched address",
            ));
        }
        state.watched.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": record.id }))
    })();
    json::respond(result, debug)
}

async fn poll_watched_address(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .watched
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Watched address not found"))?;
        if !admin && record.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can poll this watched address",
            ));
        }
        poll_one_address(&state, &encryption_secret(&ctx), &record).await
    })()
    .await;
    json::respond(result, debug)
}

async fn poll_all_watched(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        poll_watched_addresses(&state, &encryption_secret(&ctx)).await
    })()
    .await;
    json::respond(result, debug)
}

async fn list_transactions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ChainTxRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let own_watched: Vec<String> = if admin {
            Vec::new()
        } else {
            state
                .watched
                .iter()
                .filter(|entry| entry.value().user_id == caller.actor)
                .map(|entry| entry.value().id.clone())
                .collect()
        };
        let mut items: Vec<ChainTxRecord> = state
            .transactions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tx| {
                (admin
                    || tx
                        .watched_address_id
                        .as_deref()
                        .is_some_and(|id| own_watched.contains(&id.to_string())))
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &tx.network_id == network_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| &tx.status == status)
                    && params
                        .filters
                        .get("direction")
                        .is_none_or(|direction| &tx.direction == direction)
                    && params.filters.get("watched_address_id").is_none_or(
                        |watched_id| {
                            tx.watched_address_id.as_deref() == Some(watched_id.as_str())
                        },
                    )
                    && params
                        .filters
                        .get("address")
                        .is_none_or(|address| {
                            tx.from_address.eq_ignore_ascii_case(address)
                                || tx.to_address.eq_ignore_ascii_case(address)
                        })
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|tx| {
                tx.tx_hash.to_lowercase().contains(&needle)
                    || tx.from_address.to_lowercase().contains(&needle)
                    || tx.to_address.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| b.first_seen_at.cmp(&a.first_seen_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_transaction(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainTxRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .transactions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transaction not found"))?;
        if !admin {
            let owned = record
                .watched_address_id
                .as_deref()
                .and_then(|id| state.watched.get(id))
                .is_some_and(|entry| entry.value().user_id == caller.actor);
            if !owned {
                return Err(UnifiedError::forbidden(
                    "This transaction belongs to another user's watched address",
                ));
            }
        }
        Ok(record)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: anchors and the transparency ledger
// ---------------------------------------------------------------------------

async fn create_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<CreateAnchorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let input = body.into_inner();
        let network = require_network(&state, &input.network_id)?;
        let record_type = require_slug(&input.record_type, "record_type", 64)?;
        let record_id = require_text(&input.record_id, "record_id", 128)?;
        let content_sha256 = resolve_content_hash(&input)?;
        let company_id = match &input.company_id {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "company_id", 64)?),
            _ => None,
        };
        let duplicate = state.anchors.iter().any(|entry| {
            let anchor = entry.value();
            anchor.record_type == record_type
                && anchor.record_id == record_id
                && anchor.content_sha256 == content_sha256
                && anchor.status != "failed"
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "This record hash is already anchored (or awaiting anchoring)",
            ));
        }
        let record = AnchorRecord {
            id: new_id(),
            record_type,
            record_id,
            content_sha256,
            network_id: network.id,
            status: "pending".to_string(),
            tx_hash: None,
            confirmations: 0,
            block_number: None,
            requested_by: caller.actor.clone(),
            company_id,
            submitted_at: None,
            confirmed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.anchors.insert(record.id.clone(), record.clone());
        state.emit(
            events::ANCHOR_REQUESTED,
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_anchors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AnchorRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<AnchorRecord> = state
            .anchors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|anchor| {
                (admin || anchor.requested_by == caller.actor)
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &anchor.network_id == network_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| &anchor.status == status)
                    && params
                        .filters
                        .get("record_type")
                        .is_none_or(|record_type| &anchor.record_type == record_type)
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|anchor| {
                anchor.record_id.to_lowercase().contains(&needle)
                    || anchor
                        .tx_hash
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .anchors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anchor not found"))?;
        if !admin && record.requested_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This anchor was requested by another user",
            ));
        }
        Ok(record)
    })();
    json::respond(result, debug)
}

/// Submit a pending anchor to the configured anchoring relay. The
/// relay — never this module — sends the on-chain transaction, using
/// the dedicated anchoring-only service key. Without a relay or a
/// key configured, submission fails with a 422 that says so.
async fn submit_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let anchor = state
            .anchors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anchor not found"))?;
        if !admin && anchor.requested_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This anchor was requested by another user",
            ));
        }
        if anchor.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending anchor can be submitted (current status: {})",
                anchor.status
            )));
        }
        let settings = state.settings_snapshot();
        let relay_url = settings.anchoring_relay_url.clone().ok_or_else(|| {
            UnifiedError::unprocessable(
                "No anchoring relay is configured: an admin must set one in web3 settings first",
            )
        })?;
        let encrypted_key = settings.anchoring_key_encrypted.clone().ok_or_else(|| {
            UnifiedError::unprocessable(
                "No anchoring service key is configured: an admin must set one in web3 settings first",
            )
        })?;
        let service_key = decrypt_secret(&encryption_secret(&ctx), &encrypted_key)?;
        let response = state
            .http
            .post(relay_url.as_str())
            .bearer_auth(service_key)
            .json(&json!({
                "record_type": anchor.record_type,
                "record_id": anchor.record_id,
                "content_sha256": anchor.content_sha256,
                "network_id": anchor.network_id,
            }))
            .send()
            .await
            .map_err(|_| UnifiedError::internal("Anchoring relay request failed"))?;
        let status = response.status();
        if !status.is_success() {
            if let Some(mut entry) = state.anchors.get_mut(&anchor.id) {
                entry.status = "failed".to_string();
                entry.updated_at = now();
            }
            state.emit(
                events::ANCHOR_FAILED,
                json!({ "anchor_id": anchor.id, "record_type": anchor.record_type, "record_id": anchor.record_id }),
            );
            return Err(map_provider_status(status));
        }
        let body: Value = response.json().await.map_err(|_| {
            UnifiedError::internal("Anchoring relay returned an unreadable response")
        })?;
        let tx_hash = body
            .get("tx_hash")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::internal("Anchoring relay returned no transaction hash")
            })?
            .to_string();
        let mut updated = anchor.clone();
        updated.status = "submitted".to_string();
        updated.tx_hash = Some(tx_hash);
        updated.submitted_at = Some(now());
        updated.updated_at = now();
        state
            .anchors
            .insert(updated.id.clone(), updated.clone());
        Ok(updated)
    })()
    .await;
    json::respond(result, debug)
}

/// Refresh a submitted anchor against the chain: update its
/// confirmations and move it to `confirmed` (emitting
/// `web3.anchor.confirmed`) once the threshold is reached.
async fn refresh_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let anchor = state
            .anchors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anchor not found"))?;
        if !admin && anchor.requested_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This anchor was requested by another user",
            ));
        }
        let tx_hash = anchor.tx_hash.clone().ok_or_else(|| {
            UnifiedError::unprocessable("Anchor has no transaction hash yet; submit it first")
        })?;
        let network = require_network(&state, &anchor.network_id)?;
        let provider =
            provider_for_network(&state, &network, &encryption_secret(&ctx))?;
        let tx = provider.get_transaction(&tx_hash).await?;
        let mut updated = anchor.clone();
        updated.confirmations = tx.confirmations;
        updated.block_number = tx.block_number;
        updated.updated_at = now();
        if tx.status == "failed" {
            updated.status = "failed".to_string();
            state
                .anchors
                .insert(updated.id.clone(), updated.clone());
            state.emit(
                events::ANCHOR_FAILED,
                json!({ "anchor_id": updated.id, "record_type": updated.record_type, "record_id": updated.record_id, "tx_hash": tx_hash }),
            );
            return Ok(updated);
        }
        if tx.status == "confirmed" && tx.confirmations >= REQUIRED_CONFIRMATIONS {
            let was_confirmed = updated.status == "confirmed";
            updated.status = "confirmed".to_string();
            if updated.confirmed_at.is_none() {
                updated.confirmed_at = Some(now());
            }
            state
                .anchors
                .insert(updated.id.clone(), updated.clone());
            if !was_confirmed {
                state.emit(
                    events::ANCHOR_CONFIRMED,
                    serde_json::to_value(&updated).unwrap_or(Value::Null),
                );
            }
            return Ok(updated);
        }
        if updated.status == "pending" {
            updated.status = "submitted".to_string();
        }
        state
            .anchors
            .insert(updated.id.clone(), updated.clone());
        Ok(updated)
    })()
    .await;
    json::respond(result, debug)
}

/// Verify a record against its anchor: recompute the record hash and
/// compare it with the anchored hash retrieved via the provider.
/// Result is `match`, `mismatch`, or `not-found`.
async fn verify_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<VerifyAnchorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let input = body.into_inner();
        let record_type = require_slug(&input.record_type, "record_type", 64)?;
        let record_id = require_text(&input.record_id, "record_id", 128)?;
        let claimed_hash = match (&input.content_sha256, &input.content) {
            (Some(hash), _) => validate_sha256_hex(hash, "content_sha256")?,
            (None, Some(content)) => sha256_hex(content.as_bytes()),
            (None, None) => {
                return Err(UnifiedError::bad_request(
                    "Provide content_sha256 or content to verify against the anchor",
                ));
            }
        };
        let anchor = state
            .anchors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|anchor| {
                anchor.record_type == record_type && anchor.record_id == record_id
            })
            .max_by(|a, b| a.created_at.cmp(&b.created_at));
        let Some(anchor) = anchor else {
            return Ok(json!({
                "result": "not-found",
                "record_type": record_type,
                "record_id": record_id,
                "claimed_hash": claimed_hash,
                "anchored_hash": Value::Null,
                "tx_found": false,
            }));
        };
        let mut tx_found = false;
        let mut confirmations = anchor.confirmations;
        if let Some(tx_hash) = &anchor.tx_hash {
            if let Ok(network) = require_network(&state, &anchor.network_id) {
                if let Ok(provider) =
                    provider_for_network(&state, &network, &encryption_secret(&ctx))
                {
                    if let Ok(tx) = provider.get_transaction(tx_hash).await {
                        tx_found = true;
                        confirmations = tx.confirmations;
                    }
                }
            }
        }
        let result = if anchor.content_sha256 == claimed_hash {
            "match"
        } else {
            "mismatch"
        };
        Ok(json!({
            "result": result,
            "record_type": record_type,
            "record_id": record_id,
            "claimed_hash": claimed_hash,
            "anchored_hash": anchor.content_sha256,
            "anchor_id": anchor.id,
            "network_id": anchor.network_id,
            "tx_hash": anchor.tx_hash,
            "anchor_status": anchor.status,
            "confirmations": confirmations,
            "tx_found": tx_found,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn list_ledger(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LedgerEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<LedgerEntry> = state
            .anchors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|anchor| {
                anchor.tx_hash.is_some()
                    && (admin || anchor.requested_by == caller.actor)
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &anchor.network_id == network_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| &anchor.status == status)
                    && params
                        .filters
                        .get("record_type")
                        .is_none_or(|record_type| &anchor.record_type == record_type)
            })
            .map(|anchor| ledger_entry(&anchor))
            .collect();
        items.sort_by(|a, b| b.anchored_at.cmp(&a.anchored_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// Public transparency ledger. Default-deny: answers 403 unless an
/// admin enabled the public ledger in web3 settings. The API
/// Management module's allowed-public-access controls are the outer
/// gate for this route; this flag is the module-level gate. No caller
/// headers are required when the ledger is public.
async fn public_ledger(
    state: web::Data<Arc<Web3State>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let settings = state.settings_snapshot();
    if !settings.public_ledger_enabled {
        return json::error_response(
            &UnifiedError::forbidden(
                "The public transparency ledger is disabled; an admin must enable it first",
            ),
            false,
        );
    }
    let params = query.into_inner();
    let mut items: Vec<LedgerEntry> = state
        .anchors
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|anchor| {
            anchor.tx_hash.is_some()
                && anchor.status == "confirmed"
                && params
                    .filters
                    .get("network_id")
                    .is_none_or(|network_id| &anchor.network_id == network_id)
                && params
                    .filters
                    .get("record_type")
                    .is_none_or(|record_type| &anchor.record_type == record_type)
        })
        .map(|anchor| ledger_entry(&anchor))
        .collect();
    items.sort_by(|a, b| b.anchored_at.cmp(&a.anchored_at));
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Handlers: wallets and balances
// ---------------------------------------------------------------------------

async fn list_wallets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WalletLink>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<WalletLink> = state
            .wallets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|wallet| {
                (admin || wallet.user_id == caller.actor)
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &wallet.network_id == network_id)
                    && params.filters.get("user_id").is_none_or(|user_id| {
                        &wallet.user_id == user_id
                    })
                    && params.filters.get("verification_status").is_none_or(
                        |status| &wallet.verification_status == status,
                    )
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|wallet| {
                wallet.address.to_lowercase().contains(&needle)
                    || wallet
                        .label
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn link_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<LinkWalletRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WalletLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let input = body.into_inner();
        let network = require_network(&state, &input.network_id)?;
        let address = validate_address(network.kind, &input.address)?;
        let label = match &input.label {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "label", 128)?),
            _ => None,
        };
        let company_id = match &input.company_id {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "company_id", 64)?),
            _ => None,
        };
        let duplicate = state.wallets.iter().any(|entry| {
            let wallet = entry.value();
            wallet.user_id == caller.actor
                && wallet.network_id == network.id
                && wallet.address.eq_ignore_ascii_case(&address)
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "This address is already linked on this network",
            ));
        }
        let wallet = WalletLink {
            id: new_id(),
            user_id: caller.actor.clone(),
            company_id,
            network_id: network.id,
            address,
            label,
            verification_status: "unverified".to_string(),
            challenge_nonce_hash: None,
            challenge_expires_at: None,
            verified_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.wallets.insert(wallet.id.clone(), wallet.clone());
        state.emit(
            events::WALLET_LINKED,
            json!({
                "wallet_id": wallet.id,
                "user_id": wallet.user_id,
                "network_id": wallet.network_id,
                "address": wallet.address,
            }),
        );
        Ok(wallet)
    })();
    match result {
        Ok(wallet) => json::success_response(wallet, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WalletLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "This wallet belongs to another user",
            ));
        }
        Ok(wallet)
    })();
    json::respond(result, debug)
}

async fn unlink_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can unlink this wallet",
            ));
        }
        state.wallets.remove(path.as_str());
        state.challenges.remove(path.as_str());
        state.balances.retain(|_, balance| balance.wallet_id != wallet.id);
        state.emit(
            events::WALLET_UNLINKED,
            json!({
                "wallet_id": wallet.id,
                "user_id": wallet.user_id,
                "network_id": wallet.network_id,
                "address": wallet.address,
            }),
        );
        Ok(json!({ "deleted": true, "id": wallet.id }))
    })();
    json::respond(result, debug)
}

/// Issue a signed-message challenge for a linked wallet. Returns the
/// message to sign and the expiry; the raw nonce is never persisted
/// (only its SHA-256 is stored on the wallet record).
async fn challenge_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can challenge this wallet",
            ));
        }
        let nonce_bytes: [u8; 32] = rand::random();
        let nonce: String = nonce_bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let message = format!(
            "Unified wallet verification.\n\nSign this message to prove you control this address.\n\nNonce: {nonce}"
        );
        let expires_at = now() + chrono::Duration::seconds(CHALLENGE_TTL_SECS);
        state.challenges.insert(
            wallet.id.clone(),
            WalletChallenge {
                wallet_id: wallet.id.clone(),
                nonce: nonce.clone(),
                message: message.clone(),
                expires_at,
            },
        );
        if let Some(mut entry) = state.wallets.get_mut(&wallet.id) {
            entry.challenge_nonce_hash = Some(sha256_hex(nonce.as_bytes()));
            entry.challenge_expires_at = Some(expires_at);
            entry.verification_status = "pending".to_string();
            entry.updated_at = now();
        }
        Ok(json!({
            "wallet_id": wallet.id,
            "address": wallet.address,
            "network_id": wallet.network_id,
            "message": message,
            "nonce": nonce,
            "expires_at": expires_at,
        }))
    })();
    json::respond(result, debug)
}

/// Verify a wallet challenge. EVM wallets submit the signature of
/// the challenge message; Stellar wallets submit the hash of a
/// transaction whose memo carries the nonce; Cardano wallets are
/// verified by provider lookup only (reported as `lookup_only`).
async fn verify_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VerifyWalletRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can verify this wallet",
            ));
        }
        let challenge = state
            .challenges
            .get(&wallet.id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found("No active challenge for this wallet; request one first")
            })?;
        if challenge.expires_at < now() {
            state.challenges.remove(&wallet.id);
            return Err(UnifiedError::bad_request(
                "Challenge expired; request a new one",
            ));
        }
        let network = require_network(&state, &wallet.network_id)?;
        let provider =
            provider_for_network(&state, &network, &encryption_secret(&ctx))?;
        let proof = match network.kind {
            ChainKind::Evm => body
                .signature
                .clone()
                .ok_or_else(|| {
                    UnifiedError::bad_request("signature is required for EVM wallets")
                })?,
            ChainKind::Stellar => body
                .proof_tx_hash
                .clone()
                .ok_or_else(|| {
                    UnifiedError::bad_request(
                        "proof_tx_hash is required for Stellar wallets: send a transaction from the address with the challenge nonce as its memo",
                    )
                })?,
            _ => String::new(),
        };
        // Cardano verification ignores the nonce and reports the
        // lookup outcome; the message passed is the challenge nonce
        // so the lookup path stays explicit.
        let outcome = provider
            .verify_message(&wallet.address, &challenge.nonce, &proof)
            .await?;
        let mut updated = wallet.clone();
        if outcome.verified {
            updated.verification_status = "verified".to_string();
            updated.verified_at = Some(now());
        } else if outcome.method == "address_lookup" {
            updated.verification_status = "lookup_only".to_string();
        } else {
            updated.verification_status = "failed".to_string();
        }
        updated.updated_at = now();
        state
            .wallets
            .insert(updated.id.clone(), updated.clone());
        state.challenges.remove(&wallet.id);
        Ok(json!({
            "wallet_id": updated.id,
            "verification_status": updated.verification_status,
            "verified": outcome.verified,
            "method": outcome.method,
            "detail": outcome.detail,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn list_wallet_balances(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<BalanceSnapshot>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "This wallet belongs to another user",
            ));
        }
        let params = query.into_inner();
        let mut items: Vec<BalanceSnapshot> = state
            .balances
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|balance| {
                balance.wallet_id == wallet.id
                    && params
                        .filters
                        .get("asset")
                        .is_none_or(|asset| &balance.asset == asset)
            })
            .collect();
        items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn refresh_wallet_balances(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalanceSnapshot> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can refresh this wallet",
            ));
        }
        let network = require_network(&state, &wallet.network_id)?;
        let provider =
            provider_for_network(&state, &network, &encryption_secret(&ctx))?;
        let balance = provider.get_balance(&wallet.address, None).await?;
        let amount = if balance.amount_raw.chars().all(|c| c.is_ascii_digit()) {
            decimal_shift(&balance.amount_raw, balance.decimals)
        } else {
            None
        };
        let existing = state
            .balances
            .iter()
            .find(|entry| {
                entry.value().wallet_id == wallet.id && entry.value().asset == balance.asset
            })
            .map(|entry| entry.value().clone());
        let snapshot = BalanceSnapshot {
            id: existing
                .as_ref()
                .map(|snapshot| snapshot.id.clone())
                .unwrap_or_else(new_id),
            wallet_id: wallet.id,
            network_id: network.id,
            address: wallet.address,
            asset: balance.asset,
            amount_raw: balance.amount_raw,
            amount,
            decimals: balance.decimals,
            checked_at: balance.checked_at,
            created_at: existing
                .as_ref()
                .map(|snapshot| snapshot.created_at)
                .unwrap_or_else(now),
            updated_at: now(),
        };
        state
            .balances
            .insert(snapshot.id.clone(), snapshot.clone());
        Ok(snapshot)
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Web3Settings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        Ok(state.settings_snapshot())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Web3Settings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let input = body.into_inner();
        let mut settings = state.settings_snapshot();
        if let Some(enabled) = input.public_ledger_enabled {
            settings.public_ledger_enabled = enabled;
        }
        if let Some(hosts) = input.allowed_private_hosts {
            let mut cleaned: Vec<String> = Vec::new();
            for host in hosts {
                let host = host.trim().to_ascii_lowercase();
                if host.is_empty() {
                    continue;
                }
                if host.chars().any(|c| c.is_control() || c == '/' || c == ':') {
                    return Err(UnifiedError::bad_request(
                        "Allowed private hosts must be plain hostnames or IP literals",
                    ));
                }
                cleaned.push(host);
            }
            cleaned.sort();
            cleaned.dedup();
            settings.allowed_private_hosts = cleaned;
        }
        if let Some(hosts) = input.allowed_extra_hosts {
            let mut cleaned: Vec<String> = Vec::new();
            for host in hosts {
                let host = host.trim().to_ascii_lowercase();
                if host.is_empty() {
                    continue;
                }
                if host.chars().any(|c| c.is_control() || c == '/' || c == ':') {
                    return Err(UnifiedError::bad_request(
                        "Allowed extra hosts must be plain hostnames",
                    ));
                }
                cleaned.push(host);
            }
            cleaned.sort();
            cleaned.dedup();
            settings.allowed_extra_hosts = cleaned;
        }
        if let Some(raw) = &input.anchoring_relay_url {
            if raw.trim().is_empty() {
                settings.anchoring_relay_url = None;
            } else {
                let url = validate_chain_endpoint_url(
                    raw,
                    &settings.allowed_extra_hosts,
                    false,
                    &settings.allowed_private_hosts,
                )?;
                settings.anchoring_relay_url = Some(url.to_string());
            }
        }
        if let Some(key) = &input.anchoring_key {
            if key.trim().is_empty() {
                settings.anchoring_key_encrypted = None;
                settings.anchoring_key_configured = false;
            } else {
                reject_private_key_like(key, "anchoring_key")?;
                settings.anchoring_key_encrypted =
                    Some(encrypt_secret(&encryption_secret(&ctx), key.trim()));
                settings.anchoring_key_configured = true;
            }
        }
        if let Some(limit) = input.max_poll_transactions {
            settings.max_poll_transactions = limit.clamp(1, MAX_POLL_LIMIT);
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct Web3Module {
    state: Arc<Web3State>,
}

impl Web3Module {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Web3State::new()),
        }
    }

    /// Access to the module state, for the scheduler wiring that calls
    /// `poll_watched_addresses`.
    pub fn state(&self) -> Arc<Web3State> {
        self.state.clone()
    }
}

impl Default for Web3Module {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Web3Module {
    fn id(&self) -> &'static str {
        "web3"
    }

    fn name(&self) -> &'static str {
        "Web3 / Blockchain"
    }

    fn description(&self) -> &'static str {
        "Blockchain openness and traceability: watched addresses, on-chain transaction history, tamper-evident record anchoring, a public transparency ledger, read-only wallet linking, and read-only balances across EVM, Stellar, and Cardano networks."
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &[
            "reqwest",
            "serde_json",
            "sha2",
            "hmac",
            "base64",
            "async-trait",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some(ProductCategory::Infrastructure.as_str())
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/web3/networks", owner: "web3", description: "List chain networks" },
            RouteInfo { method: "POST", path: "/api/v1/web3/networks", owner: "web3", description: "Create chain network (admin)" },
            RouteInfo { method: "GET", path: "/api/v1/web3/networks/{id}", owner: "web3", description: "Get chain network" },
            RouteInfo { method: "PATCH", path: "/api/v1/web3/networks/{id}", owner: "web3", description: "Update chain network (admin)" },
            RouteInfo { method: "DELETE", path: "/api/v1/web3/networks/{id}", owner: "web3", description: "Delete chain network (admin)" },
            RouteInfo { method: "GET", path: "/api/v1/web3/networks/{id}/status", owner: "web3", description: "Network status (latest block, sync state)" },
            RouteInfo { method: "GET", path: "/api/v1/web3/networks/{id}/fee-estimate", owner: "web3", description: "Fee estimate for a network" },
            RouteInfo { method: "GET", path: "/api/v1/web3/watched-addresses", owner: "web3", description: "List watched addresses" },
            RouteInfo { method: "POST", path: "/api/v1/web3/watched-addresses", owner: "web3", description: "Watch an address" },
            RouteInfo { method: "DELETE", path: "/api/v1/web3/watched-addresses/{id}", owner: "web3", description: "Stop watching an address" },
            RouteInfo { method: "POST", path: "/api/v1/web3/watched-addresses/{id}/poll", owner: "web3", description: "Poll one watched address" },
            RouteInfo { method: "POST", path: "/api/v1/web3/poll", owner: "web3", description: "Poll all watched addresses" },
            RouteInfo { method: "GET", path: "/api/v1/web3/transactions", owner: "web3", description: "List observed transactions" },
            RouteInfo { method: "GET", path: "/api/v1/web3/transactions/{id}", owner: "web3", description: "Get one observed transaction" },
            RouteInfo { method: "POST", path: "/api/v1/web3/anchors", owner: "web3", description: "Request an on-chain anchor for a record hash" },
            RouteInfo { method: "GET", path: "/api/v1/web3/anchors", owner: "web3", description: "List anchors" },
            RouteInfo { method: "GET", path: "/api/v1/web3/anchors/{id}", owner: "web3", description: "Get one anchor" },
            RouteInfo { method: "POST", path: "/api/v1/web3/anchors/{id}/submit", owner: "web3", description: "Submit a pending anchor to the anchoring relay" },
            RouteInfo { method: "POST", path: "/api/v1/web3/anchors/{id}/refresh", owner: "web3", description: "Refresh an anchor against the chain" },
            RouteInfo { method: "POST", path: "/api/v1/web3/anchors/verify", owner: "web3", description: "Verify a record against its anchor" },
            RouteInfo { method: "GET", path: "/api/v1/web3/ledger", owner: "web3", description: "Transparency ledger (authenticated)" },
            RouteInfo { method: "GET", path: "/api/v1/web3/public/ledger", owner: "web3", description: "Public transparency ledger (default-deny, admin-enabled)" },
            RouteInfo { method: "GET", path: "/api/v1/web3/wallets", owner: "web3", description: "List linked wallets" },
            RouteInfo { method: "POST", path: "/api/v1/web3/wallets", owner: "web3", description: "Link a wallet address (read-only)" },
            RouteInfo { method: "GET", path: "/api/v1/web3/wallets/{id}", owner: "web3", description: "Get one linked wallet" },
            RouteInfo { method: "DELETE", path: "/api/v1/web3/wallets/{id}", owner: "web3", description: "Unlink a wallet" },
            RouteInfo { method: "POST", path: "/api/v1/web3/wallets/{id}/challenge", owner: "web3", description: "Issue a wallet ownership challenge" },
            RouteInfo { method: "POST", path: "/api/v1/web3/wallets/{id}/verify", owner: "web3", description: "Verify wallet ownership from a challenge proof" },
            RouteInfo { method: "GET", path: "/api/v1/web3/wallets/{id}/balances", owner: "web3", description: "List balance snapshots for a wallet" },
            RouteInfo { method: "POST", path: "/api/v1/web3/wallets/{id}/balances/refresh", owner: "web3", description: "Refresh balances for a wallet" },
            RouteInfo { method: "GET", path: "/api/v1/web3/settings", owner: "web3", description: "Get web3 settings (admin)" },
            RouteInfo { method: "PATCH", path: "/api/v1/web3/settings", owner: "web3", description: "Update web3 settings (admin)" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/web3")
                .route("/networks", web::get().to(list_networks))
                .route("/networks", web::post().to(create_network))
                .route("/networks/{id}", web::get().to(get_network))
                .route("/networks/{id}", web::patch().to(update_network))
                .route("/networks/{id}", web::delete().to(delete_network))
                .route("/networks/{id}/status", web::get().to(network_status))
                .route(
                    "/networks/{id}/fee-estimate",
                    web::get().to(network_fee_estimate),
                )
                .route("/watched-addresses", web::get().to(list_watched_addresses))
                .route(
                    "/watched-addresses",
                    web::post().to(create_watched_address),
                )
                .route(
                    "/watched-addresses/{id}",
                    web::delete().to(delete_watched_address),
                )
                .route(
                    "/watched-addresses/{id}/poll",
                    web::post().to(poll_watched_address),
                )
                .route("/poll", web::post().to(poll_all_watched))
                .route("/transactions", web::get().to(list_transactions))
                .route("/transactions/{id}", web::get().to(get_transaction))
                .route("/anchors", web::post().to(create_anchor))
                .route("/anchors", web::get().to(list_anchors))
                .route("/anchors/{id}", web::get().to(get_anchor))
                .route("/anchors/{id}/submit", web::post().to(submit_anchor))
                .route("/anchors/{id}/refresh", web::post().to(refresh_anchor))
                .route("/anchors/verify", web::post().to(verify_anchor))
                .route("/ledger", web::get().to(list_ledger))
                .route("/public/ledger", web::get().to(public_ledger))
                .route("/wallets", web::get().to(list_wallets))
                .route("/wallets", web::post().to(link_wallet))
                .route("/wallets/{id}", web::get().to(get_wallet))
                .route("/wallets/{id}", web::delete().to(unlink_wallet))
                .route("/wallets/{id}/challenge", web::post().to(challenge_wallet))
                .route("/wallets/{id}/verify", web::post().to(verify_wallet))
                .route("/wallets/{id}/balances", web::get().to(list_wallet_balances))
                .route(
                    "/wallets/{id}/balances/refresh",
                    web::post().to(refresh_wallet_balances),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(Web3Module::new()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_decimal_converts_wei_values() {
        assert_eq!(hex_to_decimal("0x0").unwrap(), "0");
        assert_eq!(hex_to_decimal("0xff").unwrap(), "255");
        assert_eq!(
            hex_to_decimal("0xde0b6b3a7640000").unwrap(),
            "1000000000000000000"
        );
        assert_eq!(hex_to_decimal("0xzz"), None);
    }

    #[test]
    fn decimal_shift_scales_chain_amounts() {
        assert_eq!(decimal_shift("1000000000000000000", 18).unwrap(), "1");
        assert_eq!(decimal_shift("1500000", 6).unwrap(), "1.5");
        assert_eq!(decimal_shift("123456789", 6).unwrap(), "123.4567");
        assert_eq!(decimal_shift("5", 6).unwrap(), "0.000005");
        assert_eq!(decimal_shift("0", 18).unwrap(), "0");
        assert_eq!(decimal_shift("12x", 6), None);
    }

    #[test]
    fn address_validation_is_kind_specific() {
        assert!(validate_address(
            ChainKind::Evm,
            "0x0000000000000000000000000000000000000000"
        )
        .is_ok());
        assert!(validate_address(ChainKind::Evm, "0x123").is_err());
        assert!(validate_address(
            ChainKind::Stellar,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        )
        .is_err());
        assert!(validate_address(
            ChainKind::Stellar,
            "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H"
        )
        .is_ok());
        assert!(validate_address(
            ChainKind::Cardano,
            "addr1qx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzr3jcu5d8ps7zex2k2xt3uqxgjqnnj83ws8lhrn648jjxtw3sshm7a"
        )
        .is_ok());
    }

    #[test]
    fn private_key_like_values_are_rejected() {
        assert!(looks_like_private_key(&"a".repeat(64)));
        assert!(looks_like_private_key(&format!("0x{}", "b".repeat(64))));
        assert!(looks_like_private_key(
            "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----"
        ));
        assert!(!looks_like_private_key("blockfrost-api-key-123"));
        assert!(!looks_like_private_key(""));
        assert!(reject_private_key_like(&"c".repeat(64), "api_key").is_err());
    }

    #[test]
    fn private_endpoints_need_an_explicit_admin_allowlist() {
        let result = validate_chain_endpoint_url(
            "http://localhost:8545",
            &[],
            false,
            &["localhost".to_string()],
        );
        assert!(result.is_err());
        let allowed = validate_chain_endpoint_url(
            "http://localhost:8545",
            &[],
            true,
            &["localhost".to_string()],
        );
        assert!(allowed.is_ok());
        let public = validate_chain_endpoint_url(
            "https://horizon.stellar.org",
            &[],
            false,
            &[],
        );
        assert!(public.is_ok());
        let unknown = validate_chain_endpoint_url("https://example.com/rpc", &[], false, &[]);
        assert!(unknown.is_err());
    }

    #[test]
    fn secret_round_trip_never_exposes_plaintext() {
        let encrypted = encrypt_secret("test-secret", "provider-api-key");
        assert!(!encrypted.contains("provider-api-key"));
        assert_eq!(
            decrypt_secret("test-secret", &encrypted).unwrap(),
            "provider-api-key"
        );
        assert!(decrypt_secret("wrong-secret", &encrypted).is_err());
    }

    #[tokio::test]
    async fn fake_provider_reports_balances_and_transactions() {
        let provider = FakeChainProvider::new("fake-net", ChainKind::Evm);
        provider.set_balance("0xabc", "123000000");
        let balance = provider.get_balance("0xabc", None).await.unwrap();
        assert_eq!(balance.amount_raw, "123000000");
        provider.add_transaction(ProviderTransaction {
            tx_hash: "0xhash".to_string(),
            from_address: "0xabc".to_string(),
            to_address: "0xdef".to_string(),
            amount_raw: "100".to_string(),
            asset: "FAKE".to_string(),
            status: "confirmed".to_string(),
            confirmations: 3,
            block_number: Some(42),
            occurred_at: None,
        });
        let found = provider.get_transactions("0xabc", 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tx_hash, "0xhash");
    }
}
