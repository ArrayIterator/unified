// Diesel models for the cluster_sync domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Cluster sync reliability (migration 000082_cluster_sync_reliability).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_conflicts)]
pub struct ClusterSyncConflict {
    pub id: Uuid,
    pub log_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub key: String,
    pub local_updated_at: Option<DateTime<Utc>>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub winner: String,
    pub reason: String,
    pub local_payload: Option<Value>,
    pub remote_payload: Option<Value>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_conflicts)]
pub struct NewClusterSyncConflict {
    pub id: Uuid,
    pub log_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub key: String,
    pub local_updated_at: Option<DateTime<Utc>>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub winner: String,
    pub local_payload: Option<Value>,
    pub remote_payload: Option<Value>,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_retries)]
pub struct ClusterSyncRetry {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub direction: String,
    pub attempt: i32,
    pub max_attempts: i32,
    pub next_retry_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_retries)]
pub struct NewClusterSyncRetry {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub direction: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_idempotency_keys)]
pub struct ClusterSyncIdempotencyKey {
    pub scope: String,
    pub key: String,
    pub status: i32,
    pub body: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_idempotency_keys)]
pub struct NewClusterSyncIdempotencyKey {
    pub scope: String,
    pub key: String,
    pub status: i32,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_settings)]
pub struct ClusterSyncSetting {
    pub key: String,
    pub value: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_settings)]
pub struct NewClusterSyncSetting {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_domains)]
pub struct ClusterSyncDomain {
    pub domain: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_domains)]
pub struct NewClusterSyncDomain {
    pub domain: String,
}
