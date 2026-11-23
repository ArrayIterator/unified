//! panels module - Collection fetching and two-way sync.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::adapter::PanelAdapter;
use super::adapter::payload_hash;
use super::adapter::unsupported;
use super::constants::COLLECTIONS;
use super::constants::PANEL_CLIENT_SYNCED;
use super::constants::PANEL_SERVICE_SYNCED;
use super::constants::PANEL_SYNC_COMPLETED;
use super::constants::PANEL_SYNC_CONFLICT;
use super::constants::PANEL_SYNC_FAILED;
use super::registry::panel_registry;
use super::store::PanelSyncState;
use super::store::RecordKey;
use super::store::SyncDirection;
use super::store::SyncedRecord;
use super::store::connections_store;
use super::store::records_store;
use super::store::sync_state_store;
use super::types::PanelConnection;
use super::types::RemoteRecord;

// ---------------------------------------------------------------------------
// Live fetch dispatch and module-level sync.
// ---------------------------------------------------------------------------

/// Fetch one collection live from an adapter, as generic records.
pub(crate) async fn fetch_collection(
    adapter: &Arc<dyn PanelAdapter>,
    collection: &str,
) -> UnifiedResult<Vec<RemoteRecord>> {
    match collection {
        "clients" => Ok(adapter
            .list_clients()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(
                    item.id.clone(),
                    item.name.clone(),
                    item.status.clone(),
                    &item,
                )
            })
            .collect()),
        "products" => Ok(adapter
            .list_products()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(
                    item.id.clone(),
                    item.name.clone(),
                    item.status.clone(),
                    &item,
                )
            })
            .collect()),
        "services" => Ok(adapter
            .list_services()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(
                    item.id.clone(),
                    item.name.clone(),
                    item.status.clone(),
                    &item,
                )
            })
            .collect()),
        "domains" => Ok(adapter
            .list_domains()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(
                    item.id.clone(),
                    item.name.clone(),
                    item.status.clone(),
                    &item,
                )
            })
            .collect()),
        "invoices" => Ok(adapter
            .list_invoices()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(
                    item.id.clone(),
                    format!("Invoice {}", item.id),
                    item.status.clone(),
                    &item,
                )
            })
            .collect()),
        "server_accounts" => Ok(adapter
            .list_server_accounts()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(
                    item.id.clone(),
                    item.username.clone(),
                    item.status.clone(),
                    &item,
                )
            })
            .collect()),
        _ => Err(UnifiedError::bad_request(format!(
            "Unknown collection \"{collection}\""
        ))),
    }
}

/// Outcome of syncing one collection into the in-memory store.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CollectionSyncOutcome {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    /// Records whose local version explicitly won over the remote one.
    pub local_wins: usize,
}

/// Result of a module-level sync run (one or all collections).
#[derive(Debug, Clone, Serialize)]
pub struct PanelSyncResult {
    pub connection_id: Uuid,
    pub kind: String,
    pub synced_at: DateTime<Utc>,
    /// Records fetched per collection.
    pub counts: HashMap<String, usize>,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub local_wins: usize,
    /// Per-collection fetch errors (empty when every collection
    /// succeeded). A run with errors for some collections still
    /// stores the collections that succeeded.
    pub errors: HashMap<String, String>,
    /// True when every supported collection synced without error.
    pub success: bool,
}

pub(crate) fn emit_record_synced(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
    collection: &str,
    record: &RemoteRecord,
) {
    let event = match collection {
        "clients" => Some(PANEL_CLIENT_SYNCED),
        "services" => Some(PANEL_SERVICE_SYNCED),
        _ => None,
    };
    if let Some(event) = event {
        ctx.hooks.emit(
            event,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-{collection}:{}", record.remote_id),
                "remote_id": record.remote_id,
                "name": record.name,
            }),
        );
    }
}

