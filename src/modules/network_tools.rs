// Network Tools module.
//
// Developer network diagnostics: DNS lookup, ping, traceroute, port
// check, SSL/TLS checker, WHOIS, HTTP header/status check, IP info and
// Geo, subnet/CIDR calculator, webhook tester, uptime checks (one-off
// and scheduled monitors), certificate expiry watch, and DNS blacklist
// checks. Saved checks, run history, and shareable results round out
// the module.
//
// Safety model (read this before extending the module):
//   - Every tool runs server-side with strict limits: per-tool
//     timeouts (default 5s connect / 10s total, configurable per call
//     within hard caps), per-actor rate limits held in memory, and
//     SSRF guards that resolve hostnames and check EVERY resolved IP.
//     Private, loopback, link-local, reserved, and multicast targets
//     are rejected unless the caller holds
//     `network_tools:probe_private`. URLs with credentials, non-http(s)
//     schemes, and redirect targets that fail the same check are
//     rejected as well.
//   - Every tool run is audit-logged: the run is stored in the run
//     history and a `network_tools.run` hook event is emitted carrying
//     the tool, target, duration, and outcome - never the response
//     body.
//   - Ping and traceroute shell out to the system `ping` /
//     `traceroute` binaries with strictly validated arguments and a
//     hard timeout, because no raw-socket crate is available. When the
//     binary is absent, ping falls back to TCP-connect timing and the
//     result records which path was used; traceroute has no faithful
//     fallback and reports the binary as unavailable instead of
//     inventing hops.
//
// Reuse seams (do not rebuild these):
//   - DNS: `crate::modules::network::DnsVerifier` for A/AAAA/MX/TXT/
//     CNAME/NS/PTR. SOA is resolved through hickory directly because
//     `DnsVerifier` does not expose an SOA lookup.
//   - Geo/IP info: `crate::modules::geo::lookup_geo`.
//   - Certificates: the live TLS storage on `AppContext.tls`
//     (`crate::http::tls::SslStorage::list_certificates`) is consulted
//     first for domains the server already manages; the SSL/TLS
//     checker below only probes the network for other domains. This
//     module does NOT duplicate the certificates module alert store:
//     its own watch list only records which domains to watch and which
//     thresholds were already alerted, and it reuses the same
//     30/14/7/1-day thresholds.
//   - Public-link sharing: `crate::core::sharing` plus the auditors
//     module (`POST /api/v1/sharing/links`, scope kind
//     `network_tools_run`). A run result is shared by minting a link
//     through that API; this module registers the shareable kind and
//     emits `network_tools.result.shared`, it does not mint tokens
//     itself.
//
// Capabilities (roles arrive as trusted query parameters until the
// auth extractor lands - the same known gap as the projects module;
// see `viewer_of` / `require_capability`):
//   - `network_tools:view` - read history, saved checks, monitors,
//     watches, tool metadata.
//   - `network_tools:run` - execute any tool.
//   - `network_tools:manage` - create/update/delete saved checks,
//     uptime monitors, certificate watches; share results; trigger
//     checks.
//   - `network_tools:probe_private` - probe private / loopback /
//     link-local / reserved / multicast targets.
//
// Routes live under `/api/v1/network-tools`. All responses use the
// standard JSON envelope (`crate::core::json::respond`).
//
// The scheduler has no network-tools job kind yet. Scheduled uptime
// checks are exposed as the `network_tools.uptime.check_requested`
// hook event plus the `POST /uptime-monitors/{id}/check` trigger
// endpoint; the integrator must arm a scheduler job that emits that
// event (or calls the trigger endpoint) at each monitor interval.
// Failures emit `network_tools.uptime.down` and request a
// notification through `notification.requested`, using the same direct
// pattern the certificates module uses for `ssl.expiring`.

use crate::core::context::{AppContext, GeoInfo};
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, Pagination, PaginationParams};
use crate::core::sharing;
use crate::modules::geo;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use hickory_proto::rr::{Name, RData, RecordType};
use hickory_resolver::{ResolverConfig, ResolverOpts, TokioResolver};
use rustls::client::danger_client::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::time::timeout;
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
// into `src/db/models.rs` (with the matching `table!` blocks in
// `src/db/schema.rs`, see the delivery report) and this module's
// imports are then updated to `crate::db::models::...`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::db::schema::network_tool_saved_checks)]
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
#[diesel(table_name = crate::db::schema::network_tool_runs)]
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
#[diesel(table_name = crate::db::schema::network_tool_uptime_monitors)]
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
#[diesel(table_name = crate::db::schema::network_tool_uptime_events)]
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
#[diesel(table_name = crate::db::schema::network_tool_cert_watches)]
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

fn default_dns_record_type() -> String {
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

fn default_ping_count() -> u32 {
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

fn default_traceroute_hops() -> u32 {
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

fn default_webhook_payload() -> Value {
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

fn default_empty_params() -> Value {
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

fn default_monitor_interval() -> i32 {
    300
}

fn default_expected_status() -> i32 {
    200
}

fn default_failure_threshold() -> i32 {
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

// ---------------------------------------------------------------------------
// In-memory stores (authoritative until the integrator lands the
// schema.rs / models.rs additions; the Diesel helpers below mirror
// them 1:1 and take over as the read path once a pool is attached).
// ---------------------------------------------------------------------------

static SAVED_CHECKS: OnceLock<DashMap<Uuid, NetworkToolSavedCheck>> = OnceLock::new();
static RUNS: OnceLock<DashMap<Uuid, NetworkToolRun>> = OnceLock::new();
static UPTIME_MONITORS: OnceLock<DashMap<Uuid, NetworkToolUptimeMonitor>> = OnceLock::new();
static UPTIME_EVENTS: OnceLock<DashMap<Uuid, NetworkToolUptimeEvent>> = OnceLock::new();
static CERT_WATCHES: OnceLock<DashMap<Uuid, NetworkToolCertWatch>> = OnceLock::new();
/// Rate-limit buckets: key `actor:scope` -> (window start, hits).
static RATE_LIMITS: OnceLock<DashMap<String, (Instant, u64)>> = OnceLock::new();
static NETWORK_TOOLS_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

fn saved_checks_store() -> &'static DashMap<Uuid, NetworkToolSavedCheck> {
    SAVED_CHECKS.get_or_init(DashMap::new)
}

fn runs_store() -> &'static DashMap<Uuid, NetworkToolRun> {
    RUNS.get_or_init(DashMap::new)
}

fn uptime_monitors_store() -> &'static DashMap<Uuid, NetworkToolUptimeMonitor> {
    UPTIME_MONITORS.get_or_init(DashMap::new)
}

fn uptime_events_store() -> &'static DashMap<Uuid, NetworkToolUptimeEvent> {
    UPTIME_EVENTS.get_or_init(DashMap::new)
}

fn cert_watches_store() -> &'static DashMap<Uuid, NetworkToolCertWatch> {
    CERT_WATCHES.get_or_init(DashMap::new)
}

fn rate_limits_store() -> &'static DashMap<String, (Instant, u64)> {
    RATE_LIMITS.get_or_init(DashMap::new)
}

fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    // Same approach as the licensing module: rebuild an AppContext
    // from the shared Arcs, so synchronous hook handlers can reach
    // the hook bus, the database pool, and the TLS storage. Only the
    // uptime and maintenance flags are snapshots.
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        storage_registry: ctx.storage_registry.clone(),
        routes: ctx.routes.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

fn hook_ctx() -> Option<Arc<AppContext>> {
    NETWORK_TOOLS_CTX.get().cloned()
}

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

// ---------------------------------------------------------------------------
// Viewer identity and capability checks.
//
// KNOWN GAP (same as the projects module): `viewer_id` and
// `viewer_role` arrive as trusted query parameters until the auth
// extractor lands. Every handler below still performs the check, so
// the enforcement point is in place and only the identity source
// needs to change.
// ---------------------------------------------------------------------------

/// Resolve the requester's identity from the list query parameters.
/// Returns (viewer id, viewer role, company id); any side may be
/// absent.
fn viewer_of(params: &PaginationParams) -> (Option<String>, Option<String>, Option<String>) {
    let pick = |key: &str| {
        params
            .filters
            .get(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    (pick("viewer_id"), pick("viewer_role"), pick("company_id"))
}

/// Parse a company id from a query/body string. A value that is not
/// a UUID is treated as absent rather than failing the request: the
/// company filter is advisory, the capability check is authoritative.
fn parse_company_id(value: Option<String>) -> Option<Uuid> {
    value.and_then(|value| Uuid::parse_str(value.trim()).ok())
}

/// Capabilities granted to each built-in role. `admin` and
/// `super_admin` hold everything; `operator` and `developer` may run
/// and manage tools but may not probe private targets; `viewer` and
/// `auditor` are read-only. Unknown or absent roles hold nothing.
fn role_capabilities(role: &str) -> &'static [&'static str] {
    match role {
        "admin" | "super_admin" => &[
            "network_tools:view",
            "network_tools:run",
            "network_tools:manage",
            "network_tools:probe_private",
        ],
        "operator" | "developer" => &[
            "network_tools:view",
            "network_tools:run",
            "network_tools:manage",
        ],
        "viewer" | "auditor" => &["network_tools:view"],
        _ => &[],
    }
}

fn has_capability(role: Option<&str>, capability: &str) -> bool {
    match role {
        Some(role) => role_capabilities(role).contains(&capability),
        None => false,
    }
}

/// A caller may touch a record they own, or any record when they
/// hold the manage capability. Anything else is forbidden.
fn require_owner_or_manage(
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    owner: &str,
) -> UnifiedResult<()> {
    if viewer_id == Some(owner) || has_capability(viewer_role, "network_tools:manage") {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "A record can only be read or changed by its owner or a manager",
        ))
    }
}

fn require_capability(role: Option<&str>, capability: &str) -> UnifiedResult<()> {
    if has_capability(role, capability) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {capability}"
        )))
    }
}

// ---------------------------------------------------------------------------
// Rate limiting (in-memory, per actor, fixed 60-second windows).
// ---------------------------------------------------------------------------

fn check_rate_limit(actor: &str, scope: &str, limit: u64) -> UnifiedResult<()> {
    let key = format!("{actor}:{scope}");
    let now = Instant::now();
    let mut entry = rate_limits_store().entry(key).or_insert((now, 0));
    if entry.0.elapsed() >= Duration::from_secs(60) {
        *entry = (now, 1);
        return Ok(());
    }
    entry.1 += 1;
    if entry.1 > limit {
        return Err(UnifiedError::new(
            format!("Rate limit exceeded for {scope}: at most {limit} requests per minute"),
            429,
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Input validation and SSRF guards.
// ---------------------------------------------------------------------------

/// A hostname is accepted when it is a dotted name of labels made of
/// letters, digits, and hyphens (no leading/trailing hyphen, no empty
/// labels, at most 253 characters), or a literal IP address.
fn valid_hostname_or_ip(target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() || target.len() > 253 {
        return false;
    }
    if target.parse::<IpAddr>().is_ok() {
        return true;
    }
    if target.starts_with('.') || target.ends_with('.') || target.contains("..") {
        return false;
    }
    target.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

fn validate_target(target: &str) -> UnifiedResult<String> {
    let target = target.trim().to_string();
    if target.is_empty() {
        return Err(UnifiedError::bad_request("Target must not be empty"));
    }
    if target.contains('/') {
        return Err(UnifiedError::bad_request(
            "Target must be a hostname or IP address, not a URL or path",
        ));
    }
    if !valid_hostname_or_ip(&target) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid hostname or IP address: \"{target}\""
        )));
    }
    Ok(target)
}

fn validate_domain(domain: &str) -> UnifiedResult<String> {
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if domain.is_empty() || domain.parse::<IpAddr>().is_ok() {
        return Err(UnifiedError::bad_request(
            "Expected a domain name, not an empty value or an IP address",
        ));
    }
    if !valid_hostname_or_ip(&domain) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid domain name: \"{domain}\""
        )));
    }
    Ok(domain)
}

/// True when the address is NOT safe to probe from the server:
/// private, loopback, link-local, unspecified, reserved, or
/// multicast ranges, in either address family (including
/// IPv4-mapped IPv6 forms).
fn ip_is_restricted(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // IPv4-mapped / compatible forms are handled by the
            // caller through `normalize_ip`; here the plain checks.
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_reserved()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // Carrier-grade NAT and documentation ranges.
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || matches!(v6.segments(), [0, 0, 0, 0, 0, 0xffff, _, _])
                || v6.to_ipv4_mapped().map(ip_is_restricted).unwrap_or(false)
                || {
                    // Unique-local (fc00::/7) and link-local (fe80::/10).
                    let first = v6.segments()[0];
                    (0xfc00..=0xfdff).contains(&first) || (0xfe80..=0xfebf).contains(&first)
                }
        }
    }
}

/// Resolve a hostname and check EVERY resolved address. Returns the
/// resolved addresses on success. When `allow_private` is false any
/// restricted address fails the whole target.
async fn resolve_and_guard(
    host: &str,
    allow_private: bool,
) -> UnifiedResult<Vec<IpAddr>> {
    let host = host.trim();
    if host.is_empty() {
        return Err(UnifiedError::bad_request("Host must not be empty"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !allow_private && ip_is_restricted(ip) {
            return Err(UnifiedError::forbidden(format!(
                "Target {ip} is in a restricted range; probing it requires network_tools:probe_private"
            )));
        }
        return Ok(vec![ip]);
    }
    if !valid_hostname_or_ip(host) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid hostname: \"{host}\""
        )));
    }
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host, 0))
        .await
        .map_err(|error| {
            UnifiedError::bad_request(format!("Cannot resolve \"{host}\": {error}"))
        })?
        .map(|socket| socket.ip())
        .collect();
    if addrs.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "Hostname \"{host}\" did not resolve to any address"
        )));
    }
    if !allow_private {
        for ip in &addrs {
            if ip_is_restricted(*ip) {
                return Err(UnifiedError::forbidden(format!(
                    "Hostname \"{host}\" resolves to restricted address {ip}; probing it requires network_tools:probe_private"
                )));
            }
        }
    }
    Ok(addrs)
}

