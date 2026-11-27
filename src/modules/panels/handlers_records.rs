//! panels module - HTTP handlers for synced records and sync actions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::adapter::payload_hash;
use super::constants::COLLECTIONS;
use super::constants::PANEL_SYNC_COMPLETED;
use super::constants::PANEL_SYNC_CONFLICT;
use super::handlers::MarkLocalChangedRequest;
use super::handlers::ResolveConflictRequest;
use super::handlers::authorize_manage;
use super::handlers::authorize_view;
use super::handlers::find_connection;
use super::handlers::list_collection_records;
use super::handlers::require_enabled;
use super::handlers::require_tenant;
use super::handlers::stored_records;
use super::registry::panel_registry;
use super::store::RecordKey;
use super::store::SyncDirection;
use super::store::SyncedRecord;
use super::store::connections_store;
use super::store::records_store;
use super::store::sync_state_store;
use super::sync::PanelSyncResult;
use super::sync::collection_sync_success;
use super::sync::fetch_collection;
use super::sync::sync_all_collections;
use super::sync::sync_one_collection;
use super::types::collection_from_segment;

macro_rules! list_records_handler {
    ($name:ident, $collection:literal) => {
        pub(crate) async fn $name(
            ctx: web::Data<Arc<AppContext>>,
            req: HttpRequest,
            path: web::Path<Uuid>,
            params: web::Query<PaginationParams>,
        ) -> HttpResponse {
            let debug = ctx.is_debug();
            if let Err(error) = require_tenant(&req) {
                return json::error_response(&error, debug);
            }
            if let Err(error) = authorize_view(&ctx, &req) {
                return json::error_response(&error, debug);
            }
            json::respond(
                list_collection_records(path.into_inner(), $collection, &params),
                debug,
            )
        }
    };
}

list_records_handler!(list_clients_records, "clients");
list_records_handler!(list_products_records, "products");
list_records_handler!(list_services_records, "services");
list_records_handler!(list_domains_records, "domains");
list_records_handler!(list_invoices_records, "invoices");
list_records_handler!(list_server_accounts_records, "server_accounts");

pub(crate) async fn get_record(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncedRecord> = (|| {
        let (connection_id, collection_segment, remote_id) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown collection \"{collection_segment}\""))
        })?;
        let connection = find_connection(connection_id)?;
        let key: RecordKey = (connection_id, collection.to_string(), remote_id.clone());
        let _ = connection;
        records_store()
            .get(&key)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
                ))
            })
    })();
    json::respond(result, debug)
}

pub(crate) async fn sync_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelSyncResult> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _ = authorize_manage(&ctx, &req)?;
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        Ok(sync_all_collections(&ctx, &connection).await)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn sync_collection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelSyncResult> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _ = authorize_manage(&ctx, &req)?;
        let (connection_id, collection_segment) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown collection \"{collection_segment}\""))
        })?;
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let outcome = sync_one_collection(&ctx, &connection, collection).await?;
        let mut counts = HashMap::new();
        counts.insert(
            collection.to_string(),
            outcome.inserted
                + outcome.updated
                + outcome.unchanged
                + outcome.conflicts
                + outcome.local_wins,
        );
        let now = Utc::now();
        if let Some(mut stored) = connections_store().get_mut(&connection.id) {
            stored.last_synced_at = Some(now);
            stored.updated_at = now;
        }
        ctx.hooks.emit(
            PANEL_SYNC_COMPLETED,
            serde_json::json!({
                "actor": "sync",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "collection": collection,
                "inserted": outcome.inserted,
                "updated": outcome.updated,
                "unchanged": outcome.unchanged,
                "conflicts": outcome.conflicts,
            }),
        );
        Ok(PanelSyncResult {
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            synced_at: now,
            counts,
            inserted: outcome.inserted,
            updated: outcome.updated,
            unchanged: outcome.unchanged,
            conflicts: outcome.conflicts,
            local_wins: outcome.local_wins,
            errors: HashMap::new(),
            // A run that ended with conflicts is not a success,
            // even though the fetch itself ran.
            success: collection_sync_success(&outcome),
        })
    })()
    .await;
    json::respond(result, debug)
}