/// Apply live records for one collection to the in-memory store,
/// resolving two-way state per record. Never silently overwrites a
/// locally changed record: a remote change to a locally changed
/// record becomes a conflict (the stored payload is kept) unless the
/// record is flagged `prefer_local`, in which case the local version
/// wins and the conflict is counted as a local win.
pub(crate) fn apply_records(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
    collection: &str,
    records: Vec<RemoteRecord>,
) -> CollectionSyncOutcome {
    let mut outcome = CollectionSyncOutcome::default();
    let now = Utc::now();
    for record in &records {
        let hash = payload_hash(&record.payload);
        let key: RecordKey = (
            connection.id,
            collection.to_string(),
            record.remote_id.clone(),
        );
        match records_store().get_mut(&key) {
            None => {
                records_store().insert(
                    key,
                    SyncedRecord {
                        connection_id: connection.id,
                        collection: collection.to_string(),
                        remote_id: record.remote_id.clone(),
                        name: record.name.clone(),
                        status: record.status.clone(),
                        hash,
                        last_synced_at: now,
                        direction: SyncDirection::RemoteToLocal.as_str().to_string(),
                        conflict: false,
                        local_changed: false,
                        prefer_local: false,
                        payload: record.payload.clone(),
                    },
                );
                outcome.inserted += 1;
                emit_record_synced(ctx, connection, collection, record);
            }
            Some(mut stored) => {
                if stored.hash == hash && !stored.local_changed {
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::Unchanged.as_str().to_string();
                    stored.conflict = false;
                    outcome.unchanged += 1;
                } else if stored.hash == hash && stored.local_changed {
                    // The remote record matches the last synced
                    // payload, but a local change is still flagged:
                    // keep the flag and the stored payload so the
                    // local change is not lost.
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
                    outcome.unchanged += 1;
                } else if stored.prefer_local {
                    // Local version explicitly wins: keep the stored
                    // payload, clear the change flag, log the win.
                    // `prefer_local` alone suffices here (the flag
                    // is not also required): a local-wins resolution
                    // sets `prefer_local` and must survive the next
                    // sync even if `local_changed` was cleared by
                    // the resolution itself.
                    stored.local_changed = false;
                    stored.prefer_local = false;
                    stored.conflict = false;
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
                    outcome.local_wins += 1;
                } else if stored.local_changed {
                    // Both sides changed and the local side was not
                    // explicitly preferred: conflict, remote payload
                    // is NOT applied.
                    stored.conflict = true;
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::Conflict.as_str().to_string();
                    outcome.conflicts += 1;
                    ctx.hooks.emit(
                        PANEL_SYNC_CONFLICT,
                        serde_json::json!({
                            "actor": "sync",
                            "connection_id": connection.id,
                            "kind": connection.kind.as_str(),
                            "resource": format!("panel-{collection}:{}", record.remote_id),
                            "collection": collection,
                            "remote_id": record.remote_id,
                        }),
                    );
                } else {
                    stored.hash = hash;
                    stored.name = record.name.clone();
                    stored.status = record.status.clone();
                    stored.payload = record.payload.clone();
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::RemoteToLocal.as_str().to_string();
                    stored.conflict = false;
                    outcome.updated += 1;
                    emit_record_synced(ctx, connection, collection, record);
                }
            }
        }
    }
    outcome
}

/// Whether a single-collection sync counts as successful: the
/// fetch ran, and no record ended in conflict. Conflicts are
/// reported in the outcome, not hidden behind a blanket `true`.
pub(crate) fn collection_sync_success(outcome: &CollectionSyncOutcome) -> bool {
    outcome.conflicts == 0
}

/// The `last_synced_at` a sync run should stamp: the run time when
/// at least one collection succeeded, otherwise the previously
/// stamped value (a run where every collection failed must not
/// pretend a sync happened).
pub(crate) fn next_last_synced_at(
    any_success: bool,
    now: DateTime<Utc>,
    previous: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    if any_success {
        Some(now)
    } else {
        previous
    }
}

/// Sync one collection: fetch live, apply to the store, and return
/// the outcome. A fetch failure is returned as an error and nothing
/// is stored for that collection.
pub(crate) async fn sync_one_collection(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
    collection: &str,
) -> UnifiedResult<CollectionSyncOutcome> {
    let adapter = panel_registry().build(connection)?;
    if !adapter.capabilities().supports(collection) {
        return Err(unsupported(connection.kind, collection));
    }
    let records = fetch_collection(&adapter, collection).await?;
    Ok(apply_records(ctx, connection, collection, records))
}