/// Validate a caller-supplied URL for the HTTP / webhook tools:
/// http(s) only, no embedded credentials, host passes the SSRF guard.
/// Returns the parsed URL plus its resolved addresses.
async fn guard_url(url_text: &str, allow_private: bool) -> UnifiedResult<(String, Vec<IpAddr>)> {
    let url_text = url_text.trim();
    if url_text.len() > 2048 {
        return Err(UnifiedError::bad_request("URL is too long"));
    }
    let parsed = url_text
        .parse::<reqwest::Url>()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid URL: \"{url_text}\"")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Only http and https URLs are allowed, not \"{other}\""
            )))
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UnifiedError::bad_request(
            "URLs with embedded credentials are not allowed",
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(UnifiedError::bad_request("URL has no host"));
    };
    let addrs = resolve_and_guard(host, allow_private).await?;
    Ok((parsed.to_string(), addrs))
}

fn clamp_timeout(requested: Option<u64>, default_secs: u64, max_secs: u64) -> Duration {
    let secs = requested.unwrap_or(default_secs).clamp(1, max_secs);
    Duration::from_secs(secs)
}

fn clamp_connect_timeout(requested: Option<u64>) -> Duration {
    clamp_timeout(
        requested,
        DEFAULT_CONNECT_TIMEOUT_SECS,
        MAX_CONNECT_TIMEOUT_SECS,
    )
}

fn clamp_total_timeout(requested: Option<u64>) -> Duration {
    clamp_timeout(requested, DEFAULT_TOTAL_TIMEOUT_SECS, MAX_TOTAL_TIMEOUT_SECS)
}

// ---------------------------------------------------------------------------
// Run recording: every tool run is stored and a hook event is emitted.
// The stored summary never contains a response body.
// ---------------------------------------------------------------------------

fn record_run(
    ctx: &AppContext,
    actor: &str,
    company_id: Option<&str>,
    tool: &str,
    target: &str,
    started: Instant,
    outcome: &str,
    summary: Value,
) -> Uuid {
    let run = NetworkToolRun {
        id: Uuid::new_v4(),
        user_id: actor.to_string(),
        company_id: company_id.and_then(|value| Uuid::parse_str(value).ok()),
        tool: tool.to_string(),
        target: target.chars().take(512).collect(),
        duration_ms: started.elapsed().as_millis() as i64,
        outcome: outcome.to_string(),
        summary,
        created_at: Utc::now(),
    };
    runs_store().insert(run.id, run.clone());
    if let Some(pool) = db_pool(ctx) {
        let _ = db_insert_run(&pool, &run);
    }
    ctx.hooks.emit(
        network_tool_events::RUN,
        json!({
            "run_id": run.id,
            "tool": tool,
            "target": run.target,
            "actor": actor,
            "duration_ms": run.duration_ms,
            "outcome": outcome,
        }),
    );
    run.id
}

fn notify(ctx: &AppContext, user_id: &str, title: String, body: String, kind: &str) {
    // Direct `notification.requested`, same pattern as the
    // certificates module's `ssl.expiring` alerts: the notifications
    // module does not subscribe to these events, so without this
    // request the alert would land nowhere.
    ctx.hooks.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "title": title,
            "body": body,
            "kind": kind,
        }),
    );
}

// ---------------------------------------------------------------------------
// Diesel helpers (persistence path; in-memory stores above stay the
// read path until the integrator adds the matching `table!` blocks
// to `src/db/schema.rs` - see the delivery report).
// ---------------------------------------------------------------------------

fn db_insert_run(pool: &crate::db::DbPool, run: &NetworkToolRun) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_runs::dsl as runs_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(runs_dsl::network_tool_runs)
        .values(run)
        .execute(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot store tool run: {error}")))?;
    Ok(())
}

fn db_load_runs(pool: &crate::db::DbPool) -> UnifiedResult<Vec<NetworkToolRun>> {
    use crate::db::schema::network_tool_runs::dsl as runs_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    runs_dsl::network_tool_runs
        .select(NetworkToolRun::as_select())
        .order(runs_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load tool runs: {error}")))
}

fn db_load_saved_checks(pool: &crate::db::DbPool) -> UnifiedResult<Vec<NetworkToolSavedCheck>> {
    use crate::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    checks_dsl::network_tool_saved_checks
        .select(NetworkToolSavedCheck::as_select())
        .order(checks_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load saved checks: {error}"))
        })
}

fn db_insert_saved_check(
    pool: &crate::db::DbPool,
    check: &NetworkToolSavedCheck,
) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(checks_dsl::network_tool_saved_checks)
        .values(check)
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store saved check: {error}"))
        })?;
    Ok(())
}

fn db_upsert_saved_check(
    pool: &crate::db::DbPool,
    check: &NetworkToolSavedCheck,
) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(checks_dsl::network_tool_saved_checks)
        .values(check)
        .on_conflict(checks_dsl::id)
        .do_update()
        .set((
            checks_dsl::name.eq(&check.name),
            checks_dsl::tool.eq(&check.tool),
            checks_dsl::target.eq(&check.target),
            checks_dsl::parameters.eq(&check.parameters),
            checks_dsl::updated_at.eq(check.updated_at),
        ))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store saved check: {error}"))
        })?;
    Ok(())
}

fn db_delete_saved_check(pool: &crate::db::DbPool, id: Uuid) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::delete(checks_dsl::network_tool_saved_checks.filter(checks_dsl::id.eq(id)))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot delete saved check: {error}"))
        })?;
    Ok(())
}

fn db_load_monitors(pool: &crate::db::DbPool) -> UnifiedResult<Vec<NetworkToolUptimeMonitor>> {
    use crate::db::schema::network_tool_uptime_monitors::dsl as mon_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    mon_dsl::network_tool_uptime_monitors
        .select(NetworkToolUptimeMonitor::as_select())
        .order(mon_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load uptime monitors: {error}"))
        })
}

fn db_upsert_monitor(
    pool: &crate::db::DbPool,
    monitor: &NetworkToolUptimeMonitor,
) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_uptime_monitors::dsl as mon_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(mon_dsl::network_tool_uptime_monitors)
        .values(monitor)
        .on_conflict(mon_dsl::id)
        .do_update()
        .set((
            mon_dsl::name.eq(&monitor.name),
            mon_dsl::url.eq(&monitor.url),
            mon_dsl::interval_seconds.eq(monitor.interval_seconds),
            mon_dsl::expected_status.eq(monitor.expected_status),
            mon_dsl::failure_threshold.eq(monitor.failure_threshold),
            mon_dsl::consecutive_failures.eq(monitor.consecutive_failures),
            mon_dsl::enabled.eq(monitor.enabled),
            mon_dsl::last_status.eq(&monitor.last_status),
            mon_dsl::last_checked_at.eq(monitor.last_checked_at),
            mon_dsl::updated_at.eq(monitor.updated_at),
        ))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store uptime monitor: {error}"))
        })?;
    Ok(())
}

fn db_delete_monitor(pool: &crate::db::DbPool, id: Uuid) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_uptime_monitors::dsl as mon_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::delete(mon_dsl::network_tool_uptime_monitors.filter(mon_dsl::id.eq(id)))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot delete uptime monitor: {error}"))
        })?;
    Ok(())
}

fn db_insert_uptime_event(
    pool: &crate::db::DbPool,
    event: &NetworkToolUptimeEvent,
) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_uptime_events::dsl as ev_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(ev_dsl::network_tool_uptime_events)
        .values(event)
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store uptime event: {error}"))
        })?;
    Ok(())
}

fn db_load_cert_watches(pool: &crate::db::DbPool) -> UnifiedResult<Vec<NetworkToolCertWatch>> {
    use crate::db::schema::network_tool_cert_watches::dsl as watch_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    watch_dsl::network_tool_cert_watches
        .select(NetworkToolCertWatch::as_select())
        .order(watch_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load certificate watches: {error}"))
        })
}

fn db_upsert_cert_watch(pool: &crate::db::DbPool, watch: &NetworkToolCertWatch) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_cert_watches::dsl as watch_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(watch_dsl::network_tool_cert_watches)
        .values(watch)
        .on_conflict(watch_dsl::id)
        .do_update()
        .set((
            watch_dsl::domain.eq(&watch.domain),
            watch_dsl::enabled.eq(watch.enabled),
            watch_dsl::last_days_remaining.eq(watch.last_days_remaining),
            watch_dsl::last_checked_at.eq(watch.last_checked_at),
            watch_dsl::alerted_thresholds.eq(&watch.alerted_thresholds),
            watch_dsl::updated_at.eq(watch.updated_at),
        ))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store certificate watch: {error}"))
        })?;
    Ok(())
}

fn db_delete_cert_watch(pool: &crate::db::DbPool, id: Uuid) -> UnifiedResult<()> {
    use crate::db::schema::network_tool_cert_watches::dsl as watch_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::delete(watch_dsl::network_tool_cert_watches.filter(watch_dsl::id.eq(id)))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot delete certificate watch: {error}"))
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pagination helper (same shape as the licensing module's `paginate`).
// ---------------------------------------------------------------------------

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination: Pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Tool implementations. Each returns a `ToolResult`; handlers record
// the run and wrap it in the standard envelope.
// ---------------------------------------------------------------------------

/// DNS lookup for one record type. SOA is resolved through hickory
/// directly because `DnsVerifier` does not expose an SOA lookup.
async fn tool_dns_lookup(target: &str, record_type: &str) -> UnifiedResult<Value> {
    let domain = validate_domain(target)?;
    let verifier = DnsVerifier::new();
    let rtype = record_type.trim().to_uppercase();
    match rtype.as_str() {
        "A" => {
            let addrs = verifier.lookup_a(&domain).await?;
            Ok(json!({ "record_type": "A", "domain": domain, "records": addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>() }))
        }
        "AAAA" => {
            let addrs = verifier.lookup_aaaa(&domain).await?;
            Ok(json!({ "record_type": "AAAA", "domain": domain, "records": addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>() }))
        }
        "MX" => {
            let records = verifier.lookup_mx(&domain).await?;
            Ok(json!({ "record_type": "MX", "domain": domain, "records": records }))
        }
        "TXT" => {
            let records = verifier.lookup_txt(&domain).await?;
            Ok(json!({ "record_type": "TXT", "domain": domain, "records": records }))
        }
        "CNAME" => {
            let record = verifier.lookup_cname(&domain).await?;
            Ok(json!({ "record_type": "CNAME", "domain": domain, "records": record.into_iter().collect::<Vec<_>>() }))
        }
        "NS" => {
            let records = verifier.lookup_ns(&domain).await?;
            Ok(json!({ "record_type": "NS", "domain": domain, "records": records }))
        }
        "SOA" => {
            // Same resolver construction as `DnsVerifier` (which does
            // not expose an SOA lookup): default config, parsed Name.
            let name: Name = format!("{domain}.")
                .parse()
                .map_err(|_| UnifiedError::bad_request(format!("Invalid domain name \"{domain}\"")))?;
            let resolver = TokioResolver::builder(ResolverConfig::default(), ResolverOpts::default())
                .build();
            let lookup = resolver.lookup(name, RecordType::SOA).await.map_err(|error| {
                UnifiedError::bad_request(format!("SOA lookup failed for \"{domain}\": {error}"))
            })?;
            let mut records: Vec<Value> = Vec::new();
            for record in lookup.iter() {
                if let RData::SOA(soa) = record {
                    records.push(json!({
                        "mname": soa.mname().to_string(),
                        "rname": soa.rname().to_string(),
                        "serial": soa.serial(),
                        "refresh": soa.refresh(),
                        "retry": soa.retry(),
                        "expire": soa.expire(),
                        "minimum": soa.minimum(),
                    }));
                }
            }
            Ok(json!({ "record_type": "SOA", "domain": domain, "records": records }))
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unsupported DNS record type \"{other}\"; use A, AAAA, MX, TXT, CNAME, NS, or SOA"
        ))),
    }
}

/// Run the system `ping` binary with strictly validated arguments.
/// Returns (stdout, method). The target is validated as a hostname
/// or IP before it ever reaches the command line, and count/timeout
/// are clamped, so no shell metacharacter can be injected (no shell
/// is involved at all: arguments are passed as an argv array).
async fn run_ping_binary(
    target: &str,
    count: u32,
    timeout_secs: u64,
) -> UnifiedResult<(String, String)> {
    let output = timeout(
        Duration::from_secs(timeout_secs + 5),
        tokio::process::Command::new("ping")
            .arg("-c")
            .arg(count.to_string())
            .arg("-W")
            .arg(timeout_secs.to_string())
            .arg(target)
            .output(),
    )
    .await
    .map_err(|_| UnifiedError::internal("Ping timed out at the runner level"))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            UnifiedError::new(
                "The system `ping` binary is not available on this host; TCP-connect timing was used instead",
                503,
            )
        } else {
            UnifiedError::internal(format!("Cannot run the `ping` binary: {error}"))
        }
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((stdout, "system ping binary".to_string()))
}

