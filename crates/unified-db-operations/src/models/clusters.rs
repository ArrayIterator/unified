// Diesel models for the clusters domain of the unified-db-operations bounded context.
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
// Clusters (2026-11-22-000008_clusters).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = clusters)]
pub struct Cluster {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = clusters)]
pub struct NewCluster {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_nodes)]
pub struct ClusterNode {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub display_name: String,
    pub base_url: String,
    pub role: String,
    pub status: String,
    #[serde(skip_serializing)]
    pub secret_hash: String,
    pub secret_fingerprint: String,
    pub health: String,
    pub consecutive_failures: i32,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_nodes)]
pub struct NewClusterNode {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub display_name: String,
    pub base_url: String,
    pub role: String,
    pub status: String,
    pub secret_hash: String,
    pub secret_fingerprint: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_plans)]
pub struct ClusterSyncPlan {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub target_node_id: Uuid,
    pub local_node_id: String,
    pub direction: String,
    pub domains: Value,
    pub schedule: String,
    pub enabled: bool,
    #[serde(skip_serializing)]
    pub peer_secret_hash: String,
    pub peer_secret_fingerprint: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_plans)]
pub struct NewClusterSyncPlan {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub target_node_id: Uuid,
    pub local_node_id: String,
    pub direction: String,
    pub peer_secret_hash: String,
    pub peer_secret_fingerprint: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_items)]
pub struct ClusterSyncItem {
    pub domain: String,
    pub key: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_items)]
pub struct NewClusterSyncItem {
    pub domain: String,
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_logs)]
pub struct ClusterSyncLog {
    pub id: Uuid,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub direction: String,
    pub domain: String,
    pub status: String,
    pub items_sent: i64,
    pub items_received: i64,
    pub items_applied: i64,
    pub items_skipped: i64,
    pub conflicts: Value,
    pub errors: Value,
    pub dry_run: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_logs)]
pub struct NewClusterSyncLog {
    pub id: Uuid,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub direction: String,
    pub domain: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_health_checks)]
pub struct ClusterHealthCheck {
    pub id: Uuid,
    pub node_id: Uuid,
    pub cluster_id: Uuid,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_health_checks)]
pub struct NewClusterHealthCheck {
    pub id: Uuid,
    pub node_id: Uuid,
    pub cluster_id: Uuid,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
}
