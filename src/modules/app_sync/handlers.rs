//! app_sync - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{ Arc, RwLock };
use uuid::Uuid;
use super::{constants::*, helpers::*, types::*, tokens::*, inputs::*, module::*};



// Handlers.
pub(crate) async fn register_device(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RegisterDeviceRequest>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    // Device registration mints an HMAC-signed sync token, so it is an
    // authenticated admin operation, not an open onboarding step: the
    // caller must hold `app-sync:manage`, and the device is bound to
    // that caller's identity.
    if let Err(error) = require_permission(&ctx, &req, PERM_APP_SYNC_MANAGE) {
        return json::error_response(&error, debug);
    }
    let owner_id = match caller(&req) {
        Ok(who) => who.actor,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        if body.device_name.trim().is_empty() {
            return Err(UnifiedError::bad_request("device_name is required"));
        }
        let platform = body.platform.trim();
        if platform.is_empty() {
            return Err(UnifiedError::bad_request("platform must not be empty"));
        }
        let device = Arc::new(DeviceState {
            id: Uuid::now_v7(),
            device_name: body.device_name.trim().to_string(),
            platform: platform.to_string(),
            owner_id,
            created_at: Utc::now(),
            last_sync_at: RwLock::new(None),
            revoked: AtomicBool::new(false),
            synced_once: AtomicBool::new(false),
            version: AtomicU64::new(0),
        });
        let token = sign_device_token(&signing_secret(&ctx)?, device.id);
        let view = device.snapshot();
        devices_store().insert(device.id, Arc::clone(&device));
        sync_store().insert(device.id, Vec::new());
        Ok(serde_json::json!({
            "device": view,
            "device_token": token,
        }))
    })();
    match result {
        Ok(value) => json::success_response(value, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn list_devices(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_APP_SYNC_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<DeviceView> = devices_store()
            .iter()
            .map(|entry| entry.value().snapshot())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|device| device.device_name.to_lowercase().contains(&needle));
        }
        if let Some(platform) = params.filters.get("platform") {
            items.retain(|device| device.platform == *platform);
        }
        if let Some(revoked) = params.filters.get("revoked") {
            let wanted = revoked == "true" || revoked == "1";
            items.retain(|device| device.revoked == wanted);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_device(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_APP_SYNC_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<DeviceView> = (|| {
        devices_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().snapshot())
            .ok_or_else(|| UnifiedError::not_found("Device not found"))
    })();
    json::respond(result, debug)
}


pub(crate) async fn revoke_device(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_APP_SYNC_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<DeviceView> = (|| {
        let device = devices_store()
            .get(&id.into_inner())
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Device not found"))?;
        device.revoked.store(true, Ordering::SeqCst);
        Ok(device.snapshot())
    })();
    json::respond(result, debug)
}


pub(crate) fn mark_synced(ctx: &AppContext, device: &DeviceState) {
    *device.last_sync_at.write().expect("device lock poisoned") = Some(Utc::now());
    if !device.synced_once.swap(true, Ordering::SeqCst) {
        ctx.hooks.emit(
            events::SYNC_CONNECTED,
            serde_json::json!({
                "device_id": device.id,
                "device_name": device.device_name,
                "platform": device.platform,
                "first_sync": true,
            }),
        );
    }
}


pub(crate) async fn sync_push(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SyncPushRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(SyncRecord, bool)> = (|| {
        if body.payload.is_null() {
            return Err(UnifiedError::bad_request("payload is required"));
        }
        let idempotency_key = match &body.idempotency_key {
            Some(key) => {
                let key = key.trim();
                if key.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "idempotency_key must not be empty",
                    ));
                }
                if key.len() > 128 {
                    return Err(UnifiedError::bad_request(
                        "idempotency_key is too long (maximum 128 characters)",
                    ));
                }
                if key.contains(char::is_whitespace) {
                    return Err(UnifiedError::bad_request(
                        "idempotency_key must not contain whitespace",
                    ));
                }
                Some(key.to_string())
            }
            None => None,
        };
        let device = validate_device_token(&ctx, &body.device_token)?;
        let (record, replayed) =
            push_record(&device, &body.payload, idempotency_key.as_deref());
        if !replayed {
            mark_synced(&ctx, &device);
        }
        Ok((record, replayed))
    })();
    match result {
        // A replayed push answers 200 with the original record; a fresh
        // push answers 201.
        Ok((record, replayed)) => json::success_response(record, if replayed { 200 } else { 201 }),
        Err(error) => json::error_response(&error, debug),
    }
}


/// Append one versioned record for a device, retaining only the
/// newest records.
fn append_sync_record(device: &DeviceState, payload: &Value) -> SyncRecord {
    let version = device.version.fetch_add(1, Ordering::SeqCst) + 1;
    let record = SyncRecord {
        id: Uuid::now_v7(),
        device_id: device.id,
        version,
        updated_at: Utc::now(),
        payload: payload.clone(),
    };
    sync_store()
        .entry(device.id)
        .or_insert_with(Vec::new)
        .push(record.clone());
    // Retain only the newest records.
    if let Some(mut records) = sync_store().get_mut(&device.id) {
        if records.len() > MAX_RECORDS_PER_DEVICE {
            let excess = records.len() - MAX_RECORDS_PER_DEVICE;
            records.drain(..excess);
        }
    }
    record
}