fn parse_ping_output(stdout: &str) -> Value {
    let mut transmitted: Option<u32> = None;
    let mut received: Option<u32> = None;
    let mut packet_loss_pct: Option<f64> = None;
    let mut rtt_min_ms: Option<f64> = None;
    let mut rtt_avg_ms: Option<f64> = None;
    let mut rtt_max_ms: Option<f64> = None;
    let mut rtt_mdev_ms: Option<f64> = None;
    for line in stdout.lines() {
        if line.contains("packets transmitted") {
            // `4 packets transmitted, 4 received, 0% packet loss, time 3002ms`
            let parts: Vec<&str> = line.split(',').collect();
            for part in parts {
                let part = part.trim();
                if let Some(num) = part.split_whitespace().next() {
                    if part.contains("packets transmitted") {
                        transmitted = num.parse().ok();
                    } else if part.contains("received") {
                        received = num.parse().ok();
                    } else if part.contains("packet loss") {
                        packet_loss_pct = part
                            .split_whitespace()
                            .next()
                            .map(|n| n.trim_end_matches('%'))
                            .and_then(|n| n.parse().ok());
                    }
                }
            }
        }
        if line.contains("rtt min/avg/max/mdev") {
            // `rtt min/avg/max/mdev = 12.1/13.4/15.2/1.1 ms`
            if let Some(values) = line.split('=').nth(1) {
                let nums: Vec<f64> = values
                    .trim()
                    .trim_end_matches(" ms")
                    .split('/')
                    .filter_map(|n| n.trim().parse().ok())
                    .collect();
                if nums.len() == 4 {
                    rtt_min_ms = Some(nums[0]);
                    rtt_avg_ms = Some(nums[1]);
                    rtt_max_ms = Some(nums[2]);
                    rtt_mdev_ms = Some(nums[3]);
                }
            }
        }
    }
    json!({
        "transmitted": transmitted,
        "received": received,
        "packet_loss_pct": packet_loss_pct,
        "rtt_min_ms": rtt_min_ms,
        "rtt_avg_ms": rtt_avg_ms,
        "rtt_max_ms": rtt_max_ms,
        "rtt_mdev_ms": rtt_mdev_ms,
        "raw": stdout.chars().take(4096).collect::<String>(),
    })
}

/// TCP-connect timing fallback for ping when the system binary is
/// absent: measures connect latency to ports 80 and 443 and reports
/// the fastest successful one. This is NOT ICMP; the result says so.
async fn ping_tcp_fallback(addrs: &[IpAddr], timeout_secs: u64) -> Value {
    let mut best_ms: Option<f64> = None;
    let mut tried = 0u32;
    let mut succeeded = 0u32;
    for ip in addrs {
        for port in [80u16, 443u16] {
            tried += 1;
            let addr = SocketAddr::new(*ip, port);
            let started = Instant::now();
            match timeout(
                Duration::from_secs(timeout_secs),
                TokioTcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(_stream)) => {
                    succeeded += 1;
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    best_ms = Some(best_ms.map_or(ms, |best: f64| best.min(ms)));
                }
                _ => {}
            }
        }
    }
    let loss = if tried == 0 {
        None
    } else {
        Some(((tried - succeeded) as f64 / tried as f64) * 100.0)
    };
    json!({
        "transmitted": tried,
        "received": succeeded,
        "packet_loss_pct": loss,
        "rtt_min_ms": best_ms,
        "rtt_avg_ms": best_ms,
        "rtt_max_ms": best_ms,
        "rtt_mdev_ms": null,
        "raw": "",
    })
}

async fn tool_ping(
    target: &str,
    count: u32,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let count = count.clamp(1, MAX_PING_COUNT);
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    let addrs = resolve_and_guard(&target, allow_private).await?;
    let mut notes: Vec<String> = Vec::new();
    let (result, method, outcome) = match run_ping_binary(&target, count, timeout_secs).await {
        Ok((stdout, method)) => {
            let parsed = parse_ping_output(&stdout);
            let loss = parsed
                .get("packet_loss_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(100.0);
            let outcome = if loss < 100.0 { "success" } else { "failed" };
            (parsed, method, outcome)
        }
        Err(error) if error.code == 503 => {
            notes.push(
                "The system `ping` binary is not available on this host; result is TCP-connect timing to ports 80/443, not ICMP echo."
                    .to_string(),
            );
            let parsed = ping_tcp_fallback(&addrs, timeout_secs).await;
            let loss = parsed
                .get("packet_loss_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(100.0);
            let outcome = if loss < 100.0 { "success" } else { "failed" };
            (
                parsed,
                "tcp connect timing fallback (no ping binary)".to_string(),
                outcome,
            )
        }
        Err(error) => return Err(error),
    };
    Ok(ToolResult {
        tool: "ping".to_string(),
        target,
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some(method),
        result,
        notes,
    })
}

fn parse_traceroute_output(stdout: &str) -> Vec<Value> {
    let mut hops: Vec<Value> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("traceroute") {
            continue;
        }
        // ` 1  192.168.1.1 (192.168.1.1)  1.234 ms  1.102 ms`
        // or ` 2  * * *` for a silent hop.
        let mut parts = line.split_whitespace();
        let Some(hop_no) = parts.next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let mut host: Option<String> = None;
        let mut ip: Option<String> = None;
        let mut rtts: Vec<f64> = Vec::new();
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (index, token) in tokens.iter().enumerate() {
            if *token == "*" {
                continue;
            }
            if token.starts_with('(') && token.ends_with(')') {
                ip = Some(token.trim_matches(|c| c == '(' || c == ')').to_string());
                if index > 1 {
                    host = Some(tokens[index - 1].to_string());
                }
            } else if *token == "ms" {
                if index > 0 {
                    if let Ok(ms) = tokens[index - 1].parse::<f64>() {
                        rtts.push(ms);
                    }
                }
            } else if host.is_none()
                && !token.parse::<f64>().is_ok()
                && *token != "ms"
                && index > 0
            {
                // A bare hostname or IP token with no parentheses.
                if token.parse::<IpAddr>().is_ok() {
                    ip = Some(token.to_string());
                } else {
                    host = Some(token.to_string());
                }
            }
        }
        let _ = parts;
        hops.push(json!({
            "hop": hop_no,
            "host": host,
            "ip": ip,
            "rtt_ms": rtts,
        }));
    }
    hops
}

async fn tool_traceroute(
    target: &str,
    max_hops: u32,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let max_hops = max_hops.clamp(1, MAX_TRACEROUTE_HOPS);
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    let _ = resolve_and_guard(&target, allow_private).await?;
    let output = timeout(
        Duration::from_secs(timeout_secs * u64::from(max_hops) + 10),
        tokio::process::Command::new("traceroute")
            .arg("-n")
            .arg("-q")
            .arg("1")
            .arg("-m")
            .arg(max_hops.to_string())
            .arg("-w")
            .arg(timeout_secs.to_string())
            .arg(&target)
            .output(),
    )
    .await
    .map_err(|_| UnifiedError::internal("Traceroute timed out at the runner level"))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            UnifiedError::new(
                "The system `traceroute` binary is not available on this host; hop resolution depends on the host tool and has no faithful fallback",
                503,
            )
        } else {
            UnifiedError::internal(format!("Cannot run the `traceroute` binary: {error}"))
        }
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let hops = parse_traceroute_output(&stdout);
    let outcome = if hops.is_empty() { "failed" } else { "success" };
    Ok(ToolResult {
        tool: "traceroute".to_string(),
        target,
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("system traceroute binary".to_string()),
        result: json!({ "hops": hops }),
        notes: vec![
            "Hop hostnames are resolved by the host `traceroute` tool; a hop with no hostname or IP timed out or does not answer.".to_string(),
        ],
    })
}

async fn tool_port_check(
    target: &str,
    port: u16,
    grab_banner: bool,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let addrs = resolve_and_guard(&target, allow_private).await?;
    let connect_timeout = clamp_connect_timeout(Some(timeout_secs));
    let mut open = false;
    let mut latency_ms: Option<f64> = None;
    let mut banner: Option<String> = None;
    let mut last_error: Option<String> = None;
    for ip in &addrs {
        let addr = SocketAddr::new(*ip, port);
        let attempt = Instant::now();
        match timeout(connect_timeout, TokioTcpStream::connect(addr)).await {
            Ok(Ok(mut stream)) => {
                open = true;
                latency_ms = Some(attempt.elapsed().as_secs_f64() * 1000.0);
                if grab_banner {
                    let mut buf = vec![0u8; MAX_BANNER_BYTES];
                    match timeout(Duration::from_secs(2), stream.read(&mut buf)).await {
                        Ok(Ok(read)) if read > 0 => {
                            banner = Some(
                                String::from_utf8_lossy(&buf[..read])
                                    .chars()
                                    .take(MAX_BANNER_BYTES)
                                    .collect(),
                            );
                        }
                        _ => {}
                    }
                }
                break;
            }
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("connect timed out".to_string()),
        }
    }
    let outcome = if open { "success" } else { "failed" };
    Ok(ToolResult {
        tool: "port_check".to_string(),
        target: format!("{target}:{port}"),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("tcp connect".to_string()),
        result: json!({
            "host": target,
            "port": port,
            "open": open,
            "latency_ms": latency_ms,
            "banner": banner,
            "error": last_error,
            "resolved": addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
        }),
        notes: if grab_banner {
            vec!["Banner grab only reads up to 1 KB and never sends a payload.".to_string()]
        } else {
            Vec::new()
        },
    })
}

// ---------------------------------------------------------------------------
// SSL/TLS checker.
// ---------------------------------------------------------------------------

/// Accept-any verifier used ONLY by the inspection tool: the checker
/// must report untrusted or expired chains, not refuse the handshake.
/// Chain trust is reported separately in the `validation_notes`.
/// (Same shape as the domains module's `AcceptAnyCertificate`.)
#[derive(Debug)]
struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger_client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger_client::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger_client::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger_client::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger_client::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger_client::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

/// Read one TLV (tag + length + value) from `der` starting at
/// `offset`. Returns (tag, value slice, next offset).
fn der_tlv<'a>(der: &'a [u8], offset: usize) -> Option<(u8, &'a [u8], usize)> {
    if offset + 2 > der.len() {
        return None;
    }
    let tag = der[offset];
    let mut pos = offset + 1;
    let first_len = der[pos] as usize;
    pos += 1;
    let len = if first_len & 0x80 == 0 {
        first_len
    } else {
        let bytes = first_len & 0x7f;
        if bytes == 0 || bytes > 4 || pos + bytes > der.len() {
            return None;
        }
        let mut len = 0usize;
        for byte in &der[pos..pos + bytes] {
            len = (len << 8) | (*byte as usize);
        }
        pos += bytes;
        len
    };
    if pos + len > der.len() {
        return None;
    }
    Some((tag, &der[pos..pos + len], pos + len))
}

/// Extract DNS and IP subject alternative names from a DER
/// certificate with a small bounded walk: find the SAN extension
/// (OID 2.5.29.17), then read its GeneralNames. dNSName is context
/// tag [2] (0x82), iPAddress is [7] (0x87). Returns an empty list
/// when the extension is absent or unparseable.
fn extract_sans(der: &[u8]) -> Vec<String> {
    // OID 2.5.29.17 encoded: 06 03 55 1D 11
    let oid: [u8; 5] = [0x06, 0x03, 0x55, 0x1d, 0x11];
    let mut sans: Vec<String> = Vec::new();
    let mut search_from = 0usize;
    while let Some(found) = der[search_from..]
        .windows(oid.len())
        .position(|window| window == oid)
    {
        let oid_pos = search_from + found;
        // The extension value is the next TLV after the OID; it is an
        // OCTET STRING wrapping a SEQUENCE of GeneralNames.
        let mut pos = oid_pos + oid.len();
        // Optional critical BOOLEAN may sit between OID and value.
        if let Some((tag, _value, next)) = der_tlv(der, pos) {
            if tag == 0x01 {
                pos = next;
            }
        }
        let Some((tag, value, _next)) = der_tlv(der, pos) else {
            break;
        };
        if tag != 0x04 {
            search_from = oid_pos + oid.len();
            continue;
        }
        // `value` is the DER of the SEQUENCE of GeneralNames.
        let Some((seq_tag, seq_value, _)) = der_tlv(value, 0) else {
            break;
        };
        if seq_tag != 0x30 {
            break;
        }
        let mut inner = 0usize;
        while let Some((tag, name_value, next)) = der_tlv(seq_value, inner) {
            match tag {
                0x82 => {
                    if let Ok(name) = std::str::from_utf8(name_value) {
                        sans.push(name.to_string());
                    }
                }
                0x87 => {
                    let ip = match name_value.len() {
                        4 => IpAddr::V4(Ipv4Addr::new(
                            name_value[0],
                            name_value[1],
                            name_value[2],
                            name_value[3],
                        ))
                        .to_string(),
                        16 => {
                            let mut octets = [0u8; 16];
                            octets.copy_from_slice(name_value);
                            IpAddr::V6(Ipv6Addr::from(octets)).to_string()
                        }
                        _ => String::new(),
                    };
                    if !ip.is_empty() {
                        sans.push(ip);
                    }
                }
                _ => {}
            }
            inner = next;
        }
        break;
    }
    sans.sort();
    sans.dedup();
    sans
}

fn parse_certificate_der(der: &[u8]) -> Value {
    let fingerprint = sharing::sha256_hex_bytes(der);
    let Ok(certificate) = x509_certificate::X509Certificate::from_der(der) else {
        return json!({
            "fingerprint_sha256": fingerprint,
            "parse_error": "Certificate DER could not be parsed",
        });
    };
    let tbs = &certificate.tbs_certificate;
    let not_before = match tbs.validity.not_before {
        x509_certificate::Time::UtcTime(time) => {
            DateTime::<Utc>::from(time.to_system_time()).to_rfc3339()
        }
        x509_certificate::Time::GeneralizedTime(time) => {
            DateTime::<Utc>::from(time.to_system_time()).to_rfc3339()
        }
    };
    let not_after = match tbs.validity.not_after {
        x509_certificate::Time::UtcTime(time) => {
            DateTime::<Utc>::from(time.to_system_time()).to_rfc3339()
        }
        x509_certificate::Time::GeneralizedTime(time) => {
            DateTime::<Utc>::from(time.to_system_time()).to_rfc3339()
        }
    };
    let serial = tbs.serial_number.to_string();
    json!({
        "subject": tbs.subject.to_string(),
        "issuer": tbs.issuer.to_string(),
        "serial": serial,
        "not_before": not_before,
        "not_after": not_after,
        "sans": extract_sans(der),
        "fingerprint_sha256": fingerprint,
    })
}

