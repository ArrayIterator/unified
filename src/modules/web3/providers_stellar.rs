// web3 -- Stellar provider (Horizon API).

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::constants::*;
use super::handlers_networks::*;
use super::handlers_watched::*;
use super::helpers::*;
use super::providers::*;
use super::types::*;
use super::validation::*;

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

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub(crate) async fn account(&self, address: &str) -> UnifiedResult<Value> {
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
        let root = provider_get(
            &self.client,
            &self.url("/"),
            &[("Accept", "application/json")],
        )
        .await?;
        let latest = root
            .get("core_latest_ledger")
            .and_then(|v| v.as_u64())
            .or_else(|| root.get("history_latest_ledger").and_then(|v| v.as_u64()));
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

    async fn get_balance(&self, address: &str, asset: Option<&str>) -> UnifiedResult<ChainBalance> {
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
                        let op_type = operation.get("type").and_then(|v| v.as_str()).unwrap_or("");
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
        // Ownership proof: a transaction from the address whose text
        // memo carries the challenge. `message` is normally the
        // 28-character memo token derived from the challenge nonce
        // (see `stellar_memo_token`); a full 64-character nonce can
        // never fit in a Stellar text memo (28 bytes max), so when a
        // caller passes the raw nonce instead, the memo is compared
        // against the token derived from it as well. A hash memo is
        // base64, not hex, and is not used here.
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
        let memo = record.get("memo").and_then(|v| v.as_str()).unwrap_or("");
        let expected = stellar_memo_token(message);
        if source == address && (memo == message || memo == expected) {
            Ok(VerificationOutcome {
                verified: true,
                method: "memo_transaction".to_string(),
                detail: "A transaction from this address carries the challenge memo token in its memo."
                    .to_string(),
            })
        } else {
            Ok(VerificationOutcome {
                verified: false,
                method: "memo_transaction".to_string(),
                detail: "No transaction from this address carries the challenge memo token in its memo."
                    .to_string(),
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
