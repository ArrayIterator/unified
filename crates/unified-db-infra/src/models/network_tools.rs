// Diesel models for the network_tools domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Network Tools + Licensing SaaS (batch 7 notes sec. 3). The notes flag
// the convention warning themselves: the network-tools structs
// combine Queryable + Selectable + Insertable on one struct, so they
// are split here into the repo's usual Queryable / New* form
// (table_name form and derives normalised to the file convention;
// the notes' `check_for_backend` is not used in this file).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_saved_checks)]
pub struct NetworkToolSavedCheck {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub tool: String,
    pub target: String,
    pub parameters: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_saved_checks)]
pub struct NewNetworkToolSavedCheck {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub tool: String,
    pub target: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_runs)]
pub struct NetworkToolRun {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub tool: String,
    pub target: String,
    pub duration_ms: i64,
    pub outcome: String,
    pub summary: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_runs)]
pub struct NewNetworkToolRun {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub tool: String,
    pub target: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_uptime_monitors)]
pub struct NetworkToolUptimeMonitor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub interval_seconds: i32,
    pub expected_status: i32,
    pub failure_threshold: i32,
    pub consecutive_failures: i32,
    pub enabled: bool,
    pub last_status: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_uptime_monitors)]
pub struct NewNetworkToolUptimeMonitor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub last_status: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_uptime_events)]
pub struct NetworkToolUptimeEvent {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_uptime_events)]
pub struct NewNetworkToolUptimeEvent {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_cert_watches)]
pub struct NetworkToolCertWatch {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub domain: String,
    pub enabled: bool,
    pub last_days_remaining: Option<i32>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub alerted_thresholds: Vec<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_cert_watches)]
pub struct NewNetworkToolCertWatch {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub domain: String,
    pub last_days_remaining: Option<i32>,
    pub last_checked_at: Option<DateTime<Utc>>,
}