/// Blocking TLS handshake (same synchronous shape as the domains
/// module's probe): TCP connect, rustls handshake with an
/// accept-any verifier, then read the peer chain.
fn tls_handshake_blocking(
    addr: SocketAddr,
    server_name_text: &str,
    connect_timeout: Duration,
) -> UnifiedResult<(Vec<Vec<u8>>, Option<String>, Option<String>)> {
    let mut stream = std::net::TcpStream::connect_timeout(&addr, connect_timeout)
        .map_err(|error| UnifiedError::bad_request(format!("TCP connect failed: {error}")))?;
    let _ = stream.set_read_timeout(Some(connect_timeout));
    let _ = stream.set_write_timeout(Some(connect_timeout));
    let config = Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate))
            .with_no_client_auth(),
    );
    let server_name: ServerName = server_name_text
        .to_owned()
        .try_into()
        .map_err(|_| UnifiedError::bad_request("Invalid TLS server name"))?;
    let mut connection = ClientConnection::new(config, server_name)
        .map_err(|error| UnifiedError::internal(format!("Cannot start TLS handshake: {error}")))?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut stream)
            .map_err(|error| UnifiedError::bad_request(format!("TLS handshake failed: {error}")))?;
    }
    let cipher = connection
        .negotiated_cipher_suite()
        .map(|suite| format!("{:?}", suite.suite()));
    let certs: Vec<Vec<u8>> = connection
        .peer_certificates()
        .iter()
        .map(|cert| cert.as_ref().to_vec())
        .collect();
    // rustls 0.23 exposes the negotiated version through
    // `connection.protocol_version()`; fall back to the cipher suite
    // string when it is unavailable.
    let version = format!("{:?}", connection.protocol_version());
    Ok((certs, Some(version), cipher))
}

async fn tool_ssl_check(
    ctx: &AppContext,
    target: &str,
    port: u16,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let domain = validate_domain(target)?;
    let connect_timeout = clamp_connect_timeout(Some(timeout_secs));
    let addrs = resolve_and_guard(&domain, allow_private).await?;
    // Reuse the certificates module's live data first for domains
    // the server already manages: no network probe is needed then.
    let mut managed: Option<Value> = None;
    if let Some(storage) = ctx.tls() {
        for info in storage.list_certificates() {
            if info.domain.eq_ignore_ascii_case(&domain) {
                managed = Some(json!({
                    "domain": info.domain,
                    "subject": info.subject,
                    "issuer": info.issuer,
                    "sans": info.sans,
                    "not_before": info.not_before,
                    "not_after": info.not_after,
                    "days_remaining": info.days_remaining,
                    "fingerprint_sha256": info.fingerprint_sha256,
                    "self_signed": info.self_signed,
                    "chain_length": info.chain_length,
                    "chain_complete": info.chain_complete,
                    "source": "certificates module (managed domain)",
                }));
                break;
            }
        }
    }
    let addr = SocketAddr::new(addrs[0], port);
    let tls_domain = domain.clone();
    let (certs, version, cipher) = tokio::task::spawn_blocking(move || {
        tls_handshake_blocking(addr, &tls_domain, connect_timeout)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("TLS probe task failed: {error}")))?;
    let chain: Vec<Value> = certs.iter().map(|der| parse_certificate_der(der)).collect();
    let mut notes: Vec<String> = Vec::new();
    let mut days_remaining: Option<i64> = None;
    let mut expired = false;
    if let Some(leaf) = chain.first() {
        if let Some(not_after) = leaf.get("not_after").and_then(|v| v.as_str()) {
            if let Ok(expiry) = DateTime::parse_from_rfc3339(not_after) {
                let days = (expiry.with_timezone(&Utc) - Utc::now()).num_days();
                days_remaining = Some(days);
                if days < 0 {
                    expired = true;
                    notes.push("The leaf certificate has expired.".to_string());
                } else if days <= 30 {
                    notes.push(format!("The leaf certificate expires in {days} day(s)."));
                }
            }
        }
        if let (Some(subject), Some(issuer)) = (
            leaf.get("subject").and_then(|v| v.as_str()),
            leaf.get("issuer").and_then(|v| v.as_str()),
        ) {
            if subject == issuer {
                notes.push("The leaf certificate is self-signed (subject equals issuer).".to_string());
            }
        }
        let sans: Vec<String> = leaf
            .get("sans")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !sans.is_empty()
            && !sans.iter().any(|san| {
                san.eq_ignore_ascii_case(&domain)
                    || (san.starts_with("*.") && domain.ends_with(&san[1..]))
            })
        {
            notes.push("The domain is not listed in the leaf certificate SANs.".to_string());
        }
    } else {
        notes.push("The server presented no certificate chain.".to_string());
    }
    if chain.len() > 1 {
        notes.push(format!("Chain length: {} certificates.", chain.len()));
    }
    notes.push(
        "Chain trust was NOT validated against a root store by this probe (accept-any verifier); the notes above are structural checks only."
            .to_string(),
    );
    let outcome = if expired || chain.is_empty() {
        "failed"
    } else {
        "success"
    };
    Ok(ToolResult {
        tool: "ssl_check".to_string(),
        target: format!("{domain}:{port}"),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("rustls handshake".to_string()),
        result: json!({
            "domain": domain,
            "port": port,
            "protocol_version": version,
            "cipher_suite": cipher,
            "chain": chain,
            "days_remaining": days_remaining,
            "expired": expired,
            "managed_certificate": managed,
        }),
        notes,
    })
}

// ---------------------------------------------------------------------------
// WHOIS (TCP port 43, one referral follow).
// ---------------------------------------------------------------------------

async fn whois_query(server: &str, query: &str, timeout_secs: u64) -> UnifiedResult<String> {
    let addrs = tokio::net::lookup_host((server, 43))
        .await
        .map_err(|error| UnifiedError::bad_request(format!("Cannot resolve WHOIS server \"{server}\": {error}")))?;
    let mut last_error: Option<String> = None;
    for addr in addrs {
        match timeout(
            Duration::from_secs(timeout_secs),
            TokioTcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(mut stream)) => {
                let _ = stream.write_all(format!("{query}\r\n").as_bytes()).await;
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let read_result = timeout(Duration::from_secs(timeout_secs), async {
                    loop {
                        match stream.read(&mut chunk).await {
                            Ok(0) => break,
                            Ok(read) => {
                                buf.extend_from_slice(&chunk[..read]);
                                if buf.len() > 65536 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                })
                .await;
                if read_result.is_err() {
                    return Err(UnifiedError::bad_request(
                        "WHOIS query timed out while reading the response",
                    ));
                }
                return Ok(String::from_utf8_lossy(&buf).to_string());
            }
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("connect timed out".to_string()),
        }
    }
    Err(UnifiedError::bad_request(format!(
        "Cannot reach WHOIS server \"{server}\": {}",
        last_error.unwrap_or_default()
    )))
}

fn whois_parse(raw: &str) -> HashMap<String, String> {
    let mut fields: HashMap<String, String> = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            if !value.is_empty()
                && matches!(
                    key.as_str(),
                    "domain name"
                        | "registrar"
                        | "creation date"
                        | "updated date"
                        | "registry expiry date"
                        | "expiry date"
                        | "name server"
                        | "status"
                        | "registrant organization"
                        | "orgname"
                        | "netname"
                        | "country"
                        | "cidr"
                        | "inetnum"
                        | "whois server"
                )
            {
                fields
                    .entry(key)
                    .and_modify(|existing| {
                        if !existing.contains(&value) {
                            existing.push_str("; ");
                            existing.push_str(&value);
                        }
                    })
                    .or_insert(value);
            }
        }
    }
    fields
}

async fn tool_whois(target: &str, timeout_secs: u64) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = target.trim().to_string();
    if target.is_empty() || target.len() > 253 {
        return Err(UnifiedError::bad_request("WHOIS target must not be empty"));
    }
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    // IP targets go to the IANA / RIR referral chain; domains go to
    // the IANA WHOIS server first, then follow one referral.
    let is_ip = target.parse::<IpAddr>().is_ok();
    let mut server = "whois.iana.org".to_string();
    let mut raw = whois_query(&server, &target, timeout_secs).await?;
    if !is_ip {
        let fields = whois_parse(&raw);
        if let Some(referral) = fields.get("whois server") {
            let referral = referral
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !referral.is_empty()
                && referral != server
                && valid_hostname_or_ip(&referral)
            {
                server = referral;
                raw = whois_query(&server, &target, timeout_secs).await?;
            }
        }
    } else {
        // For IPs, follow the referral once as well (IANA points at
        // the responsible RIR).
        let fields = whois_parse(&raw);
        if let Some(referral) = fields.get("whois server") {
            let referral = referral
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !referral.is_empty() && referral != server && valid_hostname_or_ip(&referral) {
                server = referral;
                raw = whois_query(&server, &target, timeout_secs).await?;
            }
        }
    }
    let fields = whois_parse(&raw);
    let truncated: String = raw.chars().take(MAX_WHOIS_TEXT_CHARS).collect();
    Ok(ToolResult {
        tool: "whois".to_string(),
        target,
        outcome: "success".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some(format!("tcp/43 via {server}")),
        result: json!({
            "server": server,
            "fields": fields,
            "raw": truncated,
            "raw_truncated": raw.chars().count() > MAX_WHOIS_TEXT_CHARS,
        }),
        notes: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// HTTP header / status check with manual redirect following so every
// hop is re-checked by the SSRF guard.
// ---------------------------------------------------------------------------

async fn tool_http_check(
    url_text: &str,
    method: Option<String>,
    expected_status: Option<u16>,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let method = method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
    if !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
        return Err(UnifiedError::bad_request(
            "HTTP check supports GET, HEAD, and OPTIONS only",
        ));
    }
    let total_timeout = clamp_total_timeout(Some(timeout_secs));
    let (mut current_url, _) = guard_url(url_text, allow_private).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(total_timeout)
        .user_agent("Unified-Network-Tools/1.0")
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let mut redirects: Vec<String> = Vec::new();
    let mut last_status: u16 = 0;
    let mut last_headers: HashMap<String, String> = HashMap::new();
    let mut last_body_size: u64 = 0;
    let dns_ms: Option<f64> = None;
    let connect_ms: Option<f64> = None;
    let mut ttfb_ms: Option<f64> = None;
    for _hop in 0..=MAX_HTTP_REDIRECTS {
        let hop_start = Instant::now();
        let request = client
            .request(
                method.parse().map_err(|_| {
                    UnifiedError::bad_request("Invalid HTTP method")
                })?,
                current_url.clone(),
            )
            .build()
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build request: {error}")))?;
        let response = timeout(total_timeout, client.execute(request))
            .await
            .map_err(|_| UnifiedError::bad_request("HTTP check timed out"))?
            .map_err(|error| UnifiedError::bad_request(format!("HTTP request failed: {error}")))?;
        ttfb_ms = Some(hop_start.elapsed().as_secs_f64() * 1000.0);
        last_status = response.status().as_u16();
        last_headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect();
        let is_redirect = (300..400).contains(&last_status);
        let next_location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Read the body only to measure its size; it is never stored.
        let body_bytes = response
            .bytes()
            .await
            .map_err(|error| UnifiedError::bad_request(format!("Cannot read response body: {error}")))?;
        last_body_size = body_bytes.len() as u64;
        if is_redirect {
            let Some(location) = next_location else {
                break;
            };
            let next_url = reqwest::Url::parse(&current_url)
                .and_then(|base| base.join(&location))
                .map_err(|_| {
                    UnifiedError::bad_request(format!("Invalid redirect target: \"{location}\""))
                })?;
            // Re-check the redirect target with the same SSRF guard.
            let (checked, _) = guard_url(next_url.as_str(), allow_private).await?;
            redirects.push(checked.clone());
            current_url = checked;
            continue;
        }
        break;
    }
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    let outcome = match expected_status {
        Some(expected) if last_status != expected => "failed",
        Some(_) => "success",
        None if (200..400).contains(&last_status) => "success",
        None => "failed",
    };
    // DNS / connect timings are not separable through reqwest's
    // public API; report them as not measured rather than inventing
    // numbers.
    Ok(ToolResult {
        tool: "http_check".to_string(),
        target: url_text.to_string(),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("reqwest, manual redirects".to_string()),
        result: json!({
            "method": method,
            "status": last_status,
            "expected_status": expected_status,
            "headers": last_headers,
            "final_url": current_url,
            "redirects": redirects,
            "redirect_count": redirects.len(),
            "body_size_bytes": last_body_size,
            "timings_ms": {
                "dns": dns_ms,
                "connect": connect_ms,
                "ttfb": ttfb_ms,
                "total": total_ms,
            },
        }),
        notes: vec![
            "DNS and connect timings are reported as null: reqwest does not expose per-phase timings, and this module does not invent them.".to_string(),
            "The response body is measured for size only and is never stored.".to_string(),
        ],
    })
}

// ---------------------------------------------------------------------------
// IP info + Geo (delegates to the geo module) and reverse DNS.
// ---------------------------------------------------------------------------

async fn tool_ip_info(ctx: &AppContext, target: &str) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = target.trim().to_string();
    let ip: IpAddr = target
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid IP address: \"{target}\"")))?;
    let geo_info: GeoInfo = geo::lookup_geo(ctx, &target)?;
    // Reverse DNS through the shared DNS verifier (PTR records).
    let verifier = DnsVerifier::new();
    let ptr_names: Vec<String> = verifier.lookup_ptr(ip).await.unwrap_or_default();
    Ok(ToolResult {
        tool: "ip_info".to_string(),
        target,
        outcome: "success".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("geo module lookup + dns verifier ptr".to_string()),
        result: json!({
            "geo": geo_info,
            "reverse_dns": ptr_names,
        }),
        notes: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Subnet / CIDR calculator (pure computation, IPv4 + IPv6).
// ---------------------------------------------------------------------------

fn tool_subnet(cidr_text: &str, contains: Option<&str>) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let (addr_text, prefix_text) = cidr_text
        .split_once('/')
        .ok_or_else(|| UnifiedError::bad_request("CIDR must look like 192.168.0.0/24"))?;
    let prefix: u32 = prefix_text
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request("CIDR prefix must be a number"))?;
    let addr: IpAddr = addr_text
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid IP address in CIDR: \"{addr_text}\"")))?;
    let (result, contains_result) = match addr {
        IpAddr::V4(v4) => {
            if prefix > 32 {
                return Err(UnifiedError::bad_request("IPv4 prefix must be 0-32"));
            }
            let ip_u32 = u32::from(v4);
            let mask: u32 = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            let network = ip_u32 & mask;
            let broadcast = network | !mask;
            let host_bits = 32 - prefix;
            let usable_count: u128 = if host_bits == 0 {
                1
            } else if host_bits == 1 {
                2
            } else {
                (1u128 << host_bits) - 2
            };
            let (first, last) = if prefix >= 31 {
                (network, broadcast)
            } else {
                (network + 1, broadcast - 1)
            };
            let contains_check = match contains {
                Some(candidate) => {
                    let candidate_ip: IpAddr = candidate.trim().parse().map_err(|_| {
                        UnifiedError::bad_request(format!("Invalid contains address: \"{candidate}\""))
                    })?;
                    match candidate_ip {
                        IpAddr::V4(candidate_v4) => {
                            let candidate_u32 = u32::from(candidate_v4);
                            Some((candidate_u32 & mask) == network)
                        }
                        IpAddr::V6(_) => Some(false),
                    }
                }
                None => None,
            };
            (
                json!({
                    "family": "ipv4",
                    "input": cidr_text,
                    "network": Ipv4Addr::from(network).to_string(),
                    "broadcast": Ipv4Addr::from(broadcast).to_string(),
                    "netmask": Ipv4Addr::from(mask).to_string(),
                    "wildcard_mask": Ipv4Addr::from(!mask).to_string(),
                    "first_usable": Ipv4Addr::from(first).to_string(),
                    "last_usable": Ipv4Addr::from(last).to_string(),
                    "prefix": prefix,
                    "total_addresses": 1u128 << host_bits,
                    "usable_hosts": usable_count,
                    "cidr_range": format!("{} - {}", Ipv4Addr::from(network), Ipv4Addr::from(broadcast)),
                }),
                contains_check,
            )
        }
        IpAddr::V6(v6) => {
            if prefix > 128 {
                return Err(UnifiedError::bad_request("IPv6 prefix must be 0-128"));
            }
            let ip_u128 = u128::from(v6);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            let network = ip_u128 & mask;
            let last = network | !mask;
            let host_bits = 128 - prefix;
            let total: u128 = if host_bits >= 128 {
                u128::MAX
            } else {
                1u128 << host_bits
            };
            let contains_check = match contains {
                Some(candidate) => {
                    let candidate_ip: IpAddr = candidate.trim().parse().map_err(|_| {
                        UnifiedError::bad_request(format!("Invalid contains address: \"{candidate}\""))
                    })?;
                    match candidate_ip {
                        IpAddr::V6(candidate_v6) => {
                            Some((u128::from(candidate_v6) & mask) == network)
                        }
                        IpAddr::V4(_) => Some(false),
                    }
                }
                None => None,
            };
            (
                json!({
                    "family": "ipv6",
                    "input": cidr_text,
                    "network": Ipv6Addr::from(network).to_string(),
                    "broadcast": null,
                    "netmask": Ipv6Addr::from(mask).to_string(),
                    "wildcard_mask": Ipv6Addr::from(!mask).to_string(),
                    "first_usable": Ipv6Addr::from(network).to_string(),
                    "last_usable": Ipv6Addr::from(last).to_string(),
                    "prefix": prefix,
                    "total_addresses": total.to_string(),
                    "usable_hosts": total.to_string(),
                    "cidr_range": format!("{} - {}", Ipv6Addr::from(network), Ipv6Addr::from(last)),
                }),
                contains_check,
            )
        }
    };
    let mut result = result;
    if let Some(contains_check) = contains_result {
        result["contains"] = json!(contains_check);
        result["contains_address"] = json!(contains);
    }
    Ok(ToolResult {
        tool: "subnet".to_string(),
        target: cidr_text.to_string(),
        outcome: "success".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("pure computation".to_string()),
        result,
        notes: vec!["IPv6 has no broadcast address; first/last usable are the network range ends.".to_string()],
    })
}

