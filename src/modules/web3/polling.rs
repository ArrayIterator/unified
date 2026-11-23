// web3 -- Watched-address polling (web3.poll).

use unified_core::error::UnifiedResult;
use unified_core::hooks::HookContext;
use dashmap::mapref::entry::Entry;
use serde_json::{Value, json};
use std::sync::Arc;

use super::constants::*;
use super::events;
use super::helpers::*;
use super::providers::*;
use super::state::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Transaction dedup (W9)
// ---------------------------------------------------------------------------

/// Normalise a transaction hash for dedup: trimmed, lowercased.
/// Providers return the same hash in different cases (EVM hashes
/// are hex, Stellar hashes are hex), and a case-sensitive check
/// would store the same transaction twice.
pub(crate) fn normalise_tx_hash(hash: &str) -> String {
    hash.trim().to_ascii_lowercase()
}

/// Insert a transaction record unless a record for the same network
/// with the same hash (case-insensitive) is already stored. Returns
/// `true` when the record was newly stored. The check is keyed on
/// the insert itself: the (`network_id`, lowercased hash) entry is
/// claimed atomically, so a concurrent scheduler poll and a manual
/// poll racing on the same transaction store it exactly once and
/// only the winner emits `web3.transaction.detected`.
pub(crate) fn insert_tx_if_new(state: &Arc<Web3State>, record: &ChainTxRecord) -> bool {
    let key = (
        record.network_id.clone(),
        normalise_tx_hash(&record.tx_hash),
    );
    match state.transactions_by_hash.entry(key) {
        Entry::Occupied(_) => false,
        Entry::Vacant(slot) => {
            slot.insert(record.id.clone());
            state
                .transactions
                .insert(record.id.clone(), record.clone());
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Polling (web3.poll) -- called by the scheduler, not by this module
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
            let found = provider.get_transactions(&address.address, limit).await?;
            let mut fresh = 0usize;
            for tx in found {
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
                    tx_hash: normalise_tx_hash(&tx.tx_hash),
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
                // Dedup is keyed on this insert (case-insensitive);
                // a duplicate seen by a concurrent poll is skipped
                // without re-emitting the event.
                if !insert_tx_if_new(state, &record) {
                    continue;
                }
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

/// Run the watched-address poll for `web3.poll_requested`, the
/// event the scheduler dispatches for the `web3.poll` job kind.
/// Hook handlers are synchronous but the poll is async network
/// work, so — like the geo update hook — it runs on a detached
/// thread with its own current-thread runtime. The hook result only
/// records that the poll started; the summary is logged when the
/// poll finishes, and each new transaction is still emitted as
/// `web3.transaction.detected` from the poll itself.
pub(crate) fn on_poll_requested(state: Arc<Web3State>, ctx: &mut HookContext) {
    let Some(app_ctx) = WEB3_CTX.get().cloned() else {
        ctx.add_result(json!({
            "web3": "poll",
            "started": false,
            "error": "Web3 module context is not initialised",
        }));
        return;
    };
    let secret = encryption_secret(&app_ctx).unwrap_or_default();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            log::error!("Web3 poll: cannot start async runtime");
            return;
        };
        match runtime.block_on(poll_watched_addresses(&state, &secret)) {
            Ok(summary) => log::info!("Web3 poll finished: {summary}"),
            Err(error) => log::error!("Web3 poll failed: {}", error.message),
        }
    });
    ctx.add_result(json!({
        "web3": "poll",
        "started": true,
    }));
}

pub(crate) async fn poll_one_address(
    state: &Arc<Web3State>,
    encryption_secret_value: &str,
    address: &WatchedAddress,
) -> UnifiedResult<Value> {
    let settings = state.settings_snapshot();
    let limit = settings.max_poll_transactions.clamp(1, MAX_POLL_LIMIT);
    let network = require_network(state, &address.network_id)?;
    let provider = provider_for_network(state, &network, encryption_secret_value)?;
    let found = provider.get_transactions(&address.address, limit).await?;
    let mut fresh = 0usize;
    for tx in found {
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
            tx_hash: normalise_tx_hash(&tx.tx_hash),
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
        // Dedup is keyed on this insert (case-insensitive); a
        // duplicate seen by a concurrent poll is skipped without
        // re-emitting the event.
        if !insert_tx_if_new(state, &record) {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(network_id: &str, tx_hash: &str) -> ChainTxRecord {
        ChainTxRecord {
            id: new_id(),
            network_id: network_id.to_string(),
            tx_hash: tx_hash.to_string(),
            from_address: "0xabc".to_string(),
            to_address: "0xdef".to_string(),
            amount_raw: "100".to_string(),
            amount: None,
            asset: "ETH".to_string(),
            status: "confirmed".to_string(),
            confirmations: 3,
            block_number: Some(42),
            direction: "incoming".to_string(),
            watched_address_id: None,
            occurred_at: None,
            first_seen_at: now(),
            last_checked_at: now(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn tx_hash_normalisation_is_case_insensitive() {
        assert_eq!(normalise_tx_hash("0xABCDEF"), "0xabcdef");
        assert_eq!(normalise_tx_hash("  0xAbC  "), "0xabc");
    }

    #[test]
    fn duplicate_hash_in_another_case_is_inserted_once() {
        let state = Arc::new(Web3State::new());
        let first = test_record("net-1", "0xABCDEF");
        assert!(insert_tx_if_new(&state, &first));
        // Same hash, different case: a provider returning it again
        // must not insert a duplicate or re-emit the event.
        let second = test_record("net-1", "0xabcdef");
        assert!(!insert_tx_if_new(&state, &second));
        assert_eq!(state.transactions.len(), 1);
        assert_eq!(state.transactions_by_hash.len(), 1);
    }

    #[test]
    fn same_hash_on_another_network_is_a_distinct_transaction() {
        let state = Arc::new(Web3State::new());
        assert!(insert_tx_if_new(&state, &test_record("net-1", "0xabc")));
        assert!(insert_tx_if_new(&state, &test_record("net-2", "0xABC")));
        assert_eq!(state.transactions.len(), 2);
    }
}
