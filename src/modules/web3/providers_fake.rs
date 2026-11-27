// web3 -- Fake provider (tests only).

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use std::collections::HashMap;

use super::handlers_networks::*;
use super::handlers_watched::*;
use super::helpers::*;
use super::providers::*;
use super::types::*;

// Fake provider (tests only)

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

    async fn get_balance(&self, address: &str, asset: Option<&str>) -> UnifiedResult<ChainBalance> {
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