// ---------------------------------------------------------------------------
// Webhook tester.
// ---------------------------------------------------------------------------

async fn tool_webhook_test(
    url_text: &str,
    payload: Value,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let total_timeout = clamp_total_timeout(Some(timeout_secs));
    let (url, _) = guard_url(url_text, allow_private).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(total_timeout)
        .user_agent("Unified-Network-Tools/1.0")
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let response = timeout(
        total_timeout,
        client.post(url.clone()).json(&payload).send(),
    )
    .await
    .map_err(|_| UnifiedError::bad_request("Webhook test timed out"))?
    .map_err(|error| UnifiedError::bad_request(format!("Webhook request failed: {error}")))?;
    let status = response.status().as_u16();
    let headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    // Redirects are NOT followed for webhook tests: a redirect to a
    // private target must never be chased silently.
    if (300..400).contains(&status) {
        return Ok(ToolResult {
            tool: "webhook_test".to_string(),
            target: url_text.to_string(),
            outcome: "failed".to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            method: Some("reqwest post, no redirect following".to_string()),
            result: json!({
                "url": url,
                "status": status,
                "headers": headers,
                "body": null,
                "body_truncated": false,
                "redirect_not_followed": true,
            }),
            notes: vec!["The endpoint redirected; redirects are never followed by the webhook tester.".to_string()],
        });
    }
    let body_bytes = response
        .bytes()
        .await
        .map_err(|error| UnifiedError::bad_request(format!("Cannot read webhook response: {error}")))?;
    let truncated = body_bytes.len() > MAX_WEBHOOK_BODY_BYTES;
    let body_text = String::from_utf8_lossy(&body_bytes[..body_bytes.len().min(MAX_WEBHOOK_BODY_BYTES)]).to_string();
    let outcome = if (200..300).contains(&status) {
        "success"
    } else {
        "failed"
    };
    Ok(ToolResult {
        tool: "webhook_test".to_string(),
        target: url_text.to_string(),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("reqwest post, no redirect following".to_string()),
        result: json!({
            "url": url,
            "status": status,
            "headers": headers,
            "body": body_text,
            "body_truncated": truncated,
            "redirect_not_followed": false,
        }),
        notes: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// DNS blacklist check.
// ---------------------------------------------------------------------------

async fn tool_blacklist(
    target: &str,
    zones: Option<Vec<String>>,
    timeout_secs: u64,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let target = validate_target(target)?;
    let timeout_secs = timeout_secs.clamp(1, MAX_TOTAL_TIMEOUT_SECS);
    let addrs = resolve_and_guard(&target, false).await?;
    let ip = addrs[0];
    let IpAddr::V4(v4) = ip else {
        return Err(UnifiedError::bad_request(
            "DNS blacklist checks only support IPv4 targets",
        ));
    };
    let octets = v4.octets();
    let reversed = format!("{}.{}.{}.{}", octets[3], octets[2], octets[1], octets[0]);
    let zones: Vec<String> = match zones {
        Some(list) if !list.is_empty() => {
            let mut cleaned: Vec<String> = Vec::new();
            for zone in list {
                let zone = zone.trim().trim_end_matches('.').to_lowercase();
                if zone.is_empty() || !valid_hostname_or_ip(&zone) {
                    return Err(UnifiedError::bad_request(format!(
                        "Invalid DNSBL zone: \"{zone}\""
                    )));
                }
                cleaned.push(zone);
            }
            cleaned
        }
        _ => DEFAULT_DNSBL_ZONES.iter().map(|z| z.to_string()).collect(),
    };
    let verifier = DnsVerifier::new();
    let mut results: Vec<Value> = Vec::new();
    let mut listed_count = 0usize;
    for zone in &zones {
        let query = format!("{reversed}.{zone}");
        let listed = match timeout(
            Duration::from_secs(timeout_secs),
            verifier.lookup_a(&query),
        )
        .await
        {
            Ok(Ok(records)) => records
                .iter()
                .any(|ip| matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 127)),
            _ => false,
        };
        if listed {
            listed_count += 1;
        }
        results.push(json!({
            "zone": zone,
            "listed": listed,
            "query": query,
        }));
    }
    Ok(ToolResult {
        tool: "blacklist".to_string(),
        target,
        outcome: if listed_count == 0 {
            "success".to_string()
        } else {
            "failed".to_string()
        },
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("dnsbl a-record queries".to_string()),
        result: json!({
            "ip": ip.to_string(),
            "zones_checked": zones.len(),
            "listed_count": listed_count,
            "results": results,
        }),
        notes: vec!["A zone that does not answer is reported as not-listed, not as an error; absence of an answer is not proof of absence from the list.".to_string()],
    })
}

// ---------------------------------------------------------------------------
// Uptime: one-off check plus monitor evaluation.
// ---------------------------------------------------------------------------

async fn run_uptime_probe(
    url_text: &str,
    expected_status: i32,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<(u16, u64, String)> {
    let total_timeout = clamp_total_timeout(Some(timeout_secs));
    let (url, _) = guard_url(url_text, allow_private).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(total_timeout)
        .user_agent("Unified-Network-Tools/1.0")
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let started = Instant::now();
    let response = timeout(total_timeout, client.get(url).send())
        .await
        .map_err(|_| UnifiedError::bad_request("Uptime probe timed out"))?
        .map_err(|error| UnifiedError::bad_request(format!("Uptime probe failed: {error}")))?;
    let status = response.status().as_u16();
    let latency_ms = started.elapsed().as_millis() as u64;
    let outcome = if status == expected_status as u16 {
        "up"
    } else {
        "down"
    };
    Ok((status, latency_ms, outcome.to_string()))
}

/// Evaluate one monitor: probe it, store the event, update the
/// failure counter, and emit `network_tools.uptime.down` plus a
/// notification when the failure threshold is crossed. Returns the
/// monitor's new status.
async fn evaluate_monitor(
    ctx: &AppContext,
    monitor_id: Uuid,
    actor: &str,
    allow_private: bool,
) -> UnifiedResult<Value> {
    let Some(mut monitor) = uptime_monitors_store()
        .get(&monitor_id)
        .map(|entry| entry.value().clone())
    else {
        return Err(UnifiedError::not_found(format!(
            "Uptime monitor {monitor_id} not found"
        )));
    };
    let probe = run_uptime_probe(
        &monitor.url,
        monitor.expected_status,
        DEFAULT_TOTAL_TIMEOUT_SECS,
        allow_private,
    )
    .await;
    let (http_status, latency_ms, status) = match &probe {
        Ok((status, latency, outcome)) => (Some(*status as i32), Some(*latency as i64), outcome.clone()),
        Err(error) => (None, None, format!("down: {}", error.message)),
    };
    let is_up = status == "up";
    if is_up {
        monitor.consecutive_failures = 0;
        monitor.last_status = Some("up".to_string());
    } else {
        monitor.consecutive_failures += 1;
        monitor.last_status = Some("down".to_string());
    }
    monitor.last_checked_at = Some(Utc::now());
    monitor.updated_at = Utc::now();
    let event = NetworkToolUptimeEvent {
        id: Uuid::new_v4(),
        monitor_id,
        status: if is_up { "up".to_string() } else { "down".to_string() },
        http_status,
        latency_ms,
        detail: json!({
            "url": monitor.url,
            "expected_status": monitor.expected_status,
            "consecutive_failures": monitor.consecutive_failures,
            "error": match &probe {
                Ok(_) => Value::Null,
                Err(error) => json!(error.message.clone()),
            },
        }),
        created_at: Utc::now(),
    };
    uptime_events_store().insert(event.id, event.clone());
    uptime_monitors_store().insert(monitor.id, monitor.clone());
    if let Some(pool) = db_pool(ctx) {
        let _ = db_insert_uptime_event(&pool, &event);
        let _ = db_upsert_monitor(&pool, &monitor);
    }
    if is_up {
        ctx.hooks.emit(
            network_tool_events::UPTIME_UP,
            json!({
                "monitor_id": monitor.id,
                "url": monitor.url,
                "http_status": http_status,
                "latency_ms": latency_ms,
            }),
        );
    } else {
        ctx.hooks.emit(
            network_tool_events::UPTIME_DOWN,
            json!({
                "monitor_id": monitor.id,
                "url": monitor.url,
                "http_status": http_status,
                "consecutive_failures": monitor.consecutive_failures,
                "failure_threshold": monitor.failure_threshold,
            }),
        );
        if monitor.consecutive_failures >= monitor.failure_threshold {
            notify(
                ctx,
                actor,
                format!("Uptime monitor \"{}\" is down", monitor.name),
                format!(
                    "The monitor for {} has failed {} time(s) in a row (threshold {}); last HTTP status {}.",
                    monitor.url,
                    monitor.consecutive_failures,
                    monitor.failure_threshold,
                    http_status.map(|s| s.to_string()).unwrap_or_else(|| "unreachable".to_string())
                ),
                "network_tools.uptime.down",
            );
        }
    }
    Ok(json!({
        "monitor_id": monitor.id,
        "status": monitor.last_status,
        "http_status": http_status,
        "latency_ms": latency_ms,
        "consecutive_failures": monitor.consecutive_failures,
    }))
}

// ---------------------------------------------------------------------------
// Certificate expiry watch evaluation.
// ---------------------------------------------------------------------------

/// Check every enabled watch: prefer the certificates module's live
/// data for managed domains, otherwise probe live with the SSL/TLS
/// checker. Alert at 30/14/7/1 days (and once on expiry) through
/// `network_tools.cert.expiring` plus a direct `notification.requested`.
/// Returns a summary of what was checked.
async fn check_cert_watches(ctx: &AppContext, actor: &str) -> UnifiedResult<Value> {
    let watches: Vec<NetworkToolCertWatch> = cert_watches_store()
        .iter()
        .filter(|entry| entry.value().enabled)
        .map(|entry| entry.value().clone())
        .collect();
    let mut checked = 0u64;
    let mut expiring: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for mut watch in watches {
        // Managed-domain data first (certificates module storage).
        let mut days: Option<i64> = None;
        if let Some(storage) = ctx.tls() {
            for info in storage.list_certificates() {
                if info.domain.eq_ignore_ascii_case(&watch.domain) {
                    days = Some(info.days_remaining);
                    break;
                }
            }
        }
        // Otherwise probe live.
        if days.is_none() {
            match tool_ssl_check(ctx, &watch.domain, 443, DEFAULT_TOTAL_TIMEOUT_SECS, false).await
            {
                Ok(result) => {
                    days = result
                        .result
                        .get("days_remaining")
                        .and_then(|v| v.as_i64());
                }
                Err(_) => {
                    continue;
                }
            }
        }
        let Some(days) = days else {
            continue;
        };
        checked += 1;
        watch.last_days_remaining = Some(days as i32);
        watch.last_checked_at = Some(Utc::now());
        let mut crossed: Vec<i64> = Vec::new();
        if days < 0 {
            if !watch.alerted_thresholds.contains(&-1) {
                crossed.push(-1);
            }
            expired.push(json!({
                "domain": watch.domain,
                "days_remaining": days,
            }));
        } else {
            for threshold in CERT_ALERT_THRESHOLDS_DAYS {
                if days <= threshold && !watch.alerted_thresholds.contains(&(threshold as i32)) {
                    crossed.push(threshold);
                }
            }
            if !crossed.is_empty() {
                expiring.push(json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": crossed,
                }));
            }
        }
        for threshold in &crossed {
            watch.alerted_thresholds.push(*threshold as i32);
            ctx.hooks.emit(
                network_tool_events::CERT_EXPIRING,
                json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": threshold,
                    "expired": *threshold == -1,
                }),
            );
            if *threshold == -1 {
                notify(
                    ctx,
                    actor,
                    format!("Certificate for {} has expired", watch.domain),
                    format!(
                        "The certificate for {} expired {} day(s) ago; renewal is due immediately.",
                        watch.domain,
                        -days
                    ),
                    "network_tools.cert.expiring",
                );
            } else {
                notify(
                    ctx,
                    actor,
                    format!("Certificate for {} expires in {days} day(s)", watch.domain),
                    format!(
                        "The certificate for {} expires in {days} day(s); renewal is due within the configured window.",
                        watch.domain
                    ),
                    "network_tools.cert.expiring",
                );
            }
        }
        watch.updated_at = Utc::now();
        cert_watches_store().insert(watch.id, watch.clone());
        if let Some(pool) = db_pool(ctx) {
            let _ = db_upsert_cert_watch(&pool, &watch);
        }
    }
    Ok(json!({
        "checked": checked,
        "expiring": expiring,
        "expired": expired,
    }))
}