/// Sync every supported collection for a connection, collecting
/// per-collection counts and errors. Emits `panel.sync.completed`
/// when at least one collection succeeded and `panel.sync.failed`
/// when every collection failed. Updates the per-connection sync
/// state and, only when at least one collection succeeded, the
/// connection's `last_synced_at`.
pub(crate) async fn sync_all_collections(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
) -> PanelSyncResult {
    let adapter = match panel_registry().build(connection) {
        Ok(adapter) => Some(adapter),
        Err(error) => {
            ctx.hooks.emit(
                PANEL_SYNC_FAILED,
                serde_json::json!({
                    "actor": "sync",
                    "connection_id": connection.id,
                    "kind": connection.kind.as_str(),
                    "error": error.message,
                }),
            );
            let mut errors = HashMap::new();
            errors.insert("adapter".to_string(), error.message);
            return PanelSyncResult {
                connection_id: connection.id,
                kind: connection.kind.as_str().to_string(),
                synced_at: Utc::now(),
                counts: HashMap::new(),
                inserted: 0,
                updated: 0,
                unchanged: 0,
                conflicts: 0,
                local_wins: 0,
                errors,
                success: false,
            };
        }
    };
    let adapter = adapter.expect("adapter checked above");
    let capabilities = adapter.capabilities();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut errors: HashMap<String, String> = HashMap::new();
    let mut total = CollectionSyncOutcome::default();
    for collection in COLLECTIONS {
        if !capabilities.supports(collection) {
            continue;
        }
        match fetch_collection(&adapter, collection).await {
            Ok(records) => {
                counts.insert(collection.to_string(), records.len());
                let outcome = apply_records(ctx, connection, collection, records);
                total.inserted += outcome.inserted;
                total.updated += outcome.updated;
                total.unchanged += outcome.unchanged;
                total.conflicts += outcome.conflicts;
                total.local_wins += outcome.local_wins;
            }
            Err(error) => {
                counts.insert(collection.to_string(), 0);
                errors.insert(collection.to_string(), error.message);
            }
        }
    }
    let now = Utc::now();
    let success = errors.is_empty();
    let any_success = COLLECTIONS
        .iter()
        .any(|collection| capabilities.supports(collection) && !errors.contains_key(*collection));
    // Stamp `last_synced_at` only when at least one collection
    // actually synced: a run where every collection failed must
    // not move the timestamp forward.
    let previous_last_synced = sync_state_store()
        .get(&connection.id)
        .and_then(|state| state.last_synced_at);
    let stamped_at = next_last_synced_at(any_success, now, previous_last_synced);
    if any_success {
        if let Some(mut stored) = connections_store().get_mut(&connection.id) {
            stored.last_synced_at = Some(now);
            stored.updated_at = now;
        }
    }
    let total_records = records_store()
        .iter()
        .filter(|entry| entry.key().0 == connection.id)
        .count() as u64;
    sync_state_store().insert(
        connection.id,
        PanelSyncState {
            last_synced_at: stamped_at,
            last_status: Some(if success {
                "ok".to_string()
            } else if any_success {
                "partial".to_string()
            } else {
                "failed".to_string()
            }),
            last_error: errors.values().next().cloned(),
            counts: counts.clone(),
            conflicts: total.conflicts as u64,
            total_records,
        },
    );
    let result = PanelSyncResult {
        connection_id: connection.id,
        kind: connection.kind.as_str().to_string(),
        synced_at: now,
        counts,
        inserted: total.inserted,
        updated: total.updated,
        unchanged: total.unchanged,
        conflicts: total.conflicts,
        local_wins: total.local_wins,
        errors,
        success,
    };
    if any_success {
        ctx.hooks.emit(
            PANEL_SYNC_COMPLETED,
            serde_json::json!({
                "actor": "sync",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "inserted": result.inserted,
                "updated": result.updated,
                "unchanged": result.unchanged,
                "conflicts": result.conflicts,
                "errors": result.errors,
            }),
        );
    } else {
        ctx.hooks.emit(
            PANEL_SYNC_FAILED,
            serde_json::json!({
                "actor": "sync",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "errors": result.errors,
            }),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::apply_records;
    use super::collection_sync_success;
    use super::next_last_synced_at;
    use super::CollectionSyncOutcome;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::panels::adapter::payload_hash;
    use crate::modules::panels::store::records_store;
    use crate::modules::panels::store::SyncedRecord;
    use crate::modules::panels::types::PanelConnection;
    use crate::modules::panels::types::PanelCredentials;
    use crate::modules::panels::types::PanelKind;
    use crate::modules::panels::types::RemoteRecord;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    fn test_context() -> Arc<AppContext> {
        Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        ))
    }

    fn test_connection() -> PanelConnection {
        let now = Utc::now();
        PanelConnection {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            kind: PanelKind::Whmcs,
            endpoint: Some("https://93.184.216.34/".to_string()),
            credentials: PanelCredentials::default(),
            generic_config: None,
            allow_private_endpoint: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            last_synced_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn store_record(
        connection: &PanelConnection,
        prefer_local: bool,
        local_changed: bool,
    ) -> SyncedRecord {
        let payload = json!({"id": "1", "name": "Local Name"});
        let record = SyncedRecord {
            connection_id: connection.id,
            collection: "clients".to_string(),
            remote_id: "1".to_string(),
            name: "Local Name".to_string(),
            status: "active".to_string(),
            hash: payload_hash(&payload),
            last_synced_at: Utc::now(),
            direction: "local_to_remote".to_string(),
            conflict: false,
            local_changed,
            prefer_local,
            payload,
        };
        records_store().insert(
            (connection.id, "clients".to_string(), "1".to_string()),
            record.clone(),
        );
        record
    }

    fn remote_record() -> RemoteRecord {
        RemoteRecord {
            remote_id: "1".to_string(),
            name: "Remote Name".to_string(),
            status: "active".to_string(),
            payload: json!({"id": "1", "name": "Remote Name"}),
        }
    }

    #[test]
    fn local_wins_survives_the_next_sync() {
        let ctx = test_context();
        let connection = test_connection();
        store_record(&connection, true, true);
        let outcome = apply_records(&ctx, &connection, "clients", vec![remote_record()]);
        assert_eq!(outcome.local_wins, 1);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.conflicts, 0);
        let stored = records_store()
            .get(&(connection.id, "clients".to_string(), "1".to_string()))
            .unwrap()
            .clone();
        assert_eq!(stored.payload, json!({"id": "1", "name": "Local Name"}));
        assert!(!stored.local_changed);
        assert!(!stored.conflict);
    }

    #[test]
    fn prefer_local_wins_even_when_local_changed_was_cleared() {
        // The shape the old `resolve_conflict` local-wins produced
        // (`prefer_local` set, `local_changed` cleared): the next
        // sync must still honour the local win, not overwrite.
        let ctx = test_context();
        let connection = test_connection();
        store_record(&connection, true, false);
        let outcome = apply_records(&ctx, &connection, "clients", vec![remote_record()]);
        assert_eq!(outcome.local_wins, 1);
        assert_eq!(outcome.updated, 0);
        let stored = records_store()
            .get(&(connection.id, "clients".to_string(), "1".to_string()))
            .unwrap()
            .clone();
        assert_eq!(stored.payload, json!({"id": "1", "name": "Local Name"}));
    }

    #[test]
    fn local_change_without_preference_is_a_conflict_and_keeps_payload() {
        let ctx = test_context();
        let connection = test_connection();
        store_record(&connection, false, true);
        let outcome = apply_records(&ctx, &connection, "clients", vec![remote_record()]);
        assert_eq!(outcome.conflicts, 1);
        assert_eq!(outcome.updated, 0);
        let stored = records_store()
            .get(&(connection.id, "clients".to_string(), "1".to_string()))
            .unwrap()
            .clone();
        assert_eq!(stored.payload, json!({"id": "1", "name": "Local Name"}));
        assert!(stored.conflict);
    }

    #[test]
    fn collection_sync_success_is_false_with_conflicts() {
        assert!(collection_sync_success(&CollectionSyncOutcome::default()));
        let mut outcome = CollectionSyncOutcome::default();
        outcome.conflicts = 2;
        assert!(!collection_sync_success(&outcome));
    }

    #[test]
    fn next_last_synced_at_keeps_previous_when_nothing_succeeded() {
        let now = Utc::now();
        let previous = Some(now - chrono::Duration::hours(1));
        assert_eq!(next_last_synced_at(true, now, previous), Some(now));
        assert_eq!(next_last_synced_at(false, now, previous), previous);
        assert_eq!(next_last_synced_at(false, now, None), None);
    }
}
