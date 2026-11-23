// Diesel models for the storage_targets domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_targets)]
pub struct StorageTarget {
    pub name: String,
    pub kind: String,
    pub config: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_targets)]
pub struct NewStorageTarget {
    pub name: String,
    pub kind: String,
}

// ---------------------------------------------------------------------------
// Planner and analytics (2026-11-22-000015_planner_analytics).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = capacity_pools)]
pub struct CapacityPool {
    pub id: Uuid,
    pub name: String,
    pub provider_connection_id: Option<String>,
    pub cluster_id: Option<String>,
    pub capacity_cpu_cores: f64,
    pub capacity_ram_gb: f64,
    pub capacity_disk_gb: f64,
    pub capacity_bandwidth_tb: f64,
    pub notes: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_server_count: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = capacity_pools)]
pub struct NewCapacityPool {
    pub id: Uuid,
    pub name: String,
    pub provider_connection_id: Option<String>,
    pub cluster_id: Option<String>,
    pub notes: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_server_count: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pool_usage_samples)]
pub struct PoolUsageSample {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub used_cpu_cores: f64,
    pub used_ram_gb: f64,
    pub used_disk_gb: f64,
    pub used_bandwidth_tb: f64,
    pub source: String,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pool_usage_samples)]
pub struct NewPoolUsageSample {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub source: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = allocation_plans)]
pub struct AllocationPlan {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub project_name: Option<String>,
    pub reserved_cpu_cores: f64,
    pub reserved_ram_gb: f64,
    pub reserved_disk_gb: f64,
    pub reserved_bandwidth_tb: f64,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = allocation_plans)]
pub struct NewAllocationPlan {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub project_name: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
}