/// Synchronous alert scan used by the `network_tools.cert.check_requested`
/// hook handler: re-evaluates thresholds from the last known expiry
/// (managed-domain storage or the stored `last_days_remaining`)
/// without any network probe, so it can run inside a sync handler.
fn check_cert_watches_sync(ctx: &AppContext) -> Value {
    let watches: Vec<NetworkToolCertWatch> = cert_watches_store()
        .iter()
        .filter(|entry| entry.value().enabled)
        .map(|entry| entry.value().clone())
        .collect();
    let mut checked = 0u64;
    let mut expiring: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for mut watch in watches {
        let mut days: Option<i64> = watch.last_days_remaining.map(i64::from);
        if let Some(storage) = ctx.tls() {
            for info in storage.list_certificates() {
                if info.domain.eq_ignore_ascii_case(&watch.domain) {
                    days = Some(info.days_remaining);
                    break;
                }
            }
        }
        let Some(days) = days else {
            continue;
        };
        checked += 1;
        let mut crossed: Vec<i64> = Vec::new();
        if days < 0 {
            if !watch.alerted_thresholds.contains(&-1) {
                crossed.push(-1);
            }
            expired.push(json!({ "domain": watch.domain, "days_remaining": days }));
        } else {
            for threshold in CERT_ALERT_THRESHOLDS_DAYS {
                if days <= threshold && !watch.alerted_thresholds.contains(&(threshold as i32)) {
                    crossed.push(threshold);
                }
            }
            if !crossed.is_empty() {
                expiring.push(json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": crossed,
                }));
            }
        }
        for threshold in &crossed {
            watch.alerted_thresholds.push(*threshold as i32);
            ctx.hooks.emit(
                network_tool_events::CERT_EXPIRING,
                json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": threshold,
                    "expired": *threshold == -1,
                }),
            );
            notify(
                ctx,
                "system",
                if *threshold == -1 {
                    format!("Certificate for {} has expired", watch.domain)
                } else {
                    format!("Certificate for {} expires in {days} day(s)", watch.domain)
                },
                format!(
                    "The certificate for {} expires in {days} day(s); renewal is due within the configured window.",
                    watch.domain
                ),
                "network_tools.cert.expiring",
            );
        }
        watch.updated_at = Utc::now();
        cert_watches_store().insert(watch.id, watch.clone());
    }
    json!({
        "checked": checked,
        "expiring": expiring,
        "expired": expired,
        "note": "Sync scan from last known expiry only; use POST /api/v1/network-tools/cert-watches/check for a live probe.",
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------

/// Run a tool, record the run (success or failure), and answer with
/// the standard envelope. Failures are recorded too, with the error
/// message as the summary - never with a response body.
async fn execute_tool(
    ctx: web::Data<Arc<AppContext>>,
    started: Instant,
    actor: &str,
    company_id: Option<&str>,
    tool: &str,
    target: &str,
    outcome: UnifiedResult<ToolResult>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match outcome {
        Ok(mut result) => {
            result.duration_ms = started.elapsed().as_millis() as u64;
            let summary = json!({
                "outcome": result.outcome,
                "duration_ms": result.duration_ms,
                "method": result.method,
            });
            let run_id = record_run(
                &ctx,
                actor,
                company_id,
                tool,
                target,
                started,
                &result.outcome,
                summary,
            );
            let mut payload = json!(result);
            payload["run_id"] = json!(run_id);
            json::respond(Ok::<Value, UnifiedError>(payload), debug)
        }
        Err(error) => {
            let message = error.message.clone();
            record_run(
                &ctx,
                actor,
                company_id,
                tool,
                target,
                started,
                "failed",
                json!({ "error": message }),
            );
            json::respond(Err::<Value, UnifiedError>(error), debug)
        }
    }
}

fn tool_actor(query: &PaginationParams) -> (String, Option<String>, Option<String>) {
    let (viewer_id, viewer_role, company_id) = viewer_of(query);
    (
        viewer_id.unwrap_or_else(|| "anonymous".to_string()),
        viewer_role,
        company_id,
    )
}

async fn list_tools(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (_, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        check_rate_limit(
            params
                .filters
                .get("viewer_id")
                .map(|v| v.as_str())
                .unwrap_or("anonymous"),
            "tools:list",
            RATE_LIMIT_READS_PER_MINUTE,
        )?;
        Ok(json!({
            "tools": [
                { "id": "dns", "name": "DNS lookup", "method": "POST", "path": "/api/v1/network-tools/dns", "record_types": ["A", "AAAA", "MX", "TXT", "CNAME", "NS", "SOA"] },
                { "id": "ping", "name": "Ping", "method": "POST", "path": "/api/v1/network-tools/ping" },
                { "id": "traceroute", "name": "Traceroute", "method": "POST", "path": "/api/v1/network-tools/traceroute" },
                { "id": "port_check", "name": "Port check", "method": "POST", "path": "/api/v1/network-tools/port-check" },
                { "id": "ssl_check", "name": "SSL/TLS checker", "method": "POST", "path": "/api/v1/network-tools/ssl-check" },
                { "id": "whois", "name": "WHOIS lookup", "method": "POST", "path": "/api/v1/network-tools/whois" },
                { "id": "http_check", "name": "HTTP header/status check", "method": "POST", "path": "/api/v1/network-tools/http-check" },
                { "id": "ip_info", "name": "IP info + Geo", "method": "POST", "path": "/api/v1/network-tools/ip-info" },
                { "id": "subnet", "name": "Subnet/CIDR calculator", "method": "POST", "path": "/api/v1/network-tools/subnet" },
                { "id": "webhook_test", "name": "Webhook tester", "method": "POST", "path": "/api/v1/network-tools/webhook-test" },
                { "id": "uptime_check", "name": "Uptime check (one-off)", "method": "POST", "path": "/api/v1/network-tools/uptime-check" },
                { "id": "blacklist", "name": "DNS blacklist check", "method": "POST", "path": "/api/v1/network-tools/blacklist" },
            ],
            "limits": {
                "default_connect_timeout_secs": DEFAULT_CONNECT_TIMEOUT_SECS,
                "default_total_timeout_secs": DEFAULT_TOTAL_TIMEOUT_SECS,
                "max_total_timeout_secs": MAX_TOTAL_TIMEOUT_SECS,
                "max_ping_count": MAX_PING_COUNT,
                "max_traceroute_hops": MAX_TRACEROUTE_HOPS,
                "rate_limit_runs_per_minute": RATE_LIMIT_RUNS_PER_MINUTE,
                "rate_limit_reads_per_minute": RATE_LIMIT_READS_PER_MINUTE,
            },
            "dnsbl_default_zones": DEFAULT_DNSBL_ZONES,
        }))
    })();
    json::respond(result, debug)
}

async fn dns_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<DnsLookupRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "dns", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_dns_lookup(&input.target, &input.record_type)
        .await
        .map(|value| ToolResult {
            tool: "dns".to_string(),
            target: input.target.clone(),
            outcome: "success".to_string(),
            duration_ms: 0,
            method: Some("dns verifier".to_string()),
            result: value,
            notes: Vec::new(),
        });
    execute_tool(ctx, started, &actor, company_id.as_deref(), "dns", &target, result).await
}

async fn ping_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<PingRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "ping", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_ping(
        &input.target,
        input.count,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(ctx, started, &actor, company_id.as_deref(), "ping", &target, result).await
}

async fn traceroute_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<TracerouteRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "traceroute", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_traceroute(
        &input.target,
        input.max_hops,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "traceroute",
        &target,
        result,
    )
    .await
}

async fn port_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<PortCheckRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "port_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = format!("{}:{}", input.target, input.port);
    let result = tool_port_check(
        &input.target,
        input.port,
        input.grab_banner,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "port_check",
        &target,
        result,
    )
    .await
}

async fn ssl_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<ToolTargetRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "ssl_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_ssl_check(
        &ctx,
        &input.target,
        443,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "ssl_check",
        &target,
        result,
    )
    .await
}

async fn whois_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<WhoisRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "whois", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_whois(
        &input.target,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
    )
    .await;
    execute_tool(ctx, started, &actor, company_id.as_deref(), "whois", &target, result).await
}

async fn http_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<HttpCheckRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "http_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.url.clone();
    let result = tool_http_check(
        &input.url,
        input.method,
        input.expected_status,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "http_check",
        &target,
        result,
    )
    .await
}

async fn ip_info_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<ToolTargetRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "ip_info", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_ip_info(&ctx, &input.target).await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "ip_info",
        &target,
        result,
    )
    .await
}

async fn subnet_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<SubnetRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "subnet", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.cidr.clone();
    let result = tool_subnet(&input.cidr, input.contains.as_deref());
    execute_tool(ctx, started, &actor, company_id.as_deref(), "subnet", &target, result).await
}

async fn webhook_test_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<WebhookTestRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "webhook_test", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.url.clone();
    let result = tool_webhook_test(
        &input.url,
        input.payload,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "webhook_test",
        &target,
        result,
    )
    .await
}

async fn blacklist_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<BlacklistRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "blacklist", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_blacklist(
        &input.target,
        input.zones,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "blacklist",
        &target,
        result,
    )
    .await
}

async fn uptime_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<HttpCheckRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "uptime_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.url.clone();
    let expected = input.expected_status.unwrap_or(200) as i32;
    let result = run_uptime_probe(
        &input.url,
        expected,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await
    .map(|(status, latency_ms, outcome)| ToolResult {
        tool: "uptime_check".to_string(),
        target: input.url.clone(),
        outcome,
        duration_ms: latency_ms,
        method: Some("single http probe, no redirect following".to_string()),
        result: json!({
            "url": input.url,
            "status": status,
            "expected_status": expected,
            "latency_ms": latency_ms,
        }),
        notes: Vec::new(),
    });
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "uptime_check",
        &target,
        result,
    )
    .await
}

// ---------------------------------------------------------------------------
// Run history.
// ---------------------------------------------------------------------------