/// Stored sync status: served from the last sync state and the
/// stored records only - no outbound call. A GET must not perform
/// network fetches against the panel (that is both a read doing
/// writes' work and the SSRF surface). Remote-side counts are
/// unknown here (`null`, `"live": false`); use the explicit POST
/// `.../sync-status/refresh` for a live comparison.
pub(crate) async fn get_sync_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let adapter = panel_registry().build(&connection)?;
        let capabilities = adapter.capabilities();
        let mut collections = serde_json::Map::new();
        let mut total_conflicts: u64 = 0;
        let mut total_local_changed: u64 = 0;
        let mut total_stored: u64 = 0;
        for collection in COLLECTIONS {
            if !capabilities.supports(collection) {
                continue;
            }
            let stored = stored_records(connection.id, collection);
            let local_changed = stored.iter().filter(|record| record.local_changed).count() as u64;
            let conflicts = stored.iter().filter(|record| record.conflict).count() as u64;
            total_conflicts += conflicts;
            total_local_changed += local_changed;
            total_stored += stored.len() as u64;
            let mut entry = serde_json::Map::new();
            entry.insert("stored".to_string(), Value::from(stored.len() as u64));
            entry.insert("local_changed".to_string(), Value::from(local_changed));
            entry.insert("conflicts".to_string(), Value::from(conflicts));
            entry.insert("live".to_string(), Value::Bool(false));
            entry.insert("remote".to_string(), Value::Null);
            entry.insert("remote_changed".to_string(), Value::Null);
            entry.insert("unchanged".to_string(), Value::Null);
            entry.insert("only_remote".to_string(), Value::Null);
            entry.insert("only_local".to_string(), Value::Null);
            collections.insert(collection.to_string(), Value::Object(entry));
        }
        let state = sync_state_store().get(&connection.id);
        Ok(serde_json::json!({
            "connection_id": connection.id,
            "kind": connection.kind.as_str(),
            "last_synced_at": state
                .as_ref()
                .and_then(|state| state.last_synced_at)
                .map(|time| time.to_rfc3339()),
            "last_status": state
                .as_ref()
                .and_then(|state| state.last_status.clone()),
            "live": false,
            "detail": "Stored sync state only (no live panel fetch). POST /api/v1/panels/connections/{id}/sync-status/refresh for a live comparison.",
            "collections": collections,
            "totals": {
                "stored": total_stored,
                "conflicts": total_conflicts,
                "local_changed": total_local_changed,
                "remote_changed": Value::Null,
                "only_remote": Value::Null,
                "only_local": Value::Null,
            },
        }))
    })();
    json::respond(result, debug)
}

