//! Network tool types: saved checks, runs, monitors, watch lists.

use unified_core::json;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants and limits.
// ---------------------------------------------------------------------------

/// Default connect timeout for network tools.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 5;

/// Default total timeout for a tool run.
pub const DEFAULT_TOTAL_TIMEOUT_SECS: u64 = 10;

/// Hard cap: no caller may ask for a longer total timeout than this.
pub const MAX_TOTAL_TIMEOUT_SECS: u64 = 30;

/// Hard cap: no caller may ask for a longer connect timeout than this.
pub const MAX_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Maximum ping echo requests per run.
pub const MAX_PING_COUNT: u32 = 5;

/// Maximum traceroute hops.
pub const MAX_TRACEROUTE_HOPS: u32 = 30;

/// Maximum bytes read when grabbing a service banner.
pub const MAX_BANNER_BYTES: usize = 1024;

/// Maximum bytes of a webhook response body returned to the caller.
pub const MAX_WEBHOOK_BODY_BYTES: usize = 4096;

/// Maximum bytes of raw WHOIS text returned to the caller.
pub const MAX_WHOIS_TEXT_CHARS: usize = 8192;

/// Maximum HTTP redirects followed by the HTTP checker.
pub const MAX_HTTP_REDIRECTS: usize = 5;

/// Rate limit: tool runs per actor per minute.
pub const RATE_LIMIT_RUNS_PER_MINUTE: u64 = 30;

/// Rate limit: read requests per actor per minute.
pub const RATE_LIMIT_READS_PER_MINUTE: u64 = 120;

/// Certificate expiry alert thresholds, in days (same windows as the
/// certificates module: 30 / 14 / 7 / 1, plus -1 for expired).
pub const CERT_ALERT_THRESHOLDS_DAYS: [i64; 4] = [30, 14, 7, 1];

/// Default DNSBL zones queried by the blacklist check. Operators may
/// override the list per call.
pub const DEFAULT_DNSBL_ZONES: [&str; 8] = [
    "zen.spamhaus.org",
    "bl.spamcop.net",
    "dnsbl.sorbs.net",
    "b.barracudacentral.org",
    "bl.mailspike.net",
    "dnsbl-1.uceprotect.net",
    "all.s5h.net",
    "db.wpbl.info",
];

/// Hook event names emitted by this module.
pub mod network_tool_events {
    pub const RUN: &str = "network_tools.run";
    pub const RESULT_SHARED: &str = "network_tools.result.shared";
    pub const UPTIME_CHECK_REQUESTED: &str = "network_tools.uptime.check_requested";
    pub const UPTIME_UP: &str = "network_tools.uptime.up";
    pub const UPTIME_DOWN: &str = "network_tools.uptime.down";
    pub const CERT_CHECK_REQUESTED: &str = "network_tools.cert.check_requested";
    pub const CERT_EXPIRING: &str = "network_tools.cert.expiring";
}

// ---------------------------------------------------------------------------
// Row types.
//
// These are the Diesel row types for the migration
// `2026-11-22-000069_network_tools`. The integrator moves them verbatim
// into crates/unified-core/src/db/models.rs` (with the matching `table!` blocks in
// crates/unified-core/src/db/schema.rs`, see the delivery report) and this module's
// imports are then updated to `unified_core::db::models::...`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = unified_core::db::schema::network_tool_saved_checks)]
#[diesel(check_for_backend(diesel::pg::Pg))]
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

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = unified_core::db::schema::network_tool_runs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
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

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = unified_core::db::schema::network_tool_uptime_monitors)]
#[diesel(check_for_backend(diesel::pg::Pg))]
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

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = unified_core::db::schema::network_tool_uptime_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NetworkToolUptimeEvent {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = unified_core::db::schema::network_tool_cert_watches)]
#[diesel(check_for_backend(diesel::pg::Pg))]
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

// ---------------------------------------------------------------------------
// Request / response types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ToolTargetRequest {
    pub target: String,
    #[serde(default)]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DnsLookupRequest {
    pub target: String,
    #[serde(default = "default_dns_record_type")]
    pub record_type: String,
}

pub(crate) fn default_dns_record_type() -> String {
    "A".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct PingRequest {
    pub target: String,
    #[serde(default = "default_ping_count")]
    pub count: u32,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub(crate) fn default_ping_count() -> u32 {
    4
}

#[derive(Debug, Clone, Deserialize)]
pub struct TracerouteRequest {
    pub target: String,
    #[serde(default = "default_traceroute_hops")]
    pub max_hops: u32,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub(crate) fn default_traceroute_hops() -> u32 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortCheckRequest {
    pub target: String,
    pub port: u16,
    #[serde(default)]
    pub grab_banner: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WhoisRequest {
    pub target: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpCheckRequest {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub expected_status: Option<u16>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookTestRequest {
    pub url: String,
    #[serde(default = "default_webhook_payload")]
    pub payload: Value,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub(crate) fn default_webhook_payload() -> Value {
    json!({ "event": "network_tools.test", "ok": true })
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubnetRequest {
    pub cidr: String,
    #[serde(default)]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlacklistRequest {
    pub target: String,
    #[serde(default)]
    pub zones: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSavedCheckRequest {
    pub name: String,
    pub tool: String,
    pub target: String,
    #[serde(default = "default_empty_params")]
    pub parameters: Value,
    #[serde(default)]
    pub company_id: Option<String>,
}

pub(crate) fn default_empty_params() -> Value {
    json!({})
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSavedCheckRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateUptimeMonitorRequest {
    pub name: String,
    pub url: String,
    #[serde(default = "default_monitor_interval")]
    pub interval_seconds: i32,
    #[serde(default = "default_expected_status")]
    pub expected_status: i32,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: i32,
    #[serde(default)]
    pub company_id: Option<String>,
}

pub(crate) fn default_monitor_interval() -> i32 {
    300
}

pub(crate) fn default_expected_status() -> i32 {
    200
}

pub(crate) fn default_failure_threshold() -> i32 {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUptimeMonitorRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub expected_status: Option<i32>,
    #[serde(default)]
    pub failure_threshold: Option<i32>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCertWatchRequest {
    pub domain: String,
    #[serde(default)]
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShareRunRequest {
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

/// One tool result, as returned inside the standard envelope. The
/// `method` / `notes` fields always record how the result was
/// produced, so a fallback path is never silently presented as the
/// primary one.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool: String,
    pub target: String,
    pub outcome: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub result: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}