async fn list_history(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        check_rate_limit(
            viewer_id.as_deref().unwrap_or("anonymous"),
            "history:list",
            RATE_LIMIT_READS_PER_MINUTE,
        )?;
        let mut items: Vec<NetworkToolRun> = match db_pool(&ctx) {
            Some(pool) => db_load_runs(&pool).unwrap_or_else(|_| {
                runs_store().iter().map(|entry| entry.value().clone()).collect()
            }),
            None => runs_store().iter().map(|entry| entry.value().clone()).collect(),
        };
        // Callers only see their own runs unless they hold manage.
        let can_see_all = has_capability(viewer_role.as_deref(), "network_tools:manage");
        if !can_see_all {
            if let Some(viewer_id) = &viewer_id {
                items.retain(|run| &run.user_id == viewer_id);
            } else {
                items.clear();
            }
        }
        if let Some(company) = &company_id {
            items.retain(|run| run.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(tool) = params.filters.get("tool") {
            items.retain(|run| run.tool == *tool);
        }
        if let Some(outcome) = params.filters.get("outcome") {
            items.retain(|run| run.outcome == *outcome);
        }
        if let Some(target) = params.filters.get("target") {
            items.retain(|run| run.target.contains(target.as_str()));
        }
        items.retain(|run| matches_search(&format!("{} {}", run.tool, run.target), &params));
        let sort = params.allowed_sort(&["tool", "outcome", "created_at"], "created_at");
        match sort.as_str() {
            "tool" => items.sort_by(|a, b| a.tool.cmp(&b.tool)),
            "outcome" => items.sort_by(|a, b| a.outcome.cmp(&b.outcome)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_history(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        let run = runs_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Tool run {id} not found")))?;
        let can_see_all = has_capability(viewer_role.as_deref(), "network_tools:manage");
        if !can_see_all && viewer_id.as_deref() != Some(run.user_id.as_str()) {
            return Err(UnifiedError::forbidden(
                "A run result can only be read by its owner or a manager",
            ));
        }
        Ok(json!(run))
    })();
    json::respond(result, debug)
}

/// Share a run result through the public-link sharing rule.
///
/// The auditors module owns link minting (`POST /api/v1/sharing/links`,
/// scope kind `network_tools_run`, view `summary`). This endpoint
/// verifies ownership, answers the sharing allow-check for the run,
/// and emits `network_tools.result.shared` with the exact scope the
/// caller must mint; the caller then mints the link through the
/// sharing API. The link itself is never minted here, so no token
/// handling is duplicated.
///
/// SEAM for the integrator: register kind `network_tools_run` with
/// view `summary` in the auditors module's `ShareRegistry` (one call
/// in the auditors module setup); until then link creation fails
/// closed with "not a registered shareable kind", by design.
async fn share_history(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
    body: web::Json<ShareRunRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let run = runs_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Tool run {id} not found")))?;
        let actor = viewer_id.clone().unwrap_or_else(|| "anonymous".to_string());
        if actor != run.user_id
            && !has_capability(viewer_role.as_deref(), "network_tools:probe_private")
            && viewer_role.as_deref() != Some("admin")
            && viewer_role.as_deref() != Some("super_admin")
        {
            return Err(UnifiedError::forbidden(
                "Only the run owner or an admin may share a run result",
            ));
        }
        let expires_in_days = body.into_inner().expires_in_days.unwrap_or(7).clamp(1, 30);
        ctx.hooks.emit(
            network_tool_events::RESULT_SHARED,
            json!({
                "run_id": run.id,
                "tool": run.tool,
                "target": run.target,
                "actor": actor,
                "scope_kind": "network_tools_run",
                "scope_view": "summary",
                "expires_in_days": expires_in_days,
            }),
        );
        Ok(json!({
            "run_id": run.id,
            "scope_kind": "network_tools_run",
            "scope_view": "summary",
            "expires_in_days": expires_in_days,
            "mint_via": "POST /api/v1/sharing/links",
            "mint_body": {
                "scope_kind": "network_tools_run",
                "scope_id": run.id,
                "scope_view": "summary",
                "expires_in_hours": expires_in_days * 24,
            },
            "note": "Mint the link through the sharing API; this module answers sharing.allow_check for the run and never mints tokens itself.",
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Saved checks (CRUD + run).
// ---------------------------------------------------------------------------

const KNOWN_TOOLS: [&str; 13] = [
    "dns",
    "ping",
    "traceroute",
    "port_check",
    "ssl_check",
    "whois",
    "http_check",
    "ip_info",
    "subnet",
    "webhook_test",
    "uptime_check",
    "blacklist",
    "cert_check",
];

fn validate_saved_check(tool: &str, target: &str) -> UnifiedResult<()> {
    if !KNOWN_TOOLS.contains(&tool) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown tool \"{tool}\"; known tools: {}",
            KNOWN_TOOLS.join(", ")
        )));
    }
    if target.trim().is_empty() || target.len() > 2048 {
        return Err(UnifiedError::bad_request(
            "Saved check target must not be empty and must be at most 2048 characters",
        ));
    }
    Ok(())
}

async fn list_saved_checks(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let mut items: Vec<NetworkToolSavedCheck> = match db_pool(&ctx) {
            Some(pool) => db_load_saved_checks(&pool).unwrap_or_else(|_| {
                saved_checks_store()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect()
            }),
            None => saved_checks_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(viewer_id) = &viewer_id {
            items.retain(|check| &check.user_id == viewer_id);
        } else if !has_capability(viewer_role.as_deref(), "network_tools:manage") {
            items.clear();
        }
        if let Some(company) = &company_id {
            items.retain(|check| check.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(tool) = params.filters.get("tool") {
            items.retain(|check| check.tool == *tool);
        }
        items.retain(|check| matches_search(&format!("{} {}", check.name, check.target), &params));
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_saved_check(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateSavedCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Saved check name must be 1-128 characters",
            ));
        }
        validate_saved_check(&input.tool, &input.target)?;
        let now = Utc::now();
        let check = NetworkToolSavedCheck {
            id: Uuid::new_v4(),
            user_id: viewer_id.unwrap_or_else(|| "anonymous".to_string()),
            company_id: parse_company_id(input.company_id.or(company_id)),
            name,
            tool: input.tool,
            target: input.target,
            parameters: input.parameters,
            created_at: now,
            updated_at: now,
        };
        saved_checks_store().insert(check.id, check.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_insert_saved_check(&pool, &check);
        }
        Ok(json!(check))
    })();
    json::respond(result, debug)
}

async fn get_saved_check(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        let check = saved_checks_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Saved check {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &check.user_id)?;
        Ok(json!(check))
    })();
    json::respond(result, debug)
}

async fn update_saved_check(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateSavedCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let mut check = saved_checks_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Saved check {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &check.user_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 128 {
                return Err(UnifiedError::bad_request(
                    "Saved check name must be 1-128 characters",
                ));
            }
            check.name = name;
        }
        if let Some(tool) = input.tool {
            validate_saved_check(&tool, &check.target)?;
            check.tool = tool;
        }
        if let Some(target) = input.target {
            validate_saved_check(&check.tool, &target)?;
            check.target = target;
        }
        if let Some(parameters) = input.parameters {
            check.parameters = parameters;
        }
        check.updated_at = Utc::now();
        saved_checks_store().insert(check.id, check.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_saved_check(&pool, &check);
        }
        Ok(json!(check))
    })();
    json::respond(result, debug)
}

async fn delete_saved_check(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let owner = saved_checks_store()
            .get(&id)
            .map(|entry| entry.value().user_id.clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Saved check {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &owner)?;
        saved_checks_store().remove(&id);
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_delete_saved_check(&pool, id);
        }
        Ok(json!({ "deleted": id }))
    })();
    json::respond(result, debug)
}

/// Re-run a saved check with its stored parameters. The parameter
/// JSON mirrors the per-tool request bodies (for example `{"record_type":
/// "MX"}` for DNS, `{"port": 443, "grab_banner": true}` for a port
/// check); unknown keys are ignored.
async fn run_saved_check(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role, company_id) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "saved_check:run", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let id = path.into_inner();
    let Some(check) = saved_checks_store()
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found(format!(
                "Saved check {id} not found"
            ))),
            ctx.is_debug(),
        );
    };
    let timeout_secs = check
        .parameters
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS);
    let target = check.target.clone();
    let result: UnifiedResult<ToolResult> = match check.tool.as_str() {
        "dns" => {
            let record_type = check
                .parameters
                .get("record_type")
                .and_then(|v| v.as_str())
                .unwrap_or("A")
                .to_string();
            tool_dns_lookup(&check.target, &record_type)
                .await
                .map(|value| ToolResult {
                    tool: "dns".to_string(),
                    target: check.target.clone(),
                    outcome: "success".to_string(),
                    duration_ms: 0,
                    method: Some("dns verifier".to_string()),
                    result: value,
                    notes: Vec::new(),
                })
        }
        "ping" => {
            let count = check
                .parameters
                .get("count")
                .and_then(|v| v.as_u64())
                .unwrap_or(4) as u32;
            tool_ping(&check.target, count, timeout_secs, allow_private).await
        }
        "traceroute" => {
            let max_hops = check
                .parameters
                .get("max_hops")
                .and_then(|v| v.as_u64())
                .unwrap_or(30) as u32;
            tool_traceroute(&check.target, max_hops, timeout_secs, allow_private).await
        }
        "port_check" => {
            let port = check
                .parameters
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(80) as u16;
            let grab_banner = check
                .parameters
                .get("grab_banner")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            tool_port_check(&check.target, port, grab_banner, timeout_secs, allow_private).await
        }
        "ssl_check" => {
            let port = check
                .parameters
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(443) as u16;
            tool_ssl_check(&ctx, &check.target, port, timeout_secs, allow_private).await
        }
        "whois" => tool_whois(&check.target, timeout_secs).await,
        "http_check" => {
            let method = check
                .parameters
                .get("method")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let expected = check
                .parameters
                .get("expected_status")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16);
            tool_http_check(&check.target, method, expected, timeout_secs, allow_private).await
        }
        "ip_info" => tool_ip_info(&ctx, &check.target).await,
        "subnet" => {
            let contains = check
                .parameters
                .get("contains")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            tool_subnet(&check.target, contains.as_deref())
        }
        "webhook_test" => {
            let payload = check
                .parameters
                .get("payload")
                .cloned()
                .unwrap_or_else(|| json!({ "event": "network_tools.test", "ok": true }));
            tool_webhook_test(&check.target, payload, timeout_secs, allow_private).await
        }
        "uptime_check" => {
            let expected = check
                .parameters
                .get("expected_status")
                .and_then(|v| v.as_u64())
                .unwrap_or(200) as i32;
            run_uptime_probe(&check.target, expected, timeout_secs, allow_private)
                .await
                .map(|(status, latency_ms, outcome)| ToolResult {
                    tool: "uptime_check".to_string(),
                    target: check.target.clone(),
                    outcome,
                    duration_ms: latency_ms,
                    method: Some("single http probe, no redirect following".to_string()),
                    result: json!({
                        "url": check.target,
                        "status": status,
                        "expected_status": expected,
                        "latency_ms": latency_ms,
                    }),
                    notes: Vec::new(),
                })
        }
        "blacklist" => {
            let zones = check
                .parameters
                .get("zones")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                });
            tool_blacklist(&check.target, zones, timeout_secs).await
        }
        "cert_check" => {
            tool_ssl_check(&ctx, &check.target, 443, timeout_secs, allow_private).await
        }
        other => Err(UnifiedError::bad_request(format!(
            "Saved check names unknown tool \"{other}\""
        ))),
    };
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        &check.tool,
        &target,
        result,
    )
    .await
}

// ---------------------------------------------------------------------------
// Uptime monitors (CRUD + trigger + events).
// ---------------------------------------------------------------------------

async fn list_uptime_monitors(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let mut items: Vec<NetworkToolUptimeMonitor> = match db_pool(&ctx) {
            Some(pool) => db_load_monitors(&pool).unwrap_or_else(|_| {
                uptime_monitors_store()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect()
            }),
            None => uptime_monitors_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(viewer_id) = &viewer_id {
            items.retain(|monitor| &monitor.user_id == viewer_id);
        } else if !has_capability(viewer_role.as_deref(), "network_tools:manage") {
            items.clear();
        }
        if let Some(company) = &company_id {
            items.retain(|monitor| monitor.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true";
            items.retain(|monitor| monitor.enabled == wanted);
        }
        items.retain(|monitor| {
            matches_search(&format!("{} {}", monitor.name, monitor.url), &params)
        });
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_uptime_monitor(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateUptimeMonitorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Monitor name must be 1-128 characters",
            ));
        }
        // Validate the URL shape now (without probing it); the SSRF
        // guard runs again on every probe.
        let parsed = input
            .url
            .trim()
            .parse::<reqwest::Url>()
            .map_err(|_| UnifiedError::bad_request("Monitor URL is not a valid URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Monitor URL must use http or https",
            ));
        }
        if input.interval_seconds < 60 || input.interval_seconds > 86400 {
            return Err(UnifiedError::bad_request(
                "Monitor interval must be 60-86400 seconds",
            ));
        }
        if input.expected_status < 100 || input.expected_status > 599 {
            return Err(UnifiedError::bad_request(
                "Expected status must be a valid HTTP status code",
            ));
        }
        if input.failure_threshold < 1 || input.failure_threshold > 10 {
            return Err(UnifiedError::bad_request(
                "Failure threshold must be 1-10",
            ));
        }
        let now = Utc::now();
        let monitor = NetworkToolUptimeMonitor {
            id: Uuid::new_v4(),
            user_id: viewer_id.unwrap_or_else(|| "anonymous".to_string()),
            company_id: parse_company_id(input.company_id.or(company_id)),
            name,
            url: input.url.trim().to_string(),
            interval_seconds: input.interval_seconds,
            expected_status: input.expected_status,
            failure_threshold: input.failure_threshold,
            consecutive_failures: 0,
            enabled: true,
            last_status: None,
            last_checked_at: None,
            created_at: now,
            updated_at: now,
        };
        uptime_monitors_store().insert(monitor.id, monitor.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_monitor(&pool, &monitor);
        }
        Ok(json!(monitor))
    })();
    json::respond(result, debug)
}

async fn get_uptime_monitor(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        let monitor = uptime_monitors_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Uptime monitor {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &monitor.user_id)?;
        Ok(json!(monitor))
    })();
    json::respond(result, debug)
}

async fn update_uptime_monitor(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateUptimeMonitorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let mut monitor = uptime_monitors_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Uptime monitor {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &monitor.user_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 128 {
                return Err(UnifiedError::bad_request(
                    "Monitor name must be 1-128 characters",
                ));
            }
            monitor.name = name;
        }
        if let Some(url) = input.url {
            let parsed = url
                .trim()
                .parse::<reqwest::Url>()
                .map_err(|_| UnifiedError::bad_request("Monitor URL is not a valid URL"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(UnifiedError::bad_request(
                    "Monitor URL must use http or https",
                ));
            }
            monitor.url = url.trim().to_string();
        }
        if let Some(interval) = input.interval_seconds {
            if !(60..=86400).contains(&interval) {
                return Err(UnifiedError::bad_request(
                    "Monitor interval must be 60-86400 seconds",
                ));
            }
            monitor.interval_seconds = interval;
        }
        if let Some(expected) = input.expected_status {
            if !(100..=599).contains(&expected) {
                return Err(UnifiedError::bad_request(
                    "Expected status must be a valid HTTP status code",
                ));
            }
            monitor.expected_status = expected;
        }
        if let Some(threshold) = input.failure_threshold {
            if !(1..=10).contains(&threshold) {
                return Err(UnifiedError::bad_request(
                    "Failure threshold must be 1-10",
                ));
            }
            monitor.failure_threshold = threshold;
        }
        if let Some(enabled) = input.enabled {
            monitor.enabled = enabled;
        }
        monitor.updated_at = Utc::now();
        uptime_monitors_store().insert(monitor.id, monitor.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_monitor(&pool, &monitor);
        }
        Ok(json!(monitor))
    })();
    json::respond(result, debug)
}