/// Push one record, honouring the idempotency key. Returns the record
/// and whether it is a replay of an earlier push.
///
/// The check-then-insert is atomic: the idempotency-store entry for
/// `(device id, key)` is held across the check, the append and the
/// insert, so two concurrent pushes with the same key cannot both miss
/// the store, both bump the device version and both append records.
/// A push without a key always appends a fresh record.
pub(crate) fn push_record(
    device: &DeviceState,
    payload: &Value,
    idempotency_key: Option<&str>,
) -> (SyncRecord, bool) {
    match idempotency_key {
        Some(key) => match push_idempotency_store().entry((device.id, key.to_string())) {
            Entry::Occupied(entry) => (entry.get().clone(), true),
            Entry::Vacant(slot) => {
                let record = append_sync_record(device, payload);
                slot.insert(record.clone());
                (record, false)
            }
        },
        None => (append_sync_record(device, payload), false),
    }
}


/// Cursor filter for a pull: the cursor is either a version or a
/// timestamp, never both. A timestamp cursor filters by `updated_at`
/// only; otherwise the version cursor filters by `version` only.
/// Filtering by `version > since_version || updated_at > time` made
/// every timestamp pull return the full history, because every stored
/// record has `version >= 1 > 0`.
pub(crate) fn record_matches_cursor(
    record: &SyncRecord,
    since_version: u64,
    since_time: Option<DateTime<Utc>>,
) -> bool {
    match since_time {
        Some(time) => record.updated_at > time,
        None => record.version > since_version,
    }
}


pub(crate) async fn sync_pull(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SyncPullRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let device = validate_device_token(&ctx, &body.device_token)?;
        let (since_version, since_time) = parse_cursor(&body.since);
        let records: Vec<SyncRecord> = sync_store()
            .get(&device.id)
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|record| record_matches_cursor(record, since_version, since_time))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let current_version = device.version.load(Ordering::SeqCst);
        mark_synced(&ctx, &device);
        Ok(serde_json::json!({
            "device_id": device.id,
            "current_version": current_version,
            "records": records,
            "count": records.len(),
        }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::thread;

    fn test_device() -> Arc<DeviceState> {
        Arc::new(DeviceState {
            id: Uuid::now_v7(),
            device_name: "Test laptop".to_string(),
            platform: "linux".to_string(),
            owner_id: "user-1".to_string(),
            created_at: Utc::now(),
            last_sync_at: RwLock::new(None),
            revoked: AtomicBool::new(false),
            synced_once: AtomicBool::new(false),
            version: AtomicU64::new(0),
        })
    }

    fn test_record(version: u64, updated_at: DateTime<Utc>) -> SyncRecord {
        SyncRecord {
            id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            version,
            updated_at,
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn cursor_version_filters_by_version_only() {
        let now = Utc::now();
        assert!(record_matches_cursor(&test_record(6, now), 5, None));
        assert!(!record_matches_cursor(&test_record(5, now), 5, None));
        assert!(!record_matches_cursor(&test_record(1, now), 5, None));
        // A full pull, cursor (0, None), matches everything.
        assert!(record_matches_cursor(&test_record(1, now), 0, None));
    }

    #[test]
    fn cursor_timestamp_filters_by_updated_at_only() {
        // The regression: with a timestamp cursor, a record updated
        // before the cursor must not be returned, even though its
        // version (1) is greater than the version floor (0) that a
        // timestamp cursor carries.
        let before = Utc::now() - Duration::hours(2);
        let cursor = Utc::now() - Duration::hours(1);
        let after = Utc::now();
        assert!(!record_matches_cursor(&test_record(1, before), 0, Some(cursor)));
        assert!(record_matches_cursor(&test_record(1, after), 0, Some(cursor)));
        // Boundary: updated exactly at the cursor is not newer.
        assert!(!record_matches_cursor(&test_record(9, cursor), 0, Some(cursor)));
    }

    #[test]
    fn concurrent_pushes_with_the_same_idempotency_key_append_once() {
        let device = test_device();
        let payload = serde_json::json!({"field": "value"});
        let mut handles = Vec::new();
        for _ in 0..8 {
            let device = Arc::clone(&device);
            let payload = payload.clone();
            handles.push(thread::spawn(move || {
                push_record(&device, &payload, Some("key-1"))
            }));
        }
        let mut ids = std::collections::HashSet::new();
        let mut fresh = 0;
        for handle in handles {
            let (record, replayed) = handle.join().expect("push thread");
            ids.insert(record.id);
            if !replayed {
                fresh += 1;
            }
        }
        assert_eq!(
            ids.len(),
            1,
            "all concurrent pushes with the same key must return the same record"
        );
        assert_eq!(fresh, 1, "exactly one push must be fresh");
        assert_eq!(device.version.load(Ordering::SeqCst), 1);
        assert_eq!(
            sync_store().get(&device.id).map(|entry| entry.value().len()),
            Some(1)
        );
    }

    #[test]
    fn pushes_without_a_key_each_append_a_record() {
        let device = test_device();
        let payload = serde_json::json!({});
        let (first, replayed_first) = push_record(&device, &payload, None);
        let (second, replayed_second) = push_record(&device, &payload, None);
        assert!(!replayed_first && !replayed_second);
        assert_ne!(first.id, second.id);
        assert_eq!(second.version, first.version + 1);
    }
}
