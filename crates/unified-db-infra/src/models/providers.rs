// Diesel models for the providers domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Providers (2026-11-22-000010_providers).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = provider_connections)]
pub struct ProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub project_id: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub custom_config: Option<Value>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = provider_connections)]
pub struct NewProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub project_id: Option<String>,
    pub custom_config: Option<Value>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = provider_servers)]
pub struct ProviderServer {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub remote_id: String,
    pub name: String,
    pub item_kind: String,
    pub status: String,
    pub raw_status: Option<String>,
    pub region: Option<String>,
    pub public_ip: Option<String>,
    pub private_ip: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_gb: Option<i64>,
    pub image: Option<String>,
    pub remote_created_at: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = provider_servers)]
pub struct NewProviderServer {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub remote_id: String,
    pub name: String,
    pub item_kind: String,
    pub status: String,
    pub raw_status: Option<String>,
    pub region: Option<String>,
    pub public_ip: Option<String>,
    pub private_ip: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_gb: Option<i64>,
    pub image: Option<String>,
    pub remote_created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Panels (2026-11-22-000013_panels).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = panel_connections)]
pub struct PanelConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub generic_config: Option<Value>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = panel_connections)]
pub struct NewPanelConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub generic_config: Option<Value>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = panel_synced_records)]
pub struct PanelSyncedRecord {
    pub connection_id: Uuid,
    pub collection: String,
    pub remote_id: String,
    pub name: String,
    pub status: String,
    pub hash: String,
    pub last_synced_at: DateTime<Utc>,
    pub direction: String,
    pub conflict: bool,
    pub local_changed: bool,
    pub prefer_local: bool,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = panel_synced_records)]
pub struct NewPanelSyncedRecord {
    pub connection_id: Uuid,
    pub collection: String,
    pub remote_id: String,
    pub name: String,
    pub status: String,
    pub hash: String,
    pub direction: String,
}
