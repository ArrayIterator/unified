//! app_sync - types.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;



#[derive(Debug, Clone, Serialize)]
// ---------------------------------------------------------------------------
// Device and sync-record state.
// ---------------------------------------------------------------------------
pub struct DeviceView {
    pub id: Uuid,
    pub device_name: String,
    pub platform: String,
    /// Identity of the authenticated caller that registered the device
    /// (`X-User-Id`); the device token is bound to this owner.
    pub owner_id: String,
    pub created_at: DateTime<Utc>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub version: u64,
}


pub struct DeviceState {
    pub id: Uuid,
    pub device_name: String,
    pub platform: String,
    /// Identity of the authenticated caller that registered the device.
    pub owner_id: String,
    pub created_at: DateTime<Utc>,
    pub last_sync_at: RwLock<Option<DateTime<Utc>>>,
    pub revoked: AtomicBool,
    pub synced_once: AtomicBool,
    pub version: AtomicU64,
}


impl DeviceState {
    pub(crate) fn snapshot(&self) -> DeviceView {
        DeviceView {
            id: self.id,
            device_name: self.device_name.clone(),
            platform: self.platform.clone(),
            owner_id: self.owner_id.clone(),
            created_at: self.created_at,
            last_sync_at: *self.last_sync_at.read().expect("device lock poisoned"),
            revoked: self.revoked.load(Ordering::SeqCst),
            version: self.version.load(Ordering::SeqCst),
        }
    }
}


#[derive(Debug, Clone, Serialize)]
pub struct SyncRecord {
    pub id: Uuid,
    pub device_id: Uuid,
    pub version: u64,
    pub updated_at: DateTime<Utc>,
    pub payload: Value,
}


pub(crate) static DEVICES: OnceLock<DashMap<Uuid, Arc<DeviceState>>> = OnceLock::new();

pub(crate) static SYNC_RECORDS: OnceLock<DashMap<Uuid, Vec<SyncRecord>>> = OnceLock::new();

/// Idempotent push results: (device id, client idempotency key) ->
/// the record created by the first push. A replayed push returns the
/// stored record instead of appending a duplicate.
pub(crate) static PUSH_IDEMPOTENCY: OnceLock<DashMap<(Uuid, String), SyncRecord>> = OnceLock::new();


pub(crate) fn devices_store() -> &'static DashMap<Uuid, Arc<DeviceState>> {
    DEVICES.get_or_init(DashMap::new)
}


pub(crate) fn push_idempotency_store() -> &'static DashMap<(Uuid, String), SyncRecord> {
    PUSH_IDEMPOTENCY.get_or_init(DashMap::new)
}


pub(crate) fn sync_store() -> &'static DashMap<Uuid, Vec<SyncRecord>> {
    SYNC_RECORDS.get_or_init(DashMap::new)
}


/// Maximum sync records retained per device (oldest are dropped).
pub(crate) const MAX_RECORDS_PER_DEVICE: usize = 1000;