async fn delete_uptime_monitor(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let owner = uptime_monitors_store()
            .get(&id)
            .map(|entry| entry.value().user_id.clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Uptime monitor {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &owner)?;
        uptime_monitors_store().remove(&id);
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_delete_monitor(&pool, id);
        }
        Ok(json!({ "deleted": id }))
    })();
    json::respond(result, debug)
}

/// Trigger one evaluation of a monitor now. This is the endpoint the
/// scheduler arm (or an operator) calls; it also answers the
/// `network_tools.uptime.check_requested` hook event (see the hook
/// handler below, which acknowledges the request synchronously and
/// points the caller at this endpoint for the async probe).
async fn check_uptime_monitor(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let (actor, viewer_role, _) = tool_actor(&params);
    let allow_private = has_capability(viewer_role.as_deref(), "network_tools:probe_private");
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:manage")
        .and_then(|_| check_rate_limit(&actor, "uptime:check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), debug);
    }
    let id = path.into_inner();
    let result = evaluate_monitor(&ctx, id, &actor, allow_private).await;
    json::respond(result, debug)
}

async fn list_uptime_events(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (_, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        if !uptime_monitors_store().contains_key(&id) {
            return Err(UnifiedError::not_found(format!(
                "Uptime monitor {id} not found"
            )));
        }
        let mut items: Vec<NetworkToolUptimeEvent> = uptime_events_store()
            .iter()
            .filter(|entry| entry.value().monitor_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|event| event.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Certificate watches (CRUD + check).
// ---------------------------------------------------------------------------

async fn list_cert_watches(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let mut items: Vec<NetworkToolCertWatch> = match db_pool(&ctx) {
            Some(pool) => db_load_cert_watches(&pool).unwrap_or_else(|_| {
                cert_watches_store()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect()
            }),
            None => cert_watches_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(viewer_id) = &viewer_id {
            items.retain(|watch| &watch.user_id == viewer_id);
        } else if !has_capability(viewer_role.as_deref(), "network_tools:manage") {
            items.clear();
        }
        if let Some(company) = &company_id {
            items.retain(|watch| watch.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true";
            items.retain(|watch| watch.enabled == wanted);
        }
        items.retain(|watch| matches_search(&watch.domain, &params));
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_cert_watch(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateCertWatchRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, company_id) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let input = body.into_inner();
        let domain = validate_domain(&input.domain)?;
        if cert_watches_store()
            .iter()
            .any(|entry| entry.value().domain == domain)
        {
            return Err(UnifiedError::bad_request(format!(
                "A certificate watch already exists for \"{domain}\""
            )));
        }
        let now = Utc::now();
        let watch = NetworkToolCertWatch {
            id: Uuid::new_v4(),
            user_id: viewer_id.unwrap_or_else(|| "anonymous".to_string()),
            company_id: parse_company_id(input.company_id.or(company_id)),
            domain,
            enabled: true,
            last_days_remaining: None,
            last_checked_at: None,
            alerted_thresholds: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        cert_watches_store().insert(watch.id, watch.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_cert_watch(&pool, &watch);
        }
        Ok(json!(watch))
    })();
    json::respond(result, debug)
}

async fn delete_cert_watch(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role, _) = viewer_of(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let owner = cert_watches_store()
            .get(&id)
            .map(|entry| entry.value().user_id.clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Certificate watch {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &owner)?;
        cert_watches_store().remove(&id);
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_delete_cert_watch(&pool, id);
        }
        Ok(json!({ "deleted": id }))
    })();
    json::respond(result, debug)
}

async fn check_cert_watches_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let (actor, viewer_role, _) = tool_actor(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:manage")
        .and_then(|_| check_rate_limit(&actor, "cert:check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), debug);
    }
    let result = check_cert_watches(&ctx, &actor).await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Answer `network_tools.uptime.check_requested` (payload
/// `{"monitor_id", "actor"}`). Hook handlers are synchronous, so this
/// handler only validates the request and records it; the async probe
/// runs through `POST /uptime-monitors/{id}/check`, which the
/// scheduler arm calls. The result carries that pointer, so a caller
/// that only emits the event never mistakes the acknowledgement for a
/// completed probe.
fn on_uptime_check_requested(ctx: &mut HookContext) {
    let monitor_id = ctx
        .data
        .get("monitor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let actor = ctx
        .data
        .get("actor")
        .and_then(|v| v.as_str())
        .unwrap_or("system")
        .to_string();
    let parsed = Uuid::parse_str(&monitor_id);
    match parsed {
        Ok(id) if uptime_monitors_store().contains_key(&id) => {
            ctx.add_result(json!({
                "monitor_id": id,
                "actor": actor,
                "accepted": true,
                "probe_via": format!("POST /api/v1/network-tools/uptime-monitors/{id}/check"),
                "note": "Hook handlers are synchronous; the async probe runs through the trigger endpoint.",
            }));
        }
        Ok(id) => {
            ctx.add_result(json!({
                "monitor_id": id,
                "accepted": false,
                "error": "Uptime monitor not found",
            }));
        }
        Err(_) => {
            ctx.add_result(json!({
                "accepted": false,
                "error": "monitor_id is missing or is not a UUID",
            }));
        }
    }
}

/// Answer `network_tools.cert.check_requested` by running the
/// synchronous threshold scan over the last known expiry data (no
/// network probe inside a sync handler). Use
/// `POST /api/v1/network-tools/cert-watches/check` for a live probe.
fn on_cert_check_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(json!({
            "checked": 0,
            "error": "network_tools module is not initialised",
        }));
        return;
    };
    ctx.add_result(check_cert_watches_sync(&app_ctx));
}

/// Answer the sharing `sharing.allow_check` hook for run results:
/// allow only when the scope kind is `network_tools_run`, the run
/// exists, and the requesting actor owns the run. No handler answering
/// means denied (fail closed); this handler never allows
/// unconditionally and never allows a run the actor does not own.
fn on_sharing_allow_check(ctx: &mut HookContext) {
    let kind = ctx.data.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind != "network_tools_run" {
        return;
    }
    let id_text = ctx.data.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let actor = ctx.data.get("actor").and_then(|v| v.as_str()).unwrap_or("");
    let allowed = Uuid::parse_str(id_text)
        .ok()
        .and_then(|id| runs_store().get(&id).map(|entry| entry.value().clone()))
        .map(|run| run.user_id == actor)
        .unwrap_or(false);
    if allowed {
        if let Value::Object(map) = &mut ctx.data {
            map.insert("allowed".to_string(), json!(true));
        }
    }
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct NetworkToolsModule;

impl Module for NetworkToolsModule {
    fn id(&self) -> &'static str {
        "network_tools"
    }

    fn name(&self) -> &'static str {
        "Network Tools"
    }

    fn description(&self) -> &'static str {
        "Developer network tools: DNS lookup, ping, traceroute, port check, SSL/TLS checker, WHOIS, HTTP checks, IP info and Geo, subnet calculator, webhook tester, uptime monitors, certificate expiry watch, and DNS blacklist checks, with saved checks, run history, and shareable results."
    }

    fn requires(&self) -> &[&'static str] {
        &["network", "geo", "certificates", "notifications", "api_manager"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &[
            "hickory-resolver",
            "reqwest",
            "tokio",
            "rustls",
            "x509-certificate",
            "chrono",
            "serde",
            "dashmap",
            "uuid",
            "diesel",
        ]
    }

    fn requires_external_libraries(&self) -> &[crate::core::modules::ExternalLibraryRequirement] {
        &[]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = NETWORK_TOOLS_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            network_tool_events::UPTIME_CHECK_REQUESTED,
            "network_tools",
            Arc::new(on_uptime_check_requested),
        );
        bus.register(
            network_tool_events::CERT_CHECK_REQUESTED,
            "network_tools",
            Arc::new(on_cert_check_requested),
        );
        bus.register(
            crate::core::sharing::sharing_events::ALLOW_CHECK,
            "network_tools",
            Arc::new(on_sharing_allow_check),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/network-tools/tools", owner: "network_tools", description: "List available network tools and limits" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/dns", owner: "network_tools", description: "DNS lookup (A/AAAA/MX/TXT/CNAME/NS/SOA)" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/ping", owner: "network_tools", description: "Ping with RTT and packet loss" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/traceroute", owner: "network_tools", description: "Traceroute hop list" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/port-check", owner: "network_tools", description: "TCP port check with optional banner grab" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/ssl-check", owner: "network_tools", description: "SSL/TLS checker (chain, expiry, protocol, cipher)" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/whois", owner: "network_tools", description: "WHOIS lookup (domain or IP, one referral)" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/http-check", owner: "network_tools", description: "HTTP header/status check with timings" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/ip-info", owner: "network_tools", description: "IP info + Geo + reverse DNS" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/subnet", owner: "network_tools", description: "Subnet/CIDR calculator (IPv4 + IPv6)" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/webhook-test", owner: "network_tools", description: "Webhook tester (POST JSON, no redirect following)" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/uptime-check", owner: "network_tools", description: "One-off uptime check" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/blacklist", owner: "network_tools", description: "DNS blacklist check" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/history", owner: "network_tools", description: "List tool run history (paginated, filtered)" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/history/{id}", owner: "network_tools", description: "Get one tool run" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/history/{id}/share", owner: "network_tools", description: "Share a run result via a public link scope" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/saved-checks", owner: "network_tools", description: "List saved checks" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/saved-checks", owner: "network_tools", description: "Create a saved check" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/saved-checks/{id}", owner: "network_tools", description: "Get one saved check" },
            RouteInfo { method: "PUT", path: "/api/v1/network-tools/saved-checks/{id}", owner: "network_tools", description: "Update a saved check" },
            RouteInfo { method: "DELETE", path: "/api/v1/network-tools/saved-checks/{id}", owner: "network_tools", description: "Delete a saved check" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/saved-checks/{id}/run", owner: "network_tools", description: "Run a saved check" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/uptime-monitors", owner: "network_tools", description: "List uptime monitors" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/uptime-monitors", owner: "network_tools", description: "Create an uptime monitor" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/uptime-monitors/{id}", owner: "network_tools", description: "Get one uptime monitor" },
            RouteInfo { method: "PUT", path: "/api/v1/network-tools/uptime-monitors/{id}", owner: "network_tools", description: "Update an uptime monitor" },
            RouteInfo { method: "DELETE", path: "/api/v1/network-tools/uptime-monitors/{id}", owner: "network_tools", description: "Delete an uptime monitor" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/uptime-monitors/{id}/check", owner: "network_tools", description: "Trigger one evaluation of a monitor now" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/uptime-monitors/{id}/events", owner: "network_tools", description: "List a monitor's uptime events" },
            RouteInfo { method: "GET", path: "/api/v1/network-tools/cert-watches", owner: "network_tools", description: "List certificate watches" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/cert-watches", owner: "network_tools", description: "Create a certificate watch" },
            RouteInfo { method: "DELETE", path: "/api/v1/network-tools/cert-watches/{id}", owner: "network_tools", description: "Delete a certificate watch" },
            RouteInfo { method: "POST", path: "/api/v1/network-tools/cert-watches/check", owner: "network_tools", description: "Run the certificate expiry check now" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/network-tools")
                .route("/tools", web::get().to(list_tools))
                .route("/dns", web::post().to(dns_handler))
                .route("/ping", web::post().to(ping_handler))
                .route("/traceroute", web::post().to(traceroute_handler))
                .route("/port-check", web::post().to(port_check_handler))
                .route("/ssl-check", web::post().to(ssl_check_handler))
                .route("/whois", web::post().to(whois_handler))
                .route("/http-check", web::post().to(http_check_handler))
                .route("/ip-info", web::post().to(ip_info_handler))
                .route("/subnet", web::post().to(subnet_handler))
                .route("/webhook-test", web::post().to(webhook_test_handler))
                .route("/uptime-check", web::post().to(uptime_check_handler))
                .route("/blacklist", web::post().to(blacklist_handler))
                .route("/history", web::get().to(list_history))
                .route("/history/{id}", web::get().to(get_history))
                .route("/history/{id}/share", web::post().to(share_history))
                .route("/saved-checks", web::get().to(list_saved_checks))
                .route("/saved-checks", web::post().to(create_saved_check))
                .route("/saved-checks/{id}", web::get().to(get_saved_check))
                .route("/saved-checks/{id}", web::put().to(update_saved_check))
                .route("/saved-checks/{id}", web::delete().to(delete_saved_check))
                .route("/saved-checks/{id}/run", web::post().to(run_saved_check))
                .route("/uptime-monitors", web::get().to(list_uptime_monitors))
                .route("/uptime-monitors", web::post().to(create_uptime_monitor))
                .route("/uptime-monitors/{id}", web::get().to(get_uptime_monitor))
                .route("/uptime-monitors/{id}", web::put().to(update_uptime_monitor))
                .route("/uptime-monitors/{id}", web::delete().to(delete_uptime_monitor))
                .route("/uptime-monitors/{id}/check", web::post().to(check_uptime_monitor))
                .route("/uptime-monitors/{id}/events", web::get().to(list_uptime_events))
                .route("/cert-watches", web::get().to(list_cert_watches))
                .route("/cert-watches", web::post().to(create_cert_watch))
                .route("/cert-watches/{id}", web::delete().to(delete_cert_watch))
                .route("/cert-watches/check", web::post().to(check_cert_watches_handler)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NetworkToolsModule));
}