/// Live sync status: fetches every supported collection from the
/// panel and compares it with the stored records, reporting per
/// collection what changed locally vs remotely and how many
/// conflicts exist. Explicit POST only - a live fetch is an
/// outbound call carrying panel credentials, so it needs the
/// manage permission and must never run on a GET. A collection
/// whose live fetch fails is reported with its error instead of
/// failing the whole endpoint.
///
/// The fetch is complete by adapter contract (adapters walk every
/// page or fail instead of truncating), so a stored record absent
/// from the fetched set really is absent remotely - `only_local`
/// here is a genuine count, not a page-boundary artefact.
pub(crate) async fn refresh_sync_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _ = authorize_manage(&ctx, &req)?;
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let adapter = panel_registry().build(&connection)?;
        let capabilities = adapter.capabilities();
        let mut collections = serde_json::Map::new();
        let mut total_conflicts: u64 = 0;
        let mut total_remote_changed: u64 = 0;
        let mut total_local_changed: u64 = 0;
        let mut total_only_remote: u64 = 0;
        let mut total_only_local: u64 = 0;
        for collection in COLLECTIONS {
            if !capabilities.supports(collection) {
                continue;
            }
            let stored = stored_records(connection.id, collection);
            let mut entry = serde_json::Map::new();
            entry.insert("stored".to_string(), Value::from(stored.len() as u64));
            entry.insert("live".to_string(), Value::Bool(true));
            match fetch_collection(&adapter, collection).await {
                Ok(remote) => {
                    let stored_by_id: HashMap<&str, &SyncedRecord> = stored
                        .iter()
                        .map(|record| (record.remote_id.as_str(), record))
                        .collect();
                    let mut unchanged: u64 = 0;
                    let mut remote_changed: u64 = 0;
                    let mut local_changed: u64 = 0;
                    let mut conflicts: u64 = 0;
                    let mut only_remote: Vec<String> = Vec::new();
                    for record in &remote {
                        match stored_by_id.get(record.remote_id.as_str()) {
                            None => {
                                only_remote.push(record.remote_id.clone());
                            }
                            Some(stored_record) => {
                                let hash = payload_hash(&record.payload);
                                if stored_record.local_changed && hash != stored_record.hash {
                                    conflicts += 1;
                                    local_changed += 1;
                                } else if stored_record.local_changed {
                                    local_changed += 1;
                                } else if hash != stored_record.hash {
                                    remote_changed += 1;
                                } else {
                                    unchanged += 1;
                                }
                            }
                        }
                    }
                    let remote_ids: std::collections::HashSet<&str> = remote
                        .iter()
                        .map(|record| record.remote_id.as_str())
                        .collect();
                    let only_local: Vec<String> = stored
                        .iter()
                        .filter(|record| !remote_ids.contains(record.remote_id.as_str()))
                        .map(|record| record.remote_id.clone())
                        .collect();
                    total_conflicts += conflicts;
                    total_remote_changed += remote_changed;
                    total_local_changed += local_changed;
                    total_only_remote += only_remote.len() as u64;
                    total_only_local += only_local.len() as u64;
                    entry.insert("remote".to_string(), Value::from(remote.len() as u64));
                    entry.insert("unchanged".to_string(), Value::from(unchanged));
                    entry.insert("remote_changed".to_string(), Value::from(remote_changed));
                    entry.insert("local_changed".to_string(), Value::from(local_changed));
                    entry.insert("conflicts".to_string(), Value::from(conflicts));
                    entry.insert(
                        "only_remote".to_string(),
                        Value::from(only_remote.len() as u64),
                    );
                    entry.insert(
                        "only_local".to_string(),
                        Value::from(only_local.len() as u64),
                    );
                    entry.insert(
                        "only_remote_ids".to_string(),
                        Value::Array(
                            only_remote
                                .into_iter()
                                .take(50)
                                .map(Value::String)
                                .collect(),
                        ),
                    );
                    entry.insert(
                        "only_local_ids".to_string(),
                        Value::Array(only_local.into_iter().take(50).map(Value::String).collect()),
                    );
                }
                Err(error) => {
                    entry.insert("error".to_string(), Value::String(error.message));
                }
            }
            collections.insert(collection.to_string(), Value::Object(entry));
        }
        let state = sync_state_store().get(&connection.id);
        Ok(serde_json::json!({
            "connection_id": connection.id,
            "kind": connection.kind.as_str(),
            "last_synced_at": state
                .as_ref()
                .and_then(|state| state.last_synced_at)
                .map(|time| time.to_rfc3339()),
            "last_status": state
                .as_ref()
                .and_then(|state| state.last_status.clone()),
            "live": true,
            "collections": collections,
            "totals": {
                "conflicts": total_conflicts,
                "remote_changed": total_remote_changed,
                "local_changed": total_local_changed,
                "only_remote": total_only_remote,
                "only_local": total_only_local,
            },
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn mark_local_changed(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String, String)>,
    body: web::Json<MarkLocalChangedRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SyncedRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _ = authorize_manage(&ctx, &req)?;
        let (connection_id, collection_segment, remote_id) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown collection \"{collection_segment}\""))
        })?;
        let _ = find_connection(connection_id)?;
        let key: RecordKey = (connection_id, collection.to_string(), remote_id.clone());
        let mut stored = records_store().get_mut(&key).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
            ))
        })?;
        stored.local_changed = true;
        stored.prefer_local = body.prefer_local;
        stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
        Ok(stored.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn resolve_conflict(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String, String)>,
    body: web::Json<ResolveConflictRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let caller = authorize_manage(&ctx, &req)?;
        let (connection_id, collection_segment, remote_id) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown collection \"{collection_segment}\""
            ))
        })?;
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let resolution = body.resolution.trim();
        if !matches!(resolution, "remote_wins" | "local_wins") {
            return Err(UnifiedError::bad_request(
                "resolution must be \"remote_wins\" or \"local_wins\"",
            ));
        }
        let key: RecordKey = (
            connection_id,
            collection.to_string(),
            remote_id.clone(),
        );
        match resolution {
            "local_wins" => {
                let mut stored = records_store().get_mut(&key).ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
                    ))
                })?;
                stored.prefer_local = true;
                // Keep `local_changed` set: the next sync's win path
                // requires it, and clearing it here let the next
                // `apply_records` fall through to the overwrite
                // branch and undo the resolution. `apply_records`
                // clears both flags once the win is applied.
                stored.local_changed = true;
                stored.conflict = false;
                stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
                stored.last_synced_at = Utc::now();
                let record = stored.clone();
                drop(stored);
                ctx.hooks.emit(
                    PANEL_SYNC_CONFLICT,
                    serde_json::json!({
                        "actor": caller.actor,
                        "connection_id": connection.id,
                        "kind": connection.kind.as_str(),
                        "resource": format!("panel-{collection}:{remote_id}"),
                        "collection": collection,
                        "remote_id": remote_id,
                        "resolution": "local_wins",
                    }),
                );
                Ok(serde_json::json!({ "record": record, "resolution": "local_wins" }))
            }
            _ => {
                // Remote wins: re-fetch the record live and apply it,
                // clearing the local flags. When the remote side no
                // longer has the record, the stored record is removed
                // and the deletion is reported. The fetch is complete
                // by adapter contract (every page walked, or the
                // fetch fails instead of truncating), so absence
                // here means the record is really gone remotely -
                // never a page-boundary artefact.
                let adapter = panel_registry().build(&connection)?;
                let remote = fetch_collection(&adapter, collection).await?;
                let found = remote
                    .iter()
                    .find(|record| record.remote_id == remote_id)
                    .cloned();
                match found {
                    Some(record) => {
                        let hash = payload_hash(&record.payload);
                        let mut stored = records_store().get_mut(&key).ok_or_else(|| {
                            UnifiedError::not_found(format!(
                                "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
                            ))
                        })?;
                        stored.hash = hash;
                        stored.name = record.name.clone();
                        stored.status = record.status.clone();
                        stored.payload = record.payload.clone();
                        stored.local_changed = false;
                        stored.prefer_local = false;
                        stored.conflict = false;
                        stored.direction =
                            SyncDirection::RemoteToLocal.as_str().to_string();
                        stored.last_synced_at = Utc::now();
                        let view = stored.clone();
                        drop(stored);
                        ctx.hooks.emit(
                            PANEL_SYNC_CONFLICT,
                            serde_json::json!({
                                "actor": caller.actor,
                                "connection_id": connection.id,
                                "kind": connection.kind.as_str(),
                                "resource": format!("panel-{collection}:{remote_id}"),
                                "collection": collection,
                                "remote_id": remote_id,
                                "resolution": "remote_wins",
                            }),
                        );
                        Ok(serde_json::json!({ "record": view, "resolution": "remote_wins" }))
                    }
                    None => {
                        records_store().remove(&key);
                        Ok(serde_json::json!({
                            "resolution": "remote_wins",
                            "remote_id": remote_id,
                            "deleted_locally": true,
                            "detail": "Remote record no longer exists; the stored record was removed",
                        }))
                    }
                }
            }
        }
    })()
    .await;
    json::respond(result, debug)
}
