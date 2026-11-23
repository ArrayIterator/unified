//! panels module - In-memory connection, record and sync-state stores.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use uuid::Uuid;

use super::types::PanelConnection;

// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the provider connection store).
// ---------------------------------------------------------------------------

pub(crate) static CONNECTIONS: OnceLock<DashMap<Uuid, PanelConnection>> = OnceLock::new();

pub(crate) fn connections_store() -> &'static DashMap<Uuid, PanelConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}

/// Direction of the last sync decision for one stored record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// First import, or the remote record changed and was applied.
    RemoteToLocal,
    /// Nothing changed on either side since the last sync.
    Unchanged,
    /// Both sides changed; the record is in conflict.
    Conflict,
    /// The local version was explicitly preferred over the remote one.
    LocalToRemote,
}

impl SyncDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RemoteToLocal => "remote_to_local",
            Self::Unchanged => "unchanged",
            Self::Conflict => "conflict",
            Self::LocalToRemote => "local_to_remote",
        }
    }
}

/// One imported record, with its two-way sync state.
#[derive(Debug, Clone, Serialize)]
pub struct SyncedRecord {
    pub connection_id: Uuid,
    pub collection: String,
    pub remote_id: String,
    pub name: String,
    pub status: String,
    /// SHA-256 hex of the canonical remote payload at last sync.
    pub hash: String,
    pub last_synced_at: DateTime<Utc>,
    pub direction: String,
    pub conflict: bool,
    /// True when a local change was flagged through the API and has
    /// not been resolved yet.
    pub local_changed: bool,
    /// True when the operator explicitly chose the local version for
    /// this record.
    pub prefer_local: bool,
    /// The last payload applied locally (remote payload, unless the
    /// local version won a conflict).
    pub payload: Value,
}

/// Key: (connection id, collection, remote id).
pub(crate) type RecordKey = (Uuid, String, String);

pub(crate) static RECORDS: OnceLock<DashMap<RecordKey, SyncedRecord>> = OnceLock::new();

pub(crate) fn records_store() -> &'static DashMap<RecordKey, SyncedRecord> {
    RECORDS.get_or_init(DashMap::new)
}

/// Last sync run state per connection.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PanelSyncState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub counts: HashMap<String, usize>,
    #[serde(default)]
    pub conflicts: u64,
    #[serde(default)]
    pub total_records: u64,
}

pub(crate) static SYNC_STATE: OnceLock<DashMap<Uuid, PanelSyncState>> = OnceLock::new();

pub(crate) fn sync_state_store() -> &'static DashMap<Uuid, PanelSyncState> {
    SYNC_STATE.get_or_init(DashMap::new)
}
