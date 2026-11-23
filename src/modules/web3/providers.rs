// web3 -- Chain provider trait, registry, HTTP helpers and construction.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::storage::sha256_hex;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::handlers_networks::*;
use super::handlers_watched::*;
use super::helpers::*;
use super::module::*;
use super::providers_cardano::*;
use super::providers_evm::*;
use super::providers_stellar::*;
use super::state::*;
use super::types::*;
use super::validation::*;

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
// Provider HTTP helpers (generic errors only -- never echo bodies)
// ---------------------------------------------------------------------------

pub(crate) async fn provider_get(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
) -> UnifiedResult<Value> {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .map_err(|_| UnifiedError::internal("Chain provider request failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(map_provider_status(status));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| UnifiedError::internal("Chain provider returned an unreadable response"))
}

pub(crate) async fn provider_post_json(
    client: &reqwest::Client,
    url: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> UnifiedResult<Value> {
    let mut request = client.post(url).json(&body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .map_err(|_| UnifiedError::internal("Chain provider request failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(map_provider_status(status));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| UnifiedError::internal("Chain provider returned an unreadable response"))
}

pub(crate) fn map_provider_status(status: reqwest::StatusCode) -> UnifiedError {
    match status.as_u16() {
        404 => UnifiedError::not_found("Chain provider resource not found"),
        401 => UnifiedError::unauthorized("Chain provider rejected the credentials"),
        403 => UnifiedError::forbidden("Chain provider refused the request"),
        429 => UnifiedError::new("Chain provider rate limit reached", 429),
        _ => UnifiedError::internal(format!("Chain provider request failed with HTTP {status}")),
    }
}

pub(crate) fn provider_tx(
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

pub(crate) fn parse_unix_ts(value: &Value) -> Option<DateTime<Utc>> {
    let secs = match value {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.parse::<i64>().ok()?,
        _ => return None,
    };
    DateTime::from_timestamp(secs, 0)
}

// ---------------------------------------------------------------------------
// Provider construction from a stored network record
// ---------------------------------------------------------------------------

/// Build a provider for a stored network, reusing the registry
/// entry when one is cached. The provider API key is decrypted in
/// memory for this call only and never stored in plain text.
/// Generic networks have no provider implementation.
pub(crate) fn provider_for_network(
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
    state
        .providers
        .register(network.id.clone(), provider.clone());
    Ok(provider)
}

pub(crate) fn require_network(
    state: &Arc<Web3State>,
    network_id: &str,
) -> UnifiedResult<ChainNetwork> {
    state
        .networks
        .get(network_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Network not found"))
}

/// Resolve the claimed content hash for an anchor request: either a
/// supplied SHA-256 or the hash of supplied raw content.
pub(crate) fn resolve_content_hash(input: &CreateAnchorRequest) -> UnifiedResult<String> {
    match (&input.content_sha256, &input.content) {
        (Some(hash), _) => validate_sha256_hex(hash, "content_sha256"),
        (None, Some(content)) => Ok(sha256_hex(content.as_bytes())),
        (None, None) => Err(UnifiedError::bad_request(
            "Provide content_sha256 or content so the record hash can be anchored",
        )),
    }
}

pub(crate) fn ledger_entry(anchor: &AnchorRecord) -> LedgerEntry {
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
