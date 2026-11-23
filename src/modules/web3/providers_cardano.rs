// web3 -- Cardano provider (Blockfrost- or Koios-style REST).

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use chrono::DateTime;
use serde_json::{Value, json};

use super::constants::*;
use super::handlers_networks::*;
use super::handlers_watched::*;
use super::helpers::*;
use super::providers::*;
use super::types::*;

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

    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = Vec::new();
        if self.api_style == "blockfrost" {
            if let Some(key) = &self.api_key {
                headers.push(("project_id".to_string(), key.clone()));
            }
        }
        headers
    }

    pub(crate) async fn get(&self, path: &str) -> UnifiedResult<Value> {
        let headers = self.headers();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        provider_get(&self.client, &format!("{}{}", self.base_url, path), &refs).await
    }

    pub(crate) async fn post(&self, path: &str, body: Value) -> UnifiedResult<Value> {
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

    pub(crate) fn is_blockfrost(&self) -> bool {
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
                tip.get("hash").and_then(|v| v.as_str()).map(str::to_string),
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

    async fn get_balance(&self, address: &str, asset: Option<&str>) -> UnifiedResult<ChainBalance> {
        if self.is_blockfrost() {
            let info = self.get(&format!("/addresses/{address}")).await?;
            let amounts = info
                .get("amount")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let wanted = asset.map(|a| a.to_ascii_uppercase());
            for entry in &amounts {
                let unit = entry.get("unit").and_then(|v| v.as_str()).unwrap_or("");
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
                .ok_or_else(|| UnifiedError::not_found("Cardano address not found"))?;
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
                    if let Some(outputs) = utxos.get("outputs").and_then(|v| v.as_array()) {
                        let mut total: u128 = 0;
                        for output in outputs {
                            if output
                                .get("address")
                                .and_then(|v| v.as_str())
                                .map(|a| a == address)
                                .unwrap_or(false)
                            {
                                if let Some(amounts) =
                                    output.get("amount").and_then(|v| v.as_array())
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
            Ok(info.as_array().is_some_and(|list| !list.is_empty()))
        }
    }
}
