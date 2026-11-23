// Diesel models for the load_balancer domain of the unified-db-operations bounded context.
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
// Load balancer (2026-11-22-000017_load_balancer).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = load_balancers)]
pub struct LoadBalancer {
    pub id: Uuid,
    pub name: String,
    pub listen_address: String,
    pub listen_port: i32,
    pub tls_mode: String,
    pub domain: Option<String>,
    pub certificate_name: Option<String>,
    pub strategy: String,
    pub health_check: Value,
    pub enabled: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = load_balancers)]
pub struct NewLoadBalancer {
    pub id: Uuid,
    pub name: String,
    pub listen_address: String,
    pub listen_port: i32,
    pub domain: Option<String>,
    pub certificate_name: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lb_backends)]
pub struct LbBackend {
    pub id: Uuid,
    pub balancer_id: Uuid,
    pub name: String,
    pub source_kind: String,
    pub source_id: Option<String>,
    pub address: String,
    pub weight: i32,
    pub enabled: bool,
    pub draining: bool,
    pub health: String,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lb_backends)]
pub struct NewLbBackend {
    pub id: Uuid,
    pub balancer_id: Uuid,
    pub name: String,
    pub source_id: Option<String>,
    pub address: String,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lb_backend_stats)]
pub struct LbBackendStat {
    pub backend_id: Uuid,
    pub requests_routed: i64,
    pub errors: i64,
    pub total_latency_ms: i64,
    pub latency_samples: i64,
    pub checks_total: i64,
    pub checks_ok: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lb_backend_stats)]
pub struct NewLbBackendStat {
    pub backend_id: Uuid,
}
