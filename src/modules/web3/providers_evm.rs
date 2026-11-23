// web3 -- EVM provider (JSON-RPC and indexer).

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::constants::*;
use super::handlers_networks::*;
use super::handlers_watched::*;
use super::helpers::*;
use super::providers::*;
use super::types::*;
use super::validation::*;

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

    pub(crate) async fn rpc(&self, method: &str, params: Value) -> UnifiedResult<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let value = provider_post_json(&self.client, &self.rpc_url, body, &[]).await?;
        if let Some(error) = value.get("error") {
            let _ = error;
            return Err(UnifiedError::internal("Chain provider request failed"));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| UnifiedError::internal("Chain provider returned no result"))
    }

    pub(crate) async fn latest_block(&self) -> UnifiedResult<u64> {
        let result = self.rpc("eth_blockNumber", json!([])).await?;
        result
            .as_str()
            .and_then(parse_hex_u64)
            .ok_or_else(|| UnifiedError::internal("Chain provider returned no block number"))
    }

    pub(crate) async fn indexer_get(&self, query: &[(String, String)]) -> UnifiedResult<Value> {
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

    async fn get_balance(&self, address: &str, asset: Option<&str>) -> UnifiedResult<ChainBalance> {
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
            let is_error = entry.get("isError").and_then(|v| v.as_str()).unwrap_or("0") == "1";
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
            return Err(UnifiedError::not_found(
                "Transaction not found on this network",
            ));
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
