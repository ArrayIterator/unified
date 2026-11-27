// Diesel models for the audit domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Audit (2026-11-22-000024_audit).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = audit_entries)]
pub struct AuditEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub metadata: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = audit_entries)]
pub struct NewAuditEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub ip: Option<String>,
}

// Gateway (2026-11-22-000025_gateway).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gateway_routing_rules)]
pub struct GatewayRoutingRule {
    pub id: Uuid,
    pub name: String,
    pub priority: i32,
    pub enabled: bool,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub path_regex: Option<String>,
    pub headers: Value,
    pub upstream: Value,
    pub strategy: String,
    pub require_tls: bool,
    pub rate_limit_override: Option<String>,
    pub health_check: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gateway_routing_rules)]
pub struct NewGatewayRoutingRule {
    pub id: Uuid,
    pub name: String,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub path_regex: Option<String>,
    pub strategy: String,
    pub rate_limit_override: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gateway_request_log)]
pub struct GatewayRequestLog {
    pub id: Uuid,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i64,
    pub upstream: Option<String>,
    pub backend_id: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gateway_request_log)]
pub struct NewGatewayRequestLog {
    pub id: Uuid,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i64,
    pub upstream: Option<String>,
    pub backend_id: Option<Uuid>,
}
