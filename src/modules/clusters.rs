// Multi-Cluster module — clusters of Unified nodes and provider pools,
// node registration with HMAC-signed node-to-node authentication, health
// probing, and mutual sync between clusters.
//
// A cluster groups nodes. A node is either another Unified instance or a
// provider region / pool. Nodes authenticate to each other with
// HMAC-SHA256 request signatures (timestamp + nonce + body hash, replay
// window, constant-time comparison). Only a SHA-256 hash of a node secret
// is ever stored; the secret itself is returned exactly once, at
// registration or rotation time, and is never logged or returned again.
// API responses carry only a masked fingerprint of the secret hash.
//
// Mutual sync moves versioned items for the registered sync domains
// between nodes. Conflicts are resolved last-write-wins on `updated_at`
// and every conflict is recorded twice: in the sync log entry and as a
// first-class conflict record an admin can list, inspect (both
// payloads) and resolve (`keep_local`, `keep_remote` or `dismiss`) —
// overwrites are never silent. A dry-run mode reports what would change
// without applying anything.
//
// Reliability: a failed sync domain enters a retry queue with
// exponential backoff (settings-driven); after the configured maximum
// attempts it becomes dead-letter — visible to admins, never retried
// automatically, retryable or discardable by hand. An aggregate sync
// health endpoint reports node health, failing plans, open conflicts,
// pending retries and dead-letters at a glance.
//
// All behaviour knobs (timeouts, replay window, failure threshold,
// retention caps, retry backoff, idempotency TTL) live in `SyncSettings`
// as data, seeded from configurable defaults and editable by admins
// through `GET/PUT /sync/settings`. The synced domains themselves live
// in a registry, seeded with the defaults and editable through
// `/sync/domains`.
//
// Admin endpoints require `X-User-Id` and `X-Role` headers and enforce
// the `clusters:view` / `clusters:manage` capabilities through the
// `role.check` hook (fail-closed). Node-to-node endpoints authenticate
// with HMAC signatures instead; the liveness probe is public.
// Mutating admin calls accept an `Idempotency-Key` header: replaying a
// request with the same key returns the stored response instead of
// running the operation again.
//
// Routes under `/api/v1/clusters` (see `ClustersModule::route_info` for
// the full catalog). State is held in memory; the matching tables live
// in migrations `000008_clusters` and `000074_cluster_sync_reliability`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Sync payload domains seeded into the domain registry at startup.
/// The registry is the source of truth at runtime: admins can register
/// or unregister domains through the API, and these values are only the
/// configurable defaults. Keep in sync with the integrator migration.
pub const DEFAULT_SYNC_DOMAINS: [&str; 5] = ["config", "modules", "software", "licenses", "dns_ssl"];

/// Envelope version carried by every node-to-node sync payload.
pub const SYNC_ENVELOPE_VERSION: u32 = 1;

/// Configurable defaults for sync behaviour. Every value below is a
/// default only: the live values live in `SyncSettings` on the module
/// state and are editable by admins through `GET/PUT /sync/settings`.
/// Nothing here is a hard-coded rule.
pub const DEFAULT_REPLAY_WINDOW_SECS: i64 = 300;
/// HTTP timeout for node health probes.
pub const DEFAULT_HEALTH_TIMEOUT_SECS: u64 = 5;
/// HTTP timeout for sync calls between nodes.
pub const DEFAULT_SYNC_TIMEOUT_SECS: u64 = 30;
/// Consecutive failed probes before a node is `unhealthy`.
pub const DEFAULT_HEALTH_FAILURE_THRESHOLD: u32 = 3;
/// Maximum sync log entries retained in memory.
pub const DEFAULT_MAX_SYNC_LOGS: usize = 1000;
/// Maximum health-check history entries retained in memory.
pub const DEFAULT_MAX_HEALTH_HISTORY: usize = 5000;
/// Maximum conflict records retained in memory.
pub const DEFAULT_MAX_CONFLICTS: usize = 5000;
/// Maximum retry-queue entries retained in memory.
pub const DEFAULT_MAX_RETRIES: usize = 5000;
/// Maximum idempotency records retained in memory.
pub const DEFAULT_MAX_IDEMPOTENCY_RECORDS: usize = 5000;
/// Maximum failed attempts before a retry entry becomes dead-letter.
pub const DEFAULT_RETRY_MAX_ATTEMPTS: u32 = 5;
/// Base delay in seconds before the first retry.
pub const DEFAULT_RETRY_BASE_DELAY_SECS: u64 = 60;
/// Upper bound in seconds for the exponential backoff delay.
pub const DEFAULT_RETRY_MAX_DELAY_SECS: u64 = 3600;
/// Backoff multiplier applied per attempt.
pub const DEFAULT_RETRY_BACKOFF_MULTIPLIER: f64 = 2.0;
/// How long an idempotency key stays valid, in seconds (24 hours).
pub const DEFAULT_IDEMPOTENCY_TTL_SECS: i64 = 86_400;

/// Capabilities enforced on the admin endpoints of this module. Reads
/// require `clusters:view`; every write requires `clusters:manage`.
/// Node-to-node endpoints (`/sync/receive`, `/sync/export`) do not use
/// these: they authenticate with HMAC node signatures instead. The
/// liveness probe (`/health`) is intentionally public.
pub const PERM_CLUSTERS_VIEW: &str = "clusters:view";
pub const PERM_CLUSTERS_MANAGE: &str = "clusters:manage";

/// Header carrying a client-chosen idempotency key for mutating admin
/// calls. Replaying a request with the same key returns the stored
/// response instead of running the operation again.
pub const HEADER_IDEMPOTENCY_KEY: &str = "Idempotency-Key";
pub const HEADER_IDEMPOTENCY_KEY_ALT: &str = "X-Idempotency-Key";
/// Maximum length of an idempotency key.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 128;

/// Hook events emitted by this module.
pub const CLUSTER_CREATED: &str = "cluster.created";
pub const CLUSTER_UPDATED: &str = "cluster.updated";
pub const CLUSTER_NODE_REGISTERED: &str = "cluster.node.registered";
pub const CLUSTER_NODE_REVOKED: &str = "cluster.node.revoked";
pub const CLUSTER_HEALTH_CHANGED: &str = "cluster.health.changed";
pub const CLUSTER_SYNC_STARTED: &str = "cluster.sync.started";
pub const CLUSTER_SYNC_COMPLETED: &str = "cluster.sync.completed";
pub const CLUSTER_SYNC_FAILED: &str = "cluster.sync.failed";
pub const CLUSTER_SYNC_CONFLICT: &str = "cluster.sync.conflict";
pub const CLUSTER_SYNC_CONFLICT_RESOLVED: &str = "cluster.sync.conflict.resolved";
pub const CLUSTER_SYNC_RETRY_QUEUED: &str = "cluster.sync.retry.queued";
pub const CLUSTER_SYNC_DEAD_LETTER: &str = "cluster.sync.dead_letter";
pub const CLUSTER_SYNC_RETRY_DUE: &str = "cluster.sync.retry_due";
pub const CLUSTER_SYNC_SETTINGS_UPDATED: &str = "cluster.sync.settings.updated";
/// Hook events this module listens to (emitted by the scheduler or by
/// operators through the hook bus) for background work.
pub const CLUSTER_HEALTH_CHECK_REQUESTED: &str = "cluster.health.check_requested";
pub const CLUSTER_SYNC_REQUESTED: &str = "cluster.sync.requested";

const HEADER_NODE_ID: &str = "x-cluster-node-id";
const HEADER_TIMESTAMP: &str = "x-cluster-timestamp";
const HEADER_NONCE: &str = "x-cluster-nonce";
const HEADER_SIGNATURE: &str = "x-cluster-signature";

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index])?;
        let low = hex_value(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// Constant-time equality for signature comparison.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

/// A masked fingerprint of a stored secret hash. The raw secret and the
/// full hash are never exposed; only this prefix/suffix is returned.
fn secret_fingerprint(secret_hash_hex: &str) -> String {
    let chars: Vec<char> = secret_hash_hex.chars().collect();
    if chars.len() < 12 {
        return "sha256:masked".to_string();
    }
    let first: String = chars.iter().take(8).collect();
    let last: String = chars.iter().skip(chars.len() - 4).collect();
    format!("sha256:{first}...{last}")
}

/// Generate a new node secret. Returned to the caller exactly once; only
/// its SHA-256 hash is stored.
fn generate_secret() -> String {
    format!("cls_{}_{}", Uuid::new_v4().as_simple(), Uuid::new_v4().as_simple())
}

fn hash_secret(secret: &str) -> String {
    sha256_hex(secret.as_bytes())
}

fn require_non_empty(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} is required")));
    }
    Ok(trimmed.to_string())
}

fn validate_base_url(value: &str) -> UnifiedResult<String> {
    let url = require_non_empty(value, "base_url")?;
    if url.len() > 512 {
        return Err(UnifiedError::bad_request("base_url is too long"));
    }
    if url.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("base_url must not contain whitespace"));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(UnifiedError::bad_request(
            "base_url must start with http:// or https://",
        ));
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Validate a sync domain against the registered domain registry
/// (admin-editable; seeded with `DEFAULT_SYNC_DOMAINS`).
fn validate_domain(state: &Arc<ClustersState>, domain: &str) -> UnifiedResult<String> {
    let domain = domain.trim().to_string();
    let known: Vec<String> = {
        let mut known: Vec<String> = state
            .domains
            .read()
            .expect("domains lock poisoned")
            .iter()
            .cloned()
            .collect();
        known.sort();
        known
    };
    if !known.iter().any(|known| known == &domain) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown sync domain: {domain}. Known domains: {}",
            known.join(", ")
        )));
    }
    Ok(domain)
}

fn validate_domains(
    state: &Arc<ClustersState>,
    domains: Vec<String>,
) -> UnifiedResult<Vec<String>> {
    if domains.is_empty() {
        return Err(UnifiedError::bad_request("At least one sync domain is required"));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for domain in domains {
        let domain = validate_domain(state, &domain)?;
        if seen.insert(domain.clone()) {
            out.push(domain);
        }
    }
    Ok(out)
}

/// A schedule is either an interval in seconds or five cron-like fields
/// where each field is `*`, `*/N`, or a plain number.
fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if schedule.parse::<u64>().is_ok() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field.parse::<u32>().is_ok()
    })
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks.
//
// Admin endpoints require `X-User-Id` and `X-Role` headers. The role is
// checked through the `role.check` hook answered by the roles module.
// The check fails closed: no answer means not allowed. Node-to-node
// endpoints do not use this; they authenticate with HMAC signatures.
// ---------------------------------------------------------------------------

struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    let who = caller(req)?;
    if check_capability(ctx, &who.role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            who.role, permission
        )))
    }
}

// ---------------------------------------------------------------------------
// Configurable sync settings.
//
// All behaviour knobs live here as data, seeded from the defaults above
// and editable at runtime through `GET/PUT /api/v1/clusters/sync/settings`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    pub replay_window_secs: i64,
    pub health_timeout_secs: u64,
    pub sync_timeout_secs: u64,
    pub health_failure_threshold: u32,
    pub max_sync_logs: usize,
    pub max_health_history: usize,
    pub max_conflicts: usize,
    pub max_retries: usize,
    pub max_idempotency_records: usize,
    pub retry_max_attempts: u32,
    pub retry_base_delay_secs: u64,
    pub retry_max_delay_secs: u64,
    pub retry_backoff_multiplier: f64,
    pub idempotency_ttl_secs: i64,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            replay_window_secs: DEFAULT_REPLAY_WINDOW_SECS,
            health_timeout_secs: DEFAULT_HEALTH_TIMEOUT_SECS,
            sync_timeout_secs: DEFAULT_SYNC_TIMEOUT_SECS,
            health_failure_threshold: DEFAULT_HEALTH_FAILURE_THRESHOLD,
            max_sync_logs: DEFAULT_MAX_SYNC_LOGS,
            max_health_history: DEFAULT_MAX_HEALTH_HISTORY,
            max_conflicts: DEFAULT_MAX_CONFLICTS,
            max_retries: DEFAULT_MAX_RETRIES,
            max_idempotency_records: DEFAULT_MAX_IDEMPOTENCY_RECORDS,
            retry_max_attempts: DEFAULT_RETRY_MAX_ATTEMPTS,
            retry_base_delay_secs: DEFAULT_RETRY_BASE_DELAY_SECS,
            retry_max_delay_secs: DEFAULT_RETRY_MAX_DELAY_SECS,
            retry_backoff_multiplier: DEFAULT_RETRY_BACKOFF_MULTIPLIER,
            idempotency_ttl_secs: DEFAULT_IDEMPOTENCY_TTL_SECS,
        }
    }
}

impl SyncSettings {
    /// Delay before attempt `attempt` (1-based) under exponential
    /// backoff, capped at `retry_max_delay_secs`. Attempt 1 waits the
    /// base delay; each later attempt multiplies by the backoff factor.
    fn retry_delay_secs(&self, attempt: u32) -> u64 {
        if attempt == 0 {
            return self.retry_base_delay_secs;
        }
        let mut delay = self.retry_base_delay_secs as f64;
        for _ in 1..attempt {
            delay *= self.retry_backoff_multiplier;
            if delay >= self.retry_max_delay_secs as f64 {
                return self.retry_max_delay_secs;
            }
        }
        (delay as u64).min(self.retry_max_delay_secs)
    }
}

// ---------------------------------------------------------------------------
// Conflict records, retry-queue entries, idempotency records.
// ---------------------------------------------------------------------------

/// One sync conflict, stored on its own so an admin can see and resolve
/// it. Conflicts are also embedded in the sync log entry that produced
/// them, but the log rotates; this record is the durable view and the
/// only one the admin resolves. `status` is `open`, `resolved` or
/// `dismissed`. `resolution` records the admin's choice (`keep_local`,
/// `keep_remote` or `dismiss`) together with who decided and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConflictRecord {
    pub id: Uuid,
    pub log_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub key: String,
    pub local_updated_at: Option<DateTime<Utc>>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    /// `local` or `remote`: which version won the automatic
    /// last-write-wins decision.
    pub winner: String,
    pub reason: String,
    /// The payloads that disagreed, kept so a resolution can actually
    /// apply the losing side instead of only marking the conflict.
    pub local_payload: Option<Value>,
    pub remote_payload: Option<Value>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// One failed sync domain waiting for retry. `status` is `pending`,
/// `retrying`, `completed`, `dead_letter` or `discarded`. A pending
/// entry becomes `dead_letter` once `attempt` reaches the configured
/// maximum; dead-letter entries are visible to admins and are never
/// retried automatically again — an admin must retry or discard them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRetryEntry {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub direction: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub next_retry_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One stored response for a replayed idempotent request. `scope`
/// namespaces the key (for example `clusters:create` or
/// `clusters:sync:run:<plan-id>`), so the same key can be reused safely
/// across different operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    pub scope: String,
    pub key: String,
    pub status: u16,
    pub body: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Read and validate the idempotency key from the request headers.
/// Returns `Ok(None)` when no key was sent (the call runs normally),
/// `Ok(Some(key))` for a usable key, and an error for a malformed one.
fn idempotency_key_from(req: &HttpRequest) -> UnifiedResult<Option<String>> {
    let raw = req
        .headers()
        .get(HEADER_IDEMPOTENCY_KEY)
        .or_else(|| req.headers().get(HEADER_IDEMPOTENCY_KEY_ALT))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(key) = raw else {
        return Ok(None);
    };
    if key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(UnifiedError::bad_request(
            "Idempotency-Key is too long (maximum 128 characters)",
        ));
    }
    if key.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request(
            "Idempotency-Key must not contain whitespace",
        ));
    }
    Ok(Some(key.to_string()))
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// `unified` (another Unified instance group) or `provider_pool`.
    pub kind: String,
    /// `active`, `paused` or `archived`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub display_name: String,
    pub base_url: String,
    /// `primary`, `member` or `edge`.
    pub role: String,
    /// `active` or `revoked`.
    pub status: String,
    /// SHA-256 hash of the node secret. Never serialized; the HMAC key
    /// for node-to-node calls is the raw bytes of this hash, so the peer
    /// (which holds the secret) derives the same key.
    #[serde(skip_serializing)]
    pub secret_hash: String,
    /// Masked fingerprint of `secret_hash`; the only secret-derived
    /// value ever returned by the API.
    pub secret_fingerprint: String,
    /// `healthy`, `degraded`, `unhealthy` or `unknown`.
    pub health: String,
    pub consecutive_failures: u32,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPlanState {
    pub id: Uuid,
    pub cluster_id: Uuid,
    /// The peer node this plan syncs with (a member of the cluster).
    pub target_node_id: Uuid,
    /// The node id this instance uses when talking to the peer — the id
    /// the peer registered for us. Sent as `x-cluster-node-id`.
    pub local_node_id: String,
    /// `push`, `pull` or `bidirectional`.
    pub direction: String,
    pub domains: Vec<String>,
    /// Interval in seconds or a cron-like expression.
    pub schedule: String,
    pub enabled: bool,
    /// SHA-256 hash of the peer secret (the secret we were given when
    /// the peer registered us). Never serialized; used as the HMAC key
    /// for outbound calls.
    #[serde(skip_serializing)]
    pub peer_secret_hash: String,
    pub peer_secret_fingerprint: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One versioned item of local domain state, the unit sync moves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncItem {
    pub domain: String,
    pub key: String,
    pub payload: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConflict {
    pub domain: String,
    pub key: String,
    pub local_updated_at: Option<DateTime<Utc>>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    /// `local` or `remote`: which version won.
    pub winner: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncLogEntry {
    pub id: Uuid,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub direction: String,
    pub domain: String,
    /// `completed`, `failed` or `dry_run`.
    pub status: String,
    pub items_sent: u64,
    pub items_received: u64,
    pub items_applied: u64,
    pub items_skipped: u64,
    pub conflicts: Vec<SyncConflict>,
    pub errors: Vec<String>,
    pub dry_run: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckEntry {
    pub id: Uuid,
    pub node_id: Uuid,
    pub cluster_id: Uuid,
    /// `healthy`, `degraded`, `unhealthy` or `unknown`.
    pub status: String,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

pub struct ClustersState {
    hooks: RwLock<Arc<HookBus>>,
    clusters: RwLock<HashMap<Uuid, ClusterState>>,
    nodes: RwLock<HashMap<Uuid, NodeState>>,
    plans: RwLock<HashMap<Uuid, SyncPlanState>>,
    logs: RwLock<Vec<SyncLogEntry>>,
    health_history: RwLock<Vec<HealthCheckEntry>>,
    /// Local domain state: domain -> key -> item.
    local_items: RwLock<HashMap<String, HashMap<String, SyncItem>>>,
    /// Nonces seen within the replay window, for replay protection.
    used_nonces: RwLock<HashMap<String, DateTime<Utc>>>,
    /// Live sync settings (admin-editable; seeded from the defaults).
    settings: RwLock<SyncSettings>,
    /// Registered sync domains. Seeded with `DEFAULT_SYNC_DOMAINS`;
    /// admins can register or unregister domains through the API.
    domains: RwLock<HashSet<String>>,
    /// Conflict records, independent of the rotating sync log.
    conflicts: RwLock<Vec<SyncConflictRecord>>,
    /// Failed-sync retry queue, including dead-letter entries.
    retries: RwLock<Vec<SyncRetryEntry>>,
    /// Stored responses for idempotent admin writes, keyed by
    /// (scope, key).
    idempotency: RwLock<HashMap<(String, String), IdempotencyRecord>>,
}

impl ClustersState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            clusters: RwLock::new(HashMap::new()),
            nodes: RwLock::new(HashMap::new()),
            plans: RwLock::new(HashMap::new()),
            logs: RwLock::new(Vec::new()),
            health_history: RwLock::new(Vec::new()),
            local_items: RwLock::new(HashMap::new()),
            used_nonces: RwLock::new(HashMap::new()),
            settings: RwLock::new(SyncSettings::default()),
            domains: RwLock::new(DEFAULT_SYNC_DOMAINS.iter().map(|d| d.to_string()).collect()),
            conflicts: RwLock::new(Vec::new()),
            retries: RwLock::new(Vec::new()),
            idempotency: RwLock::new(HashMap::new()),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn request_notification(&self, title: &str, body: &str, kind: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": "system",
                "title": title,
                "body": body,
                "kind": kind,
            }),
        );
    }

    fn settings(&self) -> SyncSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn push_log(&self, entry: SyncLogEntry) {
        let max = self.settings().max_sync_logs;
        let mut logs = self.logs.write().expect("logs lock poisoned");
        logs.push(entry);
        if logs.len() > max {
            let excess = logs.len() - max;
            logs.drain(..excess);
        }
    }

    fn push_health(&self, entry: HealthCheckEntry) {
        let max = self.settings().max_health_history;
        let mut history = self.health_history.write().expect("health lock poisoned");
        history.push(entry);
        if history.len() > max {
            let excess = history.len() - max;
            history.drain(..excess);
        }
    }

    /// Look up a stored idempotent response. Expired records are pruned
    /// on access. Returns the stored status and response body.
    fn idempotency_lookup(&self, scope: &str, key: &str) -> Option<(u16, Value)> {
        let mut store = self.idempotency.write().expect("idempotency lock poisoned");
        let now = Utc::now();
        store.retain(|_, record| record.expires_at > now);
        store
            .get(&(scope.to_string(), key.to_string()))
            .map(|record| (record.status, record.body.clone()))
    }

    /// Store the response of an idempotent write for later replays.
    fn idempotency_store(&self, scope: &str, key: &str, status: u16, body: Value) {
        let settings = self.settings();
        let now = Utc::now();
        let record = IdempotencyRecord {
            scope: scope.to_string(),
            key: key.to_string(),
            status,
            body,
            created_at: now,
            expires_at: now + chrono::Duration::seconds(settings.idempotency_ttl_secs),
        };
        let mut store = self.idempotency.write().expect("idempotency lock poisoned");
        store.insert((scope.to_string(), key.to_string()), record);
        if store.len() > settings.max_idempotency_records {
            let mut records: Vec<IdempotencyRecord> =
                store.values().cloned().collect();
            records.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            let excess = store.len() - settings.max_idempotency_records;
            for stale in records.into_iter().take(excess) {
                store.remove(&(stale.scope, stale.key));
            }
        }
    }

    /// Record conflicts as first-class records an admin can list and
    /// resolve. `local_payloads` / `remote_payloads` map conflict keys
    /// to the payloads that disagreed, when the caller has them.
    fn record_conflicts(
        &self,
        log_id: Option<Uuid>,
        plan_id: Option<Uuid>,
        cluster_id: Uuid,
        node_id: Option<Uuid>,
        conflicts: &[SyncConflict],
        local_payloads: &HashMap<String, Value>,
        remote_payloads: &HashMap<String, Value>,
    ) {
        if conflicts.is_empty() {
            return;
        }
        let max = self.settings().max_conflicts;
        let mut store = self.conflicts.write().expect("conflicts lock poisoned");
        for conflict in conflicts {
            store.push(SyncConflictRecord {
                id: Uuid::now_v7(),
                log_id,
                plan_id,
                cluster_id,
                node_id,
                domain: conflict.domain.clone(),
                key: conflict.key.clone(),
                local_updated_at: conflict.local_updated_at,
                remote_updated_at: conflict.remote_updated_at,
                winner: conflict.winner.clone(),
                reason: conflict.reason.clone(),
                local_payload: local_payloads.get(&conflict.key).cloned(),
                remote_payload: remote_payloads.get(&conflict.key).cloned(),
                status: "open".to_string(),
                resolution: None,
                resolved_by: None,
                resolved_at: None,
                created_at: now(),
            });
        }
        if store.len() > max {
            let excess = store.len() - max;
            store.drain(..excess);
        }
    }

    /// Queue (or re-queue) a failed sync domain for retry with
    /// exponential backoff. An existing pending entry for the same plan
    /// and domain is updated in place so a repeatedly failing domain
    /// keeps one queue entry, not one per failure. Once the attempt
    /// count reaches the configured maximum the entry becomes
    /// dead-letter: visible to admins, never retried automatically.
    fn enqueue_retry(
        &self,
        plan_id: Uuid,
        cluster_id: Uuid,
        node_id: Option<Uuid>,
        domain: &str,
        direction: &str,
        error: &str,
    ) -> SyncRetryEntry {
        let settings = self.settings();
        let now = now();
        let mut queue = self.retries.write().expect("retries lock poisoned");
        if let Some(entry) = queue.iter_mut().find(|entry| {
            entry.plan_id == plan_id
                && entry.domain == domain
                && matches!(entry.status.as_str(), "pending" | "retrying")
        }) {
            entry.attempt += 1;
            entry.last_error = Some(error.to_string());
            entry.updated_at = now;
            if entry.attempt >= entry.max_attempts {
                entry.status = "dead_letter".to_string();
                entry.next_retry_at = now;
            } else {
                entry.status = "pending".to_string();
                entry.next_retry_at =
                    now + chrono::Duration::seconds(settings.retry_delay_secs(entry.attempt) as i64);
            }
            let updated = entry.clone();
            drop(queue);
            if updated.status == "dead_letter" {
                self.emit(
                    CLUSTER_SYNC_DEAD_LETTER,
                    json!({
                        "retry_id": updated.id,
                        "plan_id": plan_id,
                        "cluster_id": cluster_id,
                        "domain": domain,
                        "attempt": updated.attempt,
                        "last_error": error,
                    }),
                );
                self.request_notification(
                    "Cluster sync dead-letter",
                    &format!(
                        "Sync of domain {domain} for plan {plan_id} failed {} times and is now dead-letter: {error}",
                        updated.attempt
                    ),
                    "cluster.sync.dead_letter",
                );
            } else {
                self.emit(
                    CLUSTER_SYNC_RETRY_QUEUED,
                    json!({
                        "retry_id": updated.id,
                        "plan_id": plan_id,
                        "cluster_id": cluster_id,
                        "domain": domain,
                        "attempt": updated.attempt,
                        "next_retry_at": updated.next_retry_at,
                    }),
                );
            }
            return updated;
        }
        let attempt: u32 = 1;
        let entry = SyncRetryEntry {
            id: Uuid::now_v7(),
            plan_id,
            cluster_id,
            node_id,
            domain: domain.to_string(),
            direction: direction.to_string(),
            attempt,
            max_attempts: settings.retry_max_attempts,
            next_retry_at: now
                + chrono::Duration::seconds(settings.retry_delay_secs(attempt) as i64),
            last_error: Some(error.to_string()),
            status: if attempt >= settings.retry_max_attempts {
                "dead_letter".to_string()
            } else {
                "pending".to_string()
            },
            created_at: now,
            updated_at: now,
        };
        queue.push(entry.clone());
        if queue.len() > settings.max_retries {
            let excess = queue.len() - settings.max_retries;
            queue.drain(..excess);
        }
        drop(queue);
        if entry.status == "dead_letter" {
            self.emit(
                CLUSTER_SYNC_DEAD_LETTER,
                json!({
                    "retry_id": entry.id,
                    "plan_id": plan_id,
                    "cluster_id": cluster_id,
                    "domain": domain,
                    "attempt": entry.attempt,
                    "last_error": error,
                }),
            );
            self.request_notification(
                "Cluster sync dead-letter",
                &format!(
                    "Sync of domain {domain} for plan {plan_id} failed {} times and is now dead-letter: {error}",
                    entry.attempt
                ),
                "cluster.sync.dead_letter",
            );
        } else {
            self.emit(
                CLUSTER_SYNC_RETRY_QUEUED,
                json!({
                    "retry_id": entry.id,
                    "plan_id": plan_id,
                    "cluster_id": cluster_id,
                    "domain": domain,
                    "attempt": entry.attempt,
                    "next_retry_at": entry.next_retry_at,
                }),
            );
        }
        entry
    }

    /// Mark the pending retry entry for a plan/domain completed after a
    /// successful run, so a later failure starts a fresh attempt count.
    fn complete_retry(&self, plan_id: Uuid, domain: &str) {
        let mut queue = self.retries.write().expect("retries lock poisoned");
        for entry in queue.iter_mut() {
            if entry.plan_id == plan_id
                && entry.domain == domain
                && matches!(entry.status.as_str(), "pending" | "retrying")
            {
                entry.status = "completed".to_string();
                entry.updated_at = now();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Node-to-node HMAC authentication.
//
// Signature input: `{unix_timestamp}.{nonce}.{sha256_hex(body)}`, signed
// with HMAC-SHA256 keyed by the raw bytes of the stored secret hash
// (SHA-256 of the secret). Both sides derive the same key: the sender
// from the secret it was given at registration, the receiver from the
// stored hash. Requests older than the replay window or with a reused
// nonce are rejected. Comparison is constant-time.
// ---------------------------------------------------------------------------

fn sign_request(secret_hash_hex: &str, timestamp: i64, nonce: &str, body: &[u8]) -> String {
    let key = hex_decode(secret_hash_hex).unwrap_or_default();
    let body_hash = sha256_hex(body);
    let canonical = format!("{timestamp}.{nonce}.{body_hash}");
    let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC key must accept any length");
    mac.update(canonical.as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

fn header_value(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Validate the HMAC signature on an incoming node-to-node request and
/// return the authenticated node. Consumes the nonce on success.
fn verify_node_request(
    state: &Arc<ClustersState>,
    req: &HttpRequest,
    body: &[u8],
) -> UnifiedResult<NodeState> {
    let node_id_text = header_value(req, HEADER_NODE_ID)
        .ok_or_else(|| UnifiedError::unauthorized("Missing node identity header"))?;
    let node_id: Uuid = node_id_text
        .parse()
        .map_err(|_| UnifiedError::unauthorized("Invalid node identity header"))?;
    let timestamp_text = header_value(req, HEADER_TIMESTAMP)
        .ok_or_else(|| UnifiedError::unauthorized("Missing request timestamp header"))?;
    let timestamp: i64 = timestamp_text
        .parse()
        .map_err(|_| UnifiedError::unauthorized("Invalid request timestamp header"))?;
    let nonce = header_value(req, HEADER_NONCE)
        .ok_or_else(|| UnifiedError::unauthorized("Missing request nonce header"))?;
    if nonce.trim().is_empty() || nonce.len() > 128 {
        return Err(UnifiedError::unauthorized("Invalid request nonce header"));
    }
    let signature_text = header_value(req, HEADER_SIGNATURE)
        .ok_or_else(|| UnifiedError::unauthorized("Missing request signature header"))?;

    let node = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&node_id)
        .cloned()
        .ok_or_else(|| UnifiedError::unauthorized("Unknown node"))?;
    if node.status == "revoked" {
        return Err(UnifiedError::unauthorized("Node credentials have been revoked"));
    }

    let now_unix = Utc::now().timestamp();
    if (now_unix - timestamp).abs() > state.settings().replay_window_secs {
        return Err(UnifiedError::unauthorized("Request timestamp outside the replay window"));
    }

    {
        let mut nonces = state.used_nonces.write().expect("nonce lock poisoned");
        let cutoff = Utc::now() - chrono::Duration::seconds(state.settings().replay_window_secs);
        nonces.retain(|_, seen| *seen > cutoff);
        if nonces.contains_key(&nonce) {
            return Err(UnifiedError::unauthorized("Request nonce has already been used"));
        }
        nonces.insert(nonce.clone(), Utc::now());
    }

    let expected = sign_request(&node.secret_hash, timestamp, &nonce, body);
    let expected_bytes = hex_decode(&expected).unwrap_or_default();
    let provided_bytes = hex_decode(&signature_text)
        .ok_or_else(|| UnifiedError::unauthorized("Invalid request signature header"))?;
    if !constant_time_eq(&expected_bytes, &provided_bytes) {
        return Err(UnifiedError::unauthorized("Invalid request signature"));
    }
    Ok(node)
}

fn join_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

// ---------------------------------------------------------------------------
// Health probing.
// ---------------------------------------------------------------------------

async fn probe_node(state: &Arc<ClustersState>, node_id: Uuid) -> UnifiedResult<HealthCheckEntry> {
    let node = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&node_id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
    if node.status == "revoked" {
        return Err(UnifiedError::unprocessable("Node credentials have been revoked"));
    }
    let url = join_url(&node.base_url, "/api/v1/clusters/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(state.settings().health_timeout_secs))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Failed to build HTTP client: {error}")))?;
    let started = Instant::now();
    let outcome: Result<u16, String> = match client.get(&url).send().await {
        Ok(response) => Ok(response.status().as_u16()),
        Err(error) => Err(error.to_string()),
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    let checked_at = now();
    let (status, error_text) = match &outcome {
        Ok(code) if (200..300).contains(code) => ("healthy".to_string(), None),
        Ok(code) => (
            "degraded".to_string(),
            Some(format!("Health endpoint returned HTTP {code}")),
        ),
        Err(message) => ("unhealthy".to_string(), Some(message.clone())),
    };

    let previous_health;
    let failures;
    {
        let mut nodes = state.nodes.write().expect("nodes lock poisoned");
        let Some(entry) = nodes.get_mut(&node_id) else {
            return Err(UnifiedError::not_found("Node not found"));
        };
        previous_health = entry.health.clone();
        if status == "healthy" {
            entry.consecutive_failures = 0;
            entry.health = "healthy".to_string();
            entry.last_seen_at = Some(checked_at);
            entry.last_latency_ms = Some(latency_ms);
        } else {
            entry.consecutive_failures += 1;
            if entry.consecutive_failures >= state.settings().health_failure_threshold {
                entry.health = "unhealthy".to_string();
            } else {
                entry.health = "degraded".to_string();
            }
            entry.last_latency_ms = Some(latency_ms);
        }
        failures = entry.consecutive_failures;
    }
    let final_status = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&node_id)
        .map(|entry| entry.health.clone())
        .unwrap_or(status.clone());

    let record = HealthCheckEntry {
        id: Uuid::now_v7(),
        node_id,
        cluster_id: node.cluster_id,
        status: final_status.clone(),
        latency_ms: Some(latency_ms),
        error: error_text,
        checked_at,
    };
    state.push_health(record.clone());
    if final_status != previous_health {
        state.emit(
            CLUSTER_HEALTH_CHANGED,
            json!({
                "node_id": node_id,
                "cluster_id": node.cluster_id,
                "previous_health": previous_health,
                "health": final_status,
                "consecutive_failures": failures,
                "latency_ms": latency_ms,
                "checked_at": checked_at,
            }),
        );
    }
    Ok(record)
}

// ---------------------------------------------------------------------------
// Conflict handling: last-write-wins on `updated_at`, never silent.
// ---------------------------------------------------------------------------

/// Apply incoming items to the local domain store. Returns
/// (applied, skipped, conflicts). A conflict is recorded whenever both
/// sides hold different payloads and neither timestamp wins cleanly, or
/// the local version is newer and must be kept.
fn apply_incoming_items(
    state: &Arc<ClustersState>,
    domain: &str,
    items: &[SyncItem],
) -> (u64, u64, Vec<SyncConflict>) {
    let mut applied: u64 = 0;
    let mut skipped: u64 = 0;
    let mut conflicts: Vec<SyncConflict> = Vec::new();
    let mut store = state.local_items.write().expect("items lock poisoned");
    let domain_store = store.entry(domain.to_string()).or_default();
    for incoming in items {
        match domain_store.get(&incoming.key) {
            None => {
                domain_store.insert(incoming.key.clone(), incoming.clone());
                applied += 1;
            }
            Some(local) => {
                if local.payload == incoming.payload {
                    skipped += 1;
                } else if incoming.updated_at > local.updated_at {
                    domain_store.insert(incoming.key.clone(), incoming.clone());
                    applied += 1;
                } else if local.updated_at > incoming.updated_at {
                    conflicts.push(SyncConflict {
                        domain: domain.to_string(),
                        key: incoming.key.clone(),
                        local_updated_at: Some(local.updated_at),
                        remote_updated_at: Some(incoming.updated_at),
                        winner: "local".to_string(),
                        reason: "Local version is newer; incoming version was not applied".to_string(),
                    });
                    skipped += 1;
                } else {
                    // Equal timestamps, different payloads: deterministic
                    // tie-break on the payload text so both sides agree.
                    let local_text = serde_json::to_string(&local.payload).unwrap_or_default();
                    let remote_text =
                        serde_json::to_string(&incoming.payload).unwrap_or_default();
                    if remote_text > local_text {
                        domain_store.insert(incoming.key.clone(), incoming.clone());
                        applied += 1;
                        conflicts.push(SyncConflict {
                            domain: domain.to_string(),
                            key: incoming.key.clone(),
                            local_updated_at: Some(local.updated_at),
                            remote_updated_at: Some(incoming.updated_at),
                            winner: "remote".to_string(),
                            reason: "Equal timestamps with different payloads; lexicographically larger payload won".to_string(),
                        });
                    } else {
                        conflicts.push(SyncConflict {
                            domain: domain.to_string(),
                            key: incoming.key.clone(),
                            local_updated_at: Some(local.updated_at),
                            remote_updated_at: Some(incoming.updated_at),
                            winner: "local".to_string(),
                            reason: "Equal timestamps with different payloads; lexicographically larger payload won".to_string(),
                        });
                        skipped += 1;
                    }
                }
            }
        }
    }
    (applied, skipped, conflicts)
}

/// Compare local items against remote items without changing anything.
/// Returns (would_send, would_receive, conflicts).
fn compare_items(local: &[SyncItem], remote: &[SyncItem]) -> (u64, u64, Vec<SyncConflict>) {
    let mut would_send: u64 = 0;
    let mut would_receive: u64 = 0;
    let mut conflicts: Vec<SyncConflict> = Vec::new();
    let remote_by_key: HashMap<&str, &SyncItem> = remote
        .iter()
        .map(|item| (item.key.as_str(), item))
        .collect();
    let local_by_key: HashMap<&str, &SyncItem> = local
        .iter()
        .map(|item| (item.key.as_str(), item))
        .collect();
    for item in local {
        match remote_by_key.get(item.key.as_str()) {
            None => would_send += 1,
            Some(other) => {
                if item.payload != other.payload {
                    if item.updated_at > other.updated_at {
                        would_send += 1;
                    } else if other.updated_at > item.updated_at {
                        would_receive += 1;
                    } else {
                        conflicts.push(SyncConflict {
                            domain: item.domain.clone(),
                            key: item.key.clone(),
                            local_updated_at: Some(item.updated_at),
                            remote_updated_at: Some(other.updated_at),
                            winner: "undecided".to_string(),
                            reason: "Dry run: equal timestamps with different payloads; no version applied".to_string(),
                        });
                    }
                }
            }
        }
    }
    for item in remote {
        if !local_by_key.contains_key(item.key.as_str()) {
            would_receive += 1;
        }
    }
    (would_send, would_receive, conflicts)
}

fn local_items_for_domain(state: &Arc<ClustersState>, domain: &str) -> Vec<SyncItem> {
    state
        .local_items
        .read()
        .expect("items lock poisoned")
        .get(domain)
        .map(|entries| entries.values().cloned().collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Sync execution.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct SyncRunSummary {
    plan_id: Uuid,
    cluster_id: Uuid,
    direction: String,
    dry_run: bool,
    domains: Vec<SyncDomainSummary>,
    conflicts: Vec<SyncConflict>,
    errors: Vec<String>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct SyncDomainSummary {
    domain: String,
    status: String,
    items_sent: u64,
    items_received: u64,
    items_applied: u64,
    items_skipped: u64,
    conflicts: u64,
}

fn signed_headers(
    _state: &Arc<ClustersState>,
    plan: &SyncPlanState,
    body: &[u8],
) -> UnifiedResult<Vec<(String, String)>> {
    let timestamp = Utc::now().timestamp();
    let nonce = Uuid::new_v4().to_string();
    let signature = sign_request(&plan.peer_secret_hash, timestamp, &nonce, body);
    Ok(vec![
        (HEADER_NODE_ID.to_string(), plan.local_node_id.clone()),
        (HEADER_TIMESTAMP.to_string(), timestamp.to_string()),
        (HEADER_NONCE.to_string(), nonce),
        (HEADER_SIGNATURE.to_string(), signature),
    ])
}

fn http_client(timeout_secs: u64) -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Failed to build HTTP client: {error}")))
}

/// Fetch the peer's export for one domain (signed GET).
async fn fetch_remote_export(
    state: &Arc<ClustersState>,
    plan: &SyncPlanState,
    node: &NodeState,
    domain: &str,
    since: Option<DateTime<Utc>>,
) -> UnifiedResult<Vec<SyncItem>> {
    let mut url = join_url(
        &node.base_url,
        &format!("/api/v1/clusters/sync/export?domain={domain}"),
    );
    if let Some(since) = since {
        url.push_str(&format!("&since={}", since.to_rfc3339()));
    }
    let client = http_client(state.settings().sync_timeout_secs)?;
    let headers = signed_headers(state, plan, &[])?;
    let mut request = client.get(&url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| UnifiedError::internal(format!("Sync export request failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(UnifiedError::internal(format!(
            "Peer export request failed with HTTP {}: {}",
            status.as_u16(),
            text.chars().take(300).collect::<String>()
        )));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| UnifiedError::internal(format!("Peer export response was not JSON: {error}")))?;
    let items = body
        .get("data")
        .and_then(|data| data.get("items"))
        .and_then(|items| serde_json::from_value::<Vec<SyncItem>>(items.clone()).ok())
        .ok_or_else(|| UnifiedError::internal("Peer export response has no data.items array"))?;
    Ok(items)
}

/// Push one domain's local items to the peer.
async fn push_domain(
    state: &Arc<ClustersState>,
    plan: &SyncPlanState,
    node: &NodeState,
    domain: &str,
) -> (u64, u64, u64, Vec<String>) {
    let items = local_items_for_domain(state, domain);
    let sent = items.len() as u64;
    let envelope = json!({
        "version": SYNC_ENVELOPE_VERSION,
        "cluster_id": plan.cluster_id,
        "plan_id": plan.id,
        "domain": domain,
        "direction": "push",
        "items": items,
        "sent_at": Utc::now(),
    });
    let body = serde_json::to_vec(&envelope).unwrap_or_default();
    let client = match http_client(state.settings().sync_timeout_secs) {
        Ok(client) => client,
        Err(error) => return (0, 0, 0, vec![error.message]),
    };
    let headers = match signed_headers(state, plan, &body) {
        Ok(headers) => headers,
        Err(error) => return (0, 0, 0, vec![error.message]),
    };
    let url = join_url(&node.base_url, "/api/v1/clusters/sync/receive");
    let mut request = client.post(&url).body(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return (
                    0,
                    0,
                    0,
                    vec![format!(
                        "Peer rejected sync push with HTTP {}: {}",
                        status.as_u16(),
                        text.chars().take(300).collect::<String>()
                    )],
                );
            }
            match response.json::<Value>().await {
                Ok(value) => {
                    let data = value.get("data").cloned().unwrap_or(Value::Null);
                    let applied = data.get("applied").and_then(|v| v.as_u64()).unwrap_or(0);
                    let skipped = data.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
                    (sent, applied, skipped, Vec::new())
                }
                Err(error) => (
                    sent,
                    0,
                    0,
                    vec![format!("Peer sync response was not JSON: {error}")],
                ),
            }
        }
        Err(error) => (0, 0, 0, vec![format!("Sync push request failed: {error}")]),
    }
}

/// Run one sync plan (manual or scheduled). Records one sync log entry
/// per domain, emits lifecycle hooks, and raises a notification plus a
/// failure hook whenever a domain fails.
async fn execute_sync(
    state: &Arc<ClustersState>,
    plan_id: Uuid,
    dry_run: bool,
) -> UnifiedResult<SyncRunSummary> {
    let started_at = now();
    let plan = state
        .plans
        .read()
        .expect("plans lock poisoned")
        .get(&plan_id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync plan not found"))?;
    if !plan.enabled {
        return Err(UnifiedError::unprocessable("Sync plan is disabled"));
    }
    if plan.peer_secret_hash.is_empty() {
        return Err(UnifiedError::unprocessable(
            "Sync plan has no peer credential; recreate the plan with a peer secret",
        ));
    }
    let node = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&plan.target_node_id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Target node not found"))?;
    if node.status == "revoked" {
        return Err(UnifiedError::unprocessable("Target node credentials have been revoked"));
    }
    if !matches!(plan.direction.as_str(), "push" | "pull" | "bidirectional") {
        return Err(UnifiedError::unprocessable(format!(
            "Unknown sync direction: {}",
            plan.direction
        )));
    }

    state.emit(
        CLUSTER_SYNC_STARTED,
        json!({
            "plan_id": plan.id,
            "cluster_id": plan.cluster_id,
            "node_id": node.id,
            "direction": plan.direction,
            "domains": plan.domains,
            "dry_run": dry_run,
            "started_at": started_at,
        }),
    );

    let mut domain_summaries: Vec<SyncDomainSummary> = Vec::new();
    let mut all_conflicts: Vec<SyncConflict> = Vec::new();
    let mut all_errors: Vec<String> = Vec::new();
    let mut any_failed = false;

    for domain in &plan.domains {
        let domain_started = now();
        let mut summary = SyncDomainSummary {
            domain: domain.clone(),
            status: "completed".to_string(),
            items_sent: 0,
            items_received: 0,
            items_applied: 0,
            items_skipped: 0,
            conflicts: 0,
        };
        let mut domain_errors: Vec<String> = Vec::new();
        let mut domain_conflicts: Vec<SyncConflict> = Vec::new();
        let mut local_payloads: HashMap<String, Value> = HashMap::new();
        let mut remote_payloads: HashMap<String, Value> = HashMap::new();

        if dry_run {
            match fetch_remote_export(state, &plan, &node, domain, None).await {
                Ok(remote) => {
                    let local = local_items_for_domain(state, domain);
                    let (would_send, would_receive, conflicts) = compare_items(&local, &remote);
                    summary.items_sent = would_send;
                    summary.items_received = would_receive;
                    domain_conflicts = conflicts;
                }
                Err(error) => {
                    domain_errors.push(error.message.clone());
                    summary.status = "failed".to_string();
                    any_failed = true;
                }
            }
        } else {
            if plan.direction == "push" || plan.direction == "bidirectional" {
                let (sent, applied, skipped, errors) =
                    push_domain(state, &plan, &node, domain).await;
                summary.items_sent += sent;
                summary.items_applied += applied;
                summary.items_skipped += skipped;
                if !errors.is_empty() {
                    domain_errors.extend(errors);
                    summary.status = "failed".to_string();
                    any_failed = true;
                }
            }
            if plan.direction == "pull" || plan.direction == "bidirectional" {
                match fetch_remote_export(state, &plan, &node, domain, plan.last_success_at).await {
                    Ok(remote) => {
                        summary.items_received += remote.len() as u64;
                        local_payloads = local_items_for_domain(state, domain)
                            .into_iter()
                            .map(|item| (item.key, item.payload))
                            .collect();
                        for item in &remote {
                            remote_payloads.insert(item.key.clone(), item.payload.clone());
                        }
                        let (applied, skipped, conflicts) =
                            apply_incoming_items(state, domain, &remote);
                        summary.items_applied += applied;
                        summary.items_skipped += skipped;
                        domain_conflicts.extend(conflicts);
                    }
                    Err(error) => {
                        domain_errors.push(error.message.clone());
                        summary.status = "failed".to_string();
                        any_failed = true;
                    }
                }
            }
        }

        summary.conflicts = domain_conflicts.len() as u64;
        all_conflicts.extend(domain_conflicts.clone());
        all_errors.extend(domain_errors.clone());
        let finished_at = now();
        let log_id = Uuid::now_v7();
        // Real (non-dry-run) conflicts become first-class records an
        // admin can list and resolve; dry-run conflicts are previews
        // only and are reported in the run summary, not stored.
        if !dry_run && !domain_conflicts.is_empty() {
            state.record_conflicts(
                Some(log_id),
                Some(plan.id),
                plan.cluster_id,
                Some(node.id),
                &domain_conflicts,
                &local_payloads,
                &remote_payloads,
            );
        }
        state.push_log(SyncLogEntry {
            id: log_id,
            plan_id: Some(plan.id),
            cluster_id: plan.cluster_id,
            node_id: Some(node.id),
            direction: plan.direction.clone(),
            domain: domain.clone(),
            status: if dry_run {
                "dry_run".to_string()
            } else {
                summary.status.clone()
            },
            items_sent: summary.items_sent,
            items_received: summary.items_received,
            items_applied: summary.items_applied,
            items_skipped: summary.items_skipped,
            conflicts: domain_conflicts,
            errors: domain_errors,
            dry_run,
            started_at: domain_started,
            finished_at,
        });
        // A failed domain enters the retry queue with exponential
        // backoff; a successful run clears any pending retry for the
        // same plan and domain. Dry runs never queue retries.
        if !dry_run {
            if summary.status == "failed" {
                let error_text = if domain_errors.is_empty() {
                    "Sync failed".to_string()
                } else {
                    domain_errors.join("; ")
                };
                state.enqueue_retry(
                    plan.id,
                    plan.cluster_id,
                    Some(node.id),
                    domain,
                    &plan.direction,
                    &error_text,
                );
            } else {
                state.complete_retry(plan.id, domain);
            }
        }
        domain_summaries.push(summary);
    }

    let finished_at = now();
    {
        let mut plans = state.plans.write().expect("plans lock poisoned");
        if let Some(entry) = plans.get_mut(&plan_id) {
            entry.last_run_at = Some(finished_at);
            if !any_failed && !dry_run {
                entry.last_success_at = Some(finished_at);
            }
            entry.updated_at = finished_at;
        }
    }

    let summary = SyncRunSummary {
        plan_id: plan.id,
        cluster_id: plan.cluster_id,
        direction: plan.direction.clone(),
        dry_run,
        domains: domain_summaries.clone(),
        conflicts: all_conflicts.clone(),
        errors: all_errors.clone(),
        started_at,
        finished_at,
    };

    if !all_conflicts.is_empty() {
        state.emit(
            CLUSTER_SYNC_CONFLICT,
            json!({
                "plan_id": plan.id,
                "cluster_id": plan.cluster_id,
                "conflicts": all_conflicts,
            }),
        );
    }
    if any_failed {
        state.emit(
            CLUSTER_SYNC_FAILED,
            json!({
                "plan_id": plan.id,
                "cluster_id": plan.cluster_id,
                "node_id": node.id,
                "direction": plan.direction,
                "errors": all_errors,
                "finished_at": finished_at,
            }),
        );
        state.request_notification(
            "Cluster sync failed",
            &format!(
                "Sync plan {} for cluster {} failed: {}",
                plan.id,
                plan.cluster_id,
                all_errors.join("; ")
            ),
            "cluster.sync.failed",
        );
        return Err(UnifiedError::internal(format!(
            "Sync failed for one or more domains: {}",
            all_errors.join("; ")
        )));
    }
    state.emit(
        CLUSTER_SYNC_COMPLETED,
        json!({
            "plan_id": plan.id,
            "cluster_id": plan.cluster_id,
            "node_id": node.id,
            "direction": plan.direction,
            "dry_run": dry_run,
            "domains": domain_summaries,
            "conflicts": all_conflicts.len(),
            "finished_at": finished_at,
        }),
    );
    Ok(summary)
}

/// Spawn async work from a synchronous hook handler: use the current
/// runtime when there is one, otherwise a dedicated thread with its own
/// runtime.
fn spawn_background<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateClusterRequest {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_cluster_kind")]
    kind: String,
}

fn default_cluster_kind() -> String {
    "unified".to_string()
}

#[derive(Debug, Deserialize)]
struct UpdateClusterRequest {
    name: Option<String>,
    description: Option<String>,
    kind: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegisterNodeRequest {
    display_name: String,
    base_url: String,
    #[serde(default = "default_node_role")]
    role: String,
}

fn default_node_role() -> String {
    "member".to_string()
}

#[derive(Debug, Deserialize)]
struct CreatePlanRequest {
    cluster_id: Uuid,
    target_node_id: Uuid,
    local_node_id: String,
    #[serde(default = "default_direction")]
    direction: String,
    domains: Vec<String>,
    #[serde(default = "default_schedule")]
    schedule: String,
    /// The secret the peer gave this instance at registration. It is
    /// hashed immediately and never stored or returned.
    peer_secret: String,
}

fn default_direction() -> String {
    "bidirectional".to_string()
}

fn default_schedule() -> String {
    "3600".to_string()
}

#[derive(Debug, Deserialize)]
struct UpdatePlanRequest {
    direction: Option<String>,
    domains: Option<Vec<String>>,
    schedule: Option<String>,
    enabled: Option<bool>,
    /// A replacement peer secret; hashed immediately, never stored.
    peer_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SyncNowRequest {
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct StageItemRequest {
    domain: String,
    key: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    domain: Option<String>,
    since: Option<String>,
}

// ---------------------------------------------------------------------------
// Cluster handlers.
// ---------------------------------------------------------------------------

async fn list_clusters(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<ClusterState> = state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .values()
            .cloned()
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|cluster| {
                cluster.name.to_lowercase().contains(&needle)
                    || cluster.description.to_lowercase().contains(&needle)
            });
        }
        if let Some(kind) = params.filters.get("kind") {
            items.retain(|cluster| cluster.kind == *kind);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|cluster| cluster.status == *status);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<CreateClusterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = "clusters:create";
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<ClusterState> = (|| {
        let name = require_non_empty(&body.name, "name")?;
        if name.len() > 128 {
            return Err(UnifiedError::bad_request("name is too long"));
        }
        if body.description.len() > 1024 {
            return Err(UnifiedError::bad_request("description is too long"));
        }
        let kind = body.kind.trim();
        if !matches!(kind, "unified" | "provider_pool") {
            return Err(UnifiedError::bad_request(
                "kind must be unified or provider_pool",
            ));
        }
        let cluster = ClusterState {
            id: Uuid::now_v7(),
            name,
            description: body.description.trim().to_string(),
            kind: kind.to_string(),
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .clusters
            .write()
            .expect("clusters lock poisoned")
            .insert(cluster.id, cluster.clone());
        state.emit(
            CLUSTER_CREATED,
            json!({
                "cluster_id": cluster.id,
                "name": cluster.name,
                "kind": cluster.kind,
                "status": cluster.status,
            }),
        );
        Ok(cluster)
    })();
    match result {
        Ok(cluster) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&cluster).unwrap_or(Value::Null),
                );
            }
            json::success_response(cluster, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<ClusterState> = state
        .clusters
        .read()
        .expect("clusters lock poisoned")
        .get(&id.into_inner())
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Cluster not found"));
    json::respond(result, debug)
}

async fn update_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateClusterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<ClusterState> = (|| {
        let id = id.into_inner();
        let mut clusters = state.clusters.write().expect("clusters lock poisoned");
        let cluster = clusters
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Cluster not found"))?;
        if let Some(name) = &body.name {
            let name = require_non_empty(name, "name")?;
            if name.len() > 128 {
                return Err(UnifiedError::bad_request("name is too long"));
            }
            cluster.name = name;
        }
        if let Some(description) = &body.description {
            if description.len() > 1024 {
                return Err(UnifiedError::bad_request("description is too long"));
            }
            cluster.description = description.trim().to_string();
        }
        if let Some(kind) = &body.kind {
            let kind = kind.trim();
            if !matches!(kind, "unified" | "provider_pool") {
                return Err(UnifiedError::bad_request(
                    "kind must be unified or provider_pool",
                ));
            }
            cluster.kind = kind.to_string();
        }
        if let Some(status) = &body.status {
            let status = status.trim();
            if !matches!(status, "active" | "paused" | "archived") {
                return Err(UnifiedError::bad_request(
                    "status must be active, paused or archived",
                ));
            }
            cluster.status = status.to_string();
        }
        cluster.updated_at = now();
        let updated = cluster.clone();
        drop(clusters);
        state.emit(
            CLUSTER_UPDATED,
            json!({
                "cluster_id": updated.id,
                "name": updated.name,
                "kind": updated.kind,
                "status": updated.status,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let removed = state
            .clusters
            .write()
            .expect("clusters lock poisoned")
            .remove(&id)
            .ok_or_else(|| UnifiedError::not_found("Cluster not found"))?;
        let _ = removed;
        state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .retain(|_, node| node.cluster_id != id);
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .retain(|_, plan| plan.cluster_id != id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn cluster_sync_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let cluster = state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .get(&id)
            .cloned()
            .ok_or_else(|| UnifiedError::not_found("Cluster not found"))?;
        let plans: Vec<SyncPlanState> = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .filter(|plan| plan.cluster_id == id)
            .cloned()
            .collect();
        let logs: Vec<SyncLogEntry> = state
            .logs
            .read()
            .expect("logs lock poisoned")
            .iter()
            .filter(|log| log.cluster_id == id)
            .cloned()
            .collect();
        let last_sync_at = logs.iter().map(|log| log.finished_at).max();
        let last_success_at = logs
            .iter()
            .filter(|log| log.status == "completed")
            .map(|log| log.finished_at)
            .max();
        // Open conflicts come from the dedicated conflict store, not
        // from the rotating log, so they stay visible until resolved.
        let open_conflicts: u64 = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .filter(|conflict| conflict.cluster_id == id && conflict.status == "open")
            .count() as u64;
        let conflicts_count: u64 = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .filter(|conflict| conflict.cluster_id == id)
            .count() as u64;
        let pending_retries: u64 = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.cluster_id == id && entry.status == "pending")
            .count() as u64;
        let dead_letters: u64 = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.cluster_id == id && entry.status == "dead_letter")
            .count() as u64;
        let failures: u64 = logs
            .iter()
            .filter(|log| log.status == "failed")
            .count() as u64;
        let plan_domains: HashSet<String> = plans
            .iter()
            .flat_map(|plan| plan.domains.iter().cloned())
            .collect();
        let pending_items: u64 = state
            .local_items
            .read()
            .expect("items lock poisoned")
            .iter()
            .filter(|(domain, _)| plan_domains.contains(*domain))
            .flat_map(|(_, entries)| entries.values())
            .filter(|item| last_success_at.map_or(true, |since| item.updated_at > since))
            .count() as u64;
        let plan_summaries: Vec<Value> = plans
            .iter()
            .map(|plan| {
                json!({
                    "plan_id": plan.id,
                    "target_node_id": plan.target_node_id,
                    "direction": plan.direction,
                    "domains": plan.domains,
                    "enabled": plan.enabled,
                    "last_run_at": plan.last_run_at,
                    "last_success_at": plan.last_success_at,
                })
            })
            .collect();
        Ok(json!({
            "cluster_id": cluster.id,
            "cluster_name": cluster.name,
            "status": cluster.status,
            "last_sync_at": last_sync_at,
            "last_success_at": last_success_at,
            "pending_items": pending_items,
            "conflicts_count": conflicts_count,
            "open_conflicts": open_conflicts,
            "pending_retries": pending_retries,
            "dead_letters": dead_letters,
            "failures": failures,
            "plans": plan_summaries,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Node handlers.
// ---------------------------------------------------------------------------

async fn register_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    cluster_id: web::Path<Uuid>,
    body: web::Json<RegisterNodeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = format!(
        "clusters:{}:nodes:register",
        req.match_info().get("id").unwrap_or_default()
    );
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<Value> = (|| {
        let cluster_id = cluster_id.into_inner();
        if !state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .contains_key(&cluster_id)
        {
            return Err(UnifiedError::not_found("Cluster not found"));
        }
        let display_name = require_non_empty(&body.display_name, "display_name")?;
        if display_name.len() > 128 {
            return Err(UnifiedError::bad_request("display_name is too long"));
        }
        let base_url = validate_base_url(&body.base_url)?;
        let role = body.role.trim();
        if !matches!(role, "primary" | "member" | "edge") {
            return Err(UnifiedError::bad_request("role must be primary, member or edge"));
        }
        {
            let nodes = state.nodes.read().expect("nodes lock poisoned");
            if nodes.values().any(|node| {
                node.cluster_id == cluster_id
                    && node.base_url == base_url
                    && node.status == "active"
            }) {
                return Err(UnifiedError::conflict(
                    "An active node with the same base_url already exists in this cluster",
                ));
            }
            if role == "primary"
                && nodes.values().any(|node| {
                    node.cluster_id == cluster_id
                        && node.role == "primary"
                        && node.status == "active"
                })
            {
                return Err(UnifiedError::conflict(
                    "This cluster already has a primary node",
                ));
            }
        }
        // The secret is returned exactly once, in this response. Only its
        // hash is stored, and it is never logged.
        let secret = generate_secret();
        let secret_hash = hash_secret(&secret);
        let node = NodeState {
            id: Uuid::now_v7(),
            cluster_id,
            display_name,
            base_url,
            role: role.to_string(),
            status: "active".to_string(),
            secret_fingerprint: secret_fingerprint(&secret_hash),
            secret_hash,
            health: "unknown".to_string(),
            consecutive_failures: 0,
            last_seen_at: None,
            last_latency_ms: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .insert(node.id, node.clone());
        state.emit(
            CLUSTER_NODE_REGISTERED,
            json!({
                "node_id": node.id,
                "cluster_id": cluster_id,
                "display_name": node.display_name,
                "role": node.role,
            }),
        );
        Ok(json!({ "node": node, "secret": secret }))
    })();
    match result {
        Ok(value) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    &idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&value).unwrap_or(Value::Null),
                );
            }
            json::success_response(value, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_nodes(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    cluster_id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let cluster_id = cluster_id.into_inner();
        if !state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .contains_key(&cluster_id)
        {
            return Err(UnifiedError::not_found("Cluster not found"));
        }
        let mut items: Vec<NodeState> = state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .values()
            .filter(|node| node.cluster_id == cluster_id)
            .cloned()
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|node| {
                node.display_name.to_lowercase().contains(&needle)
                    || node.base_url.to_lowercase().contains(&needle)
            });
        }
        if let Some(role) = params.filters.get("role") {
            items.retain(|node| node.role == *role);
        }
        if let Some(health) = params.filters.get("health") {
            items.retain(|node| node.health == *health);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|node| node.status == *status);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<NodeState> = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&id.into_inner())
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Node not found"));
    json::respond(result, debug)
}

async fn revoke_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<NodeState> = (|| {
        let id = id.into_inner();
        let mut nodes = state.nodes.write().expect("nodes lock poisoned");
        let node = nodes
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
        node.status = "revoked".to_string();
        node.health = "unknown".to_string();
        node.updated_at = now();
        let updated = node.clone();
        drop(nodes);
        state.emit(
            CLUSTER_NODE_REVOKED,
            json!({
                "node_id": updated.id,
                "cluster_id": updated.cluster_id,
                "display_name": updated.display_name,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn remove_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let removed = state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .remove(&id)
            .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .retain(|_, plan| plan.target_node_id != id);
        state.emit(
            CLUSTER_NODE_REVOKED,
            json!({
                "node_id": removed.id,
                "cluster_id": removed.cluster_id,
                "display_name": removed.display_name,
                "removed": true,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn rotate_node_secret(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let mut nodes = state.nodes.write().expect("nodes lock poisoned");
        let node = nodes
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
        if node.status == "revoked" {
            return Err(UnifiedError::unprocessable("Node credentials have been revoked"));
        }
        // The new secret is returned exactly once, in this response.
        // Only its hash is stored.
        let secret = generate_secret();
        let secret_hash = hash_secret(&secret);
        node.secret_hash = secret_hash;
        node.secret_fingerprint = secret_fingerprint(&node.secret_hash);
        node.updated_at = now();
        let updated = node.clone();
        drop(nodes);
        Ok(json!({ "node": updated, "secret": secret }))
    })();
    json::respond(result, debug)
}

async fn health_check_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<HealthCheckEntry> = probe_node(&state, id.into_inner()).await;
    json::respond(result, debug)
}

async fn node_health_history(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if !state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .contains_key(&id)
        {
            return Err(UnifiedError::not_found("Node not found"));
        }
        let mut items: Vec<HealthCheckEntry> = state
            .health_history
            .read()
            .expect("health lock poisoned")
            .iter()
            .filter(|entry| entry.node_id == id)
            .cloned()
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|entry| entry.status == *status);
        }
        items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Lightweight probe endpoint other Unified instances call when they
/// health-check this node. No authentication: it reveals nothing but
/// liveness.
async fn peer_health(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        Ok::<Value, UnifiedError>(json!({
            "status": "ok",
            "module": "clusters",
            "time": Utc::now(),
            "uptime_seconds": ctx.uptime_seconds(),
        })),
        debug,
    )
}

// ---------------------------------------------------------------------------
// Sync plan handlers.
// ---------------------------------------------------------------------------

async fn list_plans(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<SyncPlanState> = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|plan| plan.cluster_id == wanted);
        }
        if let Some(direction) = params.filters.get("direction") {
            items.retain(|plan| plan.direction == *direction);
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true" || enabled == "1";
            items.retain(|plan| plan.enabled == wanted);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = "clusters:plans:create";
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<SyncPlanState> = (|| {
        if !state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .contains_key(&body.cluster_id)
        {
            return Err(UnifiedError::not_found("Cluster not found"));
        }
        let target = state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .get(&body.target_node_id)
            .cloned()
            .ok_or_else(|| UnifiedError::not_found("Target node not found"))?;
        if target.cluster_id != body.cluster_id {
            return Err(UnifiedError::unprocessable(
                "Target node does not belong to this cluster",
            ));
        }
        if target.status == "revoked" {
            return Err(UnifiedError::unprocessable("Target node credentials have been revoked"));
        }
        let local_node_id = require_non_empty(&body.local_node_id, "local_node_id")?;
        if local_node_id.len() > 128 {
            return Err(UnifiedError::bad_request("local_node_id is too long"));
        }
        let direction = body.direction.trim();
        if !matches!(direction, "push" | "pull" | "bidirectional") {
            return Err(UnifiedError::bad_request(
                "direction must be push, pull or bidirectional",
            ));
        }
        let domains = validate_domains(&state, body.domains.clone())?;
        if !valid_schedule(&body.schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
            ));
        }
        if body.peer_secret.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "peer_secret is required: the secret the peer gave this instance at registration",
            ));
        }
        // Hash immediately; the raw secret is never stored or returned.
        let peer_secret_hash = hash_secret(body.peer_secret.trim());
        let plan = SyncPlanState {
            id: Uuid::now_v7(),
            cluster_id: body.cluster_id,
            target_node_id: body.target_node_id,
            local_node_id,
            direction: direction.to_string(),
            domains,
            schedule: body.schedule.trim().to_string(),
            enabled: true,
            peer_secret_fingerprint: Some(secret_fingerprint(&peer_secret_hash)),
            peer_secret_hash,
            last_run_at: None,
            last_success_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .insert(plan.id, plan.clone());
        Ok(plan)
    })();
    match result {
        Ok(plan) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&plan).unwrap_or(Value::Null),
                );
            }
            json::success_response(plan, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncPlanState> = state
        .plans
        .read()
        .expect("plans lock poisoned")
        .get(&id.into_inner())
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync plan not found"));
    json::respond(result, debug)
}

async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncPlanState> = (|| {
        let id = id.into_inner();
        let mut plans = state.plans.write().expect("plans lock poisoned");
        let plan = plans
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Sync plan not found"))?;
        if let Some(direction) = &body.direction {
            let direction = direction.trim();
            if !matches!(direction, "push" | "pull" | "bidirectional") {
                return Err(UnifiedError::bad_request(
                    "direction must be push, pull or bidirectional",
                ));
            }
            plan.direction = direction.to_string();
        }
        if let Some(domains) = &body.domains {
            plan.domains = validate_domains(&state, domains.clone())?;
        }
        if let Some(schedule) = &body.schedule {
            if !valid_schedule(schedule) {
                return Err(UnifiedError::bad_request(
                    "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
                ));
            }
            plan.schedule = schedule.trim().to_string();
        }
        if let Some(enabled) = body.enabled {
            plan.enabled = enabled;
        }
        if let Some(secret) = &body.peer_secret {
            if secret.trim().is_empty() {
                return Err(UnifiedError::bad_request("peer_secret must not be empty"));
            }
            let hash = hash_secret(secret.trim());
            plan.peer_secret_fingerprint = Some(secret_fingerprint(&hash));
            plan.peer_secret_hash = hash;
        }
        plan.updated_at = now();
        Ok(plan.clone())
    })();
    json::respond(result, debug)
}

async fn delete_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let plan_id = id.into_inner();
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .remove(&plan_id)
            .map(|_| json!({ "deleted": true, "id": plan_id }))
            .ok_or_else(|| UnifiedError::not_found("Sync plan not found"))
    })();
    json::respond(result, debug)
}

async fn sync_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<SyncNowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = format!(
        "clusters:sync:run:{}",
        req.match_info().get("id").unwrap_or_default()
    );
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<SyncRunSummary> =
        execute_sync(&state, id.into_inner(), body.dry_run).await;
    // Only successful runs are stored: a failed run must be retried
    // for real, never answered from a stale stored response.
    if let Ok(summary) = &result {
        if let Some(key) = &idempotency_key {
            state.idempotency_store(
                &idempotency_scope,
                key,
                200,
                serde_json::to_value(summary).unwrap_or(Value::Null),
            );
        }
    }
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Sync log handlers.
// ---------------------------------------------------------------------------

async fn list_sync_logs(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<SyncLogEntry> = state
            .logs
            .read()
            .expect("logs lock poisoned")
            .iter()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|log| log.cluster_id == wanted);
        }
        if let Some(plan_id) = params.filters.get("plan_id") {
            let wanted: Uuid = plan_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("plan_id must be a UUID"))?;
            items.retain(|log| log.plan_id == Some(wanted));
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|log| log.status == *status);
        }
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|log| log.domain == *domain);
        }
        if let Some(direction) = params.filters.get("direction") {
            items.retain(|log| log.direction == *direction);
        }
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_sync_log(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: UnifiedResult<SyncLogEntry> = state
        .logs
        .read()
        .expect("logs lock poisoned")
        .iter()
        .find(|log| log.id == id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync log entry not found"));
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Node-to-node sync transport: receive and export.
// ---------------------------------------------------------------------------

async fn sync_receive(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ClustersState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let sender = verify_node_request(&state, &req, &body)?;
        let envelope: Value = serde_json::from_slice(&body)
            .map_err(|_| UnifiedError::bad_request("Sync envelope is not valid JSON"))?;
        let version = envelope
            .get("version")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no version"))?;
        if version != u64::from(SYNC_ENVELOPE_VERSION) {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported sync envelope version: {version}"
            )));
        }
        let domain = envelope
            .get("domain")
            .and_then(|value| value.as_str())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no domain"))?;
        let domain = validate_domain(&state, domain)?;
        let cluster_id = envelope
            .get("cluster_id")
            .and_then(|value| value.as_str())
            .and_then(|text| text.parse::<Uuid>().ok())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no cluster_id"))?;
        if sender.cluster_id != cluster_id {
            return Err(UnifiedError::forbidden(
                "Sending node does not belong to the envelope cluster",
            ));
        }
        let items: Vec<SyncItem> = envelope
            .get("items")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no items array"))?;
        let started_at = now();
        let received = items.len() as u64;
        let local_payloads: HashMap<String, Value> = local_items_for_domain(&state, &domain)
            .into_iter()
            .map(|item| (item.key, item.payload))
            .collect();
        let remote_payloads: HashMap<String, Value> = items
            .iter()
            .map(|item| (item.key.clone(), item.payload.clone()))
            .collect();
        let (applied, skipped, conflicts) = apply_incoming_items(&state, &domain, &items);
        let finished_at = now();
        let log_id = Uuid::now_v7();
        let plan_id = envelope
            .get("plan_id")
            .and_then(|value| value.as_str())
            .and_then(|text| text.parse::<Uuid>().ok());
        if !conflicts.is_empty() {
            state.record_conflicts(
                Some(log_id),
                plan_id,
                cluster_id,
                Some(sender.id),
                &conflicts,
                &local_payloads,
                &remote_payloads,
            );
        }
        state.push_log(SyncLogEntry {
            id: log_id,
            plan_id,
            cluster_id,
            node_id: Some(sender.id),
            direction: "push".to_string(),
            domain: domain.clone(),
            status: "completed".to_string(),
            items_sent: 0,
            items_received: received,
            items_applied: applied,
            items_skipped: skipped,
            conflicts: conflicts.clone(),
            errors: Vec::new(),
            dry_run: false,
            started_at,
            finished_at,
        });
        if !conflicts.is_empty() {
            state.emit(
                CLUSTER_SYNC_CONFLICT,
                json!({
                    "cluster_id": cluster_id,
                    "node_id": sender.id,
                    "domain": domain,
                    "conflicts": conflicts,
                }),
            );
        }
        state.emit(
            CLUSTER_SYNC_COMPLETED,
            json!({
                "cluster_id": cluster_id,
                "node_id": sender.id,
                "domain": domain,
                "direction": "push",
                "received": received,
                "applied": applied,
                "skipped": skipped,
                "conflicts": conflicts.len(),
                "finished_at": finished_at,
            }),
        );
        Ok(json!({
            "received": received,
            "applied": applied,
            "skipped": skipped,
            "conflicts": conflicts,
        }))
    })();
    json::respond(result, debug)
}

async fn sync_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ClustersState>>,
    req: HttpRequest,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The export request is signed over an empty body.
    let result: UnifiedResult<Value> = (|| {
        let requester = verify_node_request(&state, &req, &[])?;
        let domain = query
            .domain
            .as_deref()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter domain is required"))?;
        let domain = validate_domain(&state, domain)?;
        let since: Option<DateTime<Utc>> = match &query.since {
            Some(text) => Some(
                text.parse::<DateTime<Utc>>().map_err(|_| {
                    UnifiedError::bad_request("since must be an RFC 3339 timestamp")
                })?,
            ),
            None => None,
        };
        let mut items = local_items_for_domain(&state, &domain);
        if let Some(since) = since {
            items.retain(|item| item.updated_at > since);
        }
        items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        let count = items.len();
        Ok(json!({
            "domain": domain,
            "node_id": requester.id,
            "items": items,
            "count": count,
            "exported_at": Utc::now(),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Local domain state (what sync sends and receives).
// ---------------------------------------------------------------------------

async fn stage_local_item(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<StageItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = "clusters:local-items:stage";
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<SyncItem> = (|| {
        let domain = validate_domain(&state, &body.domain)?;
        let key = require_non_empty(&body.key, "key")?;
        if key.len() > 256 {
            return Err(UnifiedError::bad_request("key is too long"));
        }
        if body.payload.is_null() {
            return Err(UnifiedError::bad_request("payload is required"));
        }
        let item = SyncItem {
            domain: domain.clone(),
            key: key.clone(),
            payload: body.payload.clone(),
            updated_at: now(),
        };
        state
            .local_items
            .write()
            .expect("items lock poisoned")
            .entry(domain)
            .or_default()
            .insert(key, item.clone());
        Ok(item)
    })();
    match result {
        Ok(item) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&item).unwrap_or(Value::Null),
                );
            }
            json::success_response(item, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_local_items(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<SyncItem> = state
            .local_items
            .read()
            .expect("items lock poisoned")
            .values()
            .flat_map(|entries| entries.values().cloned())
            .collect();
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|item| item.domain == *domain);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|item| item.key.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then(a.updated_at.cmp(&b.updated_at))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Sync health overview, conflict visibility, retry queue, settings and
// the domain registry.
// ---------------------------------------------------------------------------

async fn sync_health_overview(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let nodes: Vec<NodeState> = state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .values()
            .cloned()
            .collect();
        let plans: Vec<SyncPlanState> = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .cloned()
            .collect();
        let logs: Vec<SyncLogEntry> = state
            .logs
            .read()
            .expect("logs lock poisoned")
            .iter()
            .cloned()
            .collect();
        let conflicts: Vec<SyncConflictRecord> = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .cloned()
            .collect();
        let retries: Vec<SyncRetryEntry> = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .cloned()
            .collect();
        let count_health = |health: &str| {
            nodes.iter().filter(|node| node.health == health).count() as u64
        };
        let plans_failed: u64 = plans
            .iter()
            .filter(|plan| {
                plan.last_run_at.is_some()
                    && plan
                        .last_success_at
                        .map_or(true, |success| plan.last_run_at.unwrap() > success)
            })
            .count() as u64;
        let clusters: Vec<ClusterState> = state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .values()
            .cloned()
            .collect();
        let per_cluster: Vec<Value> = clusters
            .iter()
            .map(|cluster| {
                let cluster_nodes: Vec<&NodeState> = nodes
                    .iter()
                    .filter(|node| node.cluster_id == cluster.id)
                    .collect();
                let unhealthy = cluster_nodes
                    .iter()
                    .filter(|node| node.health == "unhealthy")
                    .count() as u64;
                let open_conflicts = conflicts
                    .iter()
                    .filter(|conflict| {
                        conflict.cluster_id == cluster.id && conflict.status == "open"
                    })
                    .count() as u64;
                let pending_retries = retries
                    .iter()
                    .filter(|entry| {
                        entry.cluster_id == cluster.id && entry.status == "pending"
                    })
                    .count() as u64;
                let dead_letters = retries
                    .iter()
                    .filter(|entry| {
                        entry.cluster_id == cluster.id && entry.status == "dead_letter"
                    })
                    .count() as u64;
                let last_success_at = logs
                    .iter()
                    .filter(|log| log.cluster_id == cluster.id && log.status == "completed")
                    .map(|log| log.finished_at)
                    .max();
                json!({
                    "cluster_id": cluster.id,
                    "cluster_name": cluster.name,
                    "status": cluster.status,
                    "nodes": cluster_nodes.len(),
                    "nodes_unhealthy": unhealthy,
                    "open_conflicts": open_conflicts,
                    "pending_retries": pending_retries,
                    "dead_letters": dead_letters,
                    "last_success_at": last_success_at,
                })
            })
            .collect();
        Ok(json!({
            "clusters": clusters.len(),
            "nodes": nodes.len(),
            "nodes_healthy": count_health("healthy"),
            "nodes_degraded": count_health("degraded"),
            "nodes_unhealthy": count_health("unhealthy"),
            "nodes_unknown": count_health("unknown"),
            "plans": plans.len(),
            "plans_enabled": plans.iter().filter(|plan| plan.enabled).count(),
            "plans_failed": plans_failed,
            "open_conflicts": conflicts.iter().filter(|conflict| conflict.status == "open").count(),
            "conflicts": conflicts.len(),
            "pending_retries": retries.iter().filter(|entry| entry.status == "pending").count(),
            "dead_letters": retries.iter().filter(|entry| entry.status == "dead_letter").count(),
            "failed_logs": logs.iter().filter(|log| log.status == "failed").count(),
            "last_sync_at": logs.iter().map(|log| log.finished_at).max(),
            "last_success_at": logs.iter().filter(|log| log.status == "completed").map(|log| log.finished_at).max(),
            "per_cluster": per_cluster,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Conflict visibility and resolution.
// ---------------------------------------------------------------------------

async fn list_conflicts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<SyncConflictRecord> = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|conflict| conflict.cluster_id == wanted);
        }
        if let Some(plan_id) = params.filters.get("plan_id") {
            let wanted: Uuid = plan_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("plan_id must be a UUID"))?;
            items.retain(|conflict| conflict.plan_id == Some(wanted));
        }
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|conflict| conflict.domain == *domain);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|conflict| conflict.status == *status);
        }
        if let Some(winner) = params.filters.get("winner") {
            items.retain(|conflict| conflict.winner == *winner);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|conflict| conflict.key.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_conflict(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: UnifiedResult<SyncConflictRecord> = state
        .conflicts
        .read()
        .expect("conflicts lock poisoned")
        .iter()
        .find(|conflict| conflict.id == id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync conflict not found"));
    json::respond(result, debug)
}

#[derive(Debug, Deserialize)]
struct ResolveConflictRequest {
    /// `keep_local`, `keep_remote` or `dismiss`.
    resolution: String,
    #[serde(default)]
    note: String,
}

async fn resolve_conflict(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<ResolveConflictRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let actor = match caller(&req) {
        Ok(who) => who.actor,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<SyncConflictRecord> = (|| {
        let id = id.into_inner();
        let resolution = body.resolution.trim();
        if !matches!(resolution, "keep_local" | "keep_remote" | "dismiss") {
            return Err(UnifiedError::bad_request(
                "resolution must be keep_local, keep_remote or dismiss",
            ));
        }
        if body.note.len() > 1024 {
            return Err(UnifiedError::bad_request("note is too long"));
        }
        let mut conflicts = state.conflicts.write().expect("conflicts lock poisoned");
        let conflict = conflicts
            .iter_mut()
            .find(|conflict| conflict.id == id)
            .ok_or_else(|| UnifiedError::not_found("Sync conflict not found"))?;
        if conflict.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Sync conflict is already {}",
                conflict.status
            )));
        }
        // An admin override applies the chosen payload as the local
        // item with a fresh timestamp, so the chosen version wins the
        // next sync instead of losing silently again. `dismiss` changes
        // no data; it only closes the record.
        if resolution != "dismiss" {
            let chosen = if resolution == "keep_local" {
                conflict.local_payload.clone()
            } else {
                conflict.remote_payload.clone()
            };
            if let Some(payload) = chosen {
                let mut items = state.local_items.write().expect("items lock poisoned");
                items.entry(conflict.domain.clone()).or_default().insert(
                    conflict.key.clone(),
                    SyncItem {
                        domain: conflict.domain.clone(),
                        key: conflict.key.clone(),
                        payload,
                        updated_at: now(),
                    },
                );
            }
        }
        conflict.status = if resolution == "dismiss" {
            "dismissed".to_string()
        } else {
            "resolved".to_string()
        };
        conflict.resolution = Some(format!(
            "{resolution}{}",
            if body.note.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", body.note.trim())
            }
        ));
        conflict.resolved_by = Some(actor.clone());
        conflict.resolved_at = Some(now());
        let updated = conflict.clone();
        drop(conflicts);
        state.emit(
            CLUSTER_SYNC_CONFLICT_RESOLVED,
            json!({
                "conflict_id": updated.id,
                "cluster_id": updated.cluster_id,
                "domain": updated.domain,
                "key": updated.key,
                "resolution": resolution,
                "resolved_by": actor,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Failed-sync retry queue and dead-letter handling.
// ---------------------------------------------------------------------------

async fn list_retries(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<SyncRetryEntry> = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|entry| entry.cluster_id == wanted);
        }
        if let Some(plan_id) = params.filters.get("plan_id") {
            let wanted: Uuid = plan_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("plan_id must be a UUID"))?;
            items.retain(|entry| entry.plan_id == wanted);
        }
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|entry| entry.domain == *domain);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|entry| entry.status == *status);
        }
        items.sort_by(|a, b| b.next_retry_at.cmp(&a.next_retry_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_retry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: UnifiedResult<SyncRetryEntry> = state
        .retries
        .read()
        .expect("retries lock poisoned")
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"));
    json::respond(result, debug)
}

/// Re-run the sync plan behind a retry entry right now. The entry is
/// marked `retrying` first; a successful run completes it through the
/// normal path, a failed run re-queues it with backoff (or dead-letters
/// it at the attempt limit) through the normal path.
async fn retry_entry_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncRetryEntry> = (|| {
        let id = id.into_inner();
        let mut queue = state.retries.write().expect("retries lock poisoned");
        let entry = queue
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"))?;
        if !matches!(entry.status.as_str(), "pending" | "dead_letter" | "retrying") {
            return Err(UnifiedError::conflict(format!(
                "Sync retry entry is {} and cannot be retried",
                entry.status
            )));
        }
        entry.status = "retrying".to_string();
        entry.updated_at = now();
        Ok(entry.clone())
    })();
    let entry = match result {
        Ok(entry) => entry,
        Err(error) => return json::error_response(&error, debug),
    };
    let run = execute_sync(&state, entry.plan_id, false).await;
    if let Err(error) = run {
        return json::error_response(&error, debug);
    }
    // `execute_sync` completes or re-queues the entry through the
    // normal path; re-read it so the caller sees the final status.
    let stored: UnifiedResult<SyncRetryEntry> = state
        .retries
        .read()
        .expect("retries lock poisoned")
        .iter()
        .find(|candidate| candidate.id == entry.id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"));
    json::respond(stored, debug)
}

async fn discard_retry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncRetryEntry> = (|| {
        let id = id.into_inner();
        let mut queue = state.retries.write().expect("retries lock poisoned");
        let entry = queue
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"))?;
        if !matches!(
            entry.status.as_str(),
            "pending" | "retrying" | "dead_letter"
        ) {
            return Err(UnifiedError::conflict(format!(
                "Sync retry entry is {} and cannot be discarded",
                entry.status
            )));
        }
        entry.status = "discarded".to_string();
        entry.updated_at = now();
        Ok(entry.clone())
    })();
    json::respond(result, debug)
}

/// Run every retry entry whose `next_retry_at` has passed. Dead-letter
/// entries are never retried here; only pending ones are due.
async fn process_due_retries(state: &Arc<ClustersState>) -> Vec<Uuid> {
    let due: Vec<SyncRetryEntry> = {
        let now = now();
        state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.status == "pending" && entry.next_retry_at <= now)
            .cloned()
            .collect()
    };
    let mut processed: Vec<Uuid> = Vec::new();
    for entry in due {
        {
            let mut queue = state.retries.write().expect("retries lock poisoned");
            if let Some(stored) = queue.iter_mut().find(|stored| stored.id == entry.id) {
                stored.status = "retrying".to_string();
                stored.updated_at = now();
            }
        }
        // `execute_sync` completes or re-queues the entry through the
        // normal path, including dead-lettering at the attempt limit.
        let _ = execute_sync(state, entry.plan_id, false).await;
        processed.push(entry.id);
    }
    processed
}

async fn process_due_retries_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let state_clone = state.clone();
    spawn_background(async move {
        let _ = process_due_retries(&state_clone).await;
    });
    let due: u64 = {
        let now = now();
        state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.status == "pending" && entry.next_retry_at <= now)
            .count() as u64
    };
    json::respond(
        Ok::<Value, UnifiedError>(json!({ "queued": true, "due": due })),
        debug,
    )
}

// ---------------------------------------------------------------------------
// Sync settings and the domain registry.
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    json::respond(Ok::<SyncSettings, UnifiedError>(state.settings()), debug)
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    replay_window_secs: Option<i64>,
    health_timeout_secs: Option<u64>,
    sync_timeout_secs: Option<u64>,
    health_failure_threshold: Option<u32>,
    max_sync_logs: Option<usize>,
    max_health_history: Option<usize>,
    max_conflicts: Option<usize>,
    max_retries: Option<usize>,
    max_idempotency_records: Option<usize>,
    retry_max_attempts: Option<u32>,
    retry_base_delay_secs: Option<u64>,
    retry_max_delay_secs: Option<u64>,
    retry_backoff_multiplier: Option<f64>,
    idempotency_ttl_secs: Option<i64>,
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncSettings> = (|| {
        let mut settings = state.settings();
        if let Some(value) = body.replay_window_secs {
            if !(30..=3600).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "replay_window_secs must be between 30 and 3600",
                ));
            }
            settings.replay_window_secs = value;
        }
        if let Some(value) = body.health_timeout_secs {
            if !(1..=300).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "health_timeout_secs must be between 1 and 300",
                ));
            }
            settings.health_timeout_secs = value;
        }
        if let Some(value) = body.sync_timeout_secs {
            if !(1..=600).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "sync_timeout_secs must be between 1 and 600",
                ));
            }
            settings.sync_timeout_secs = value;
        }
        if let Some(value) = body.health_failure_threshold {
            if !(1..=100).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "health_failure_threshold must be between 1 and 100",
                ));
            }
            settings.health_failure_threshold = value;
        }
        for (name, value) in [
            ("max_sync_logs", body.max_sync_logs),
            ("max_health_history", body.max_health_history),
            ("max_conflicts", body.max_conflicts),
            ("max_retries", body.max_retries),
            ("max_idempotency_records", body.max_idempotency_records),
        ] {
            if let Some(value) = value {
                if !(10..=100_000).contains(&value) {
                    return Err(UnifiedError::bad_request(format!(
                        "{name} must be between 10 and 100000"
                    )));
                }
            }
        }
        if let Some(value) = body.max_sync_logs {
            settings.max_sync_logs = value;
        }
        if let Some(value) = body.max_health_history {
            settings.max_health_history = value;
        }
        if let Some(value) = body.max_conflicts {
            settings.max_conflicts = value;
        }
        if let Some(value) = body.max_retries {
            settings.max_retries = value;
        }
        if let Some(value) = body.max_idempotency_records {
            settings.max_idempotency_records = value;
        }
        if let Some(value) = body.retry_max_attempts {
            if !(1..=100).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_max_attempts must be between 1 and 100",
                ));
            }
            settings.retry_max_attempts = value;
        }
        if let Some(value) = body.retry_base_delay_secs {
            if !(1..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_base_delay_secs must be between 1 and 86400",
                ));
            }
            settings.retry_base_delay_secs = value;
        }
        if let Some(value) = body.retry_max_delay_secs {
            if !(1..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_max_delay_secs must be between 1 and 86400",
                ));
            }
            settings.retry_max_delay_secs = value;
        }
        if settings.retry_max_delay_secs < settings.retry_base_delay_secs {
            return Err(UnifiedError::bad_request(
                "retry_max_delay_secs must not be smaller than retry_base_delay_secs",
            ));
        }
        if let Some(value) = body.retry_backoff_multiplier {
            if !value.is_finite() || !(1.0..=10.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retry_backoff_multiplier must be between 1.0 and 10.0",
                ));
            }
            settings.retry_backoff_multiplier = value;
        }
        if let Some(value) = body.idempotency_ttl_secs {
            if !(60..=604_800).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idempotency_ttl_secs must be between 60 and 604800",
                ));
            }
            settings.idempotency_ttl_secs = value;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            CLUSTER_SYNC_SETTINGS_UPDATED,
            json!({
                "replay_window_secs": settings.replay_window_secs,
                "health_timeout_secs": settings.health_timeout_secs,
                "sync_timeout_secs": settings.sync_timeout_secs,
                "health_failure_threshold": settings.health_failure_threshold,
                "retry_max_attempts": settings.retry_max_attempts,
                "retry_base_delay_secs": settings.retry_base_delay_secs,
                "retry_max_delay_secs": settings.retry_max_delay_secs,
                "retry_backoff_multiplier": settings.retry_backoff_multiplier,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

async fn list_domains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let mut domains: Vec<String> = state
        .domains
        .read()
        .expect("domains lock poisoned")
        .iter()
        .cloned()
        .collect();
    domains.sort();
    json::respond(
        Ok::<Value, UnifiedError>(json!({ "domains": domains, "count": domains.len() })),
        debug,
    )
}

#[derive(Debug, Deserialize)]
struct RegisterDomainRequest {
    domain: String,
}

fn valid_domain_name(domain: &str) -> bool {
    let domain = domain.trim();
    !domain.is_empty()
        && domain.len() <= 64
        && domain
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

async fn register_domain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<RegisterDomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let domain = body.domain.trim().to_string();
        if !valid_domain_name(&domain) {
            return Err(UnifiedError::bad_request(
                "domain must be 1-64 characters of lowercase letters, digits or underscore",
            ));
        }
        let mut domains = state.domains.write().expect("domains lock poisoned");
        if !domains.insert(domain.clone()) {
            return Err(UnifiedError::conflict(format!(
                "Sync domain {domain} is already registered"
            )));
        }
        let mut registered: Vec<String> = domains.iter().cloned().collect();
        registered.sort();
        Ok(json!({ "domain": domain, "domains": registered }))
    })();
    json::respond(result, debug)
}

async fn unregister_domain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let domain = domain.into_inner();
        if !valid_domain_name(&domain) {
            return Err(UnifiedError::bad_request("Invalid sync domain name"));
        }
        let in_use = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .any(|plan| plan.domains.iter().any(|d| d == &domain));
        if in_use {
            return Err(UnifiedError::conflict(format!(
                "Sync domain {domain} is still used by a sync plan"
            )));
        }
        let mut domains = state.domains.write().expect("domains lock poisoned");
        if domains.len() <= 1 {
            return Err(UnifiedError::unprocessable(
                "At least one sync domain must stay registered",
            ));
        }
        if !domains.remove(&domain) {
            return Err(UnifiedError::not_found(format!(
                "Sync domain {domain} is not registered"
            )));
        }
        let mut registered: Vec<String> = domains.iter().cloned().collect();
        registered.sort();
        Ok(json!({ "deleted": true, "domain": domain, "domains": registered }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook listeners: scheduled health checks and scheduled sync runs.
// ---------------------------------------------------------------------------

fn on_health_check_requested(state: Arc<ClustersState>, ctx: &mut HookContext) {
    let Some(node_id_text) = ctx.data.get("node_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "queued": false, "error": "cluster.health.check_requested requires 'node_id'" }));
        return;
    };
    let Ok(node_id) = node_id_text.parse::<Uuid>() else {
        ctx.add_result(json!({ "queued": false, "error": "node_id must be a UUID" }));
        return;
    };
    spawn_background(async move {
        let _ = probe_node(&state, node_id).await;
    });
    ctx.add_result(json!({ "queued": true, "node_id": node_id }));
}

fn on_sync_requested(state: Arc<ClustersState>, ctx: &mut HookContext) {
    let Some(plan_id_text) = ctx.data.get("plan_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "queued": false, "error": "cluster.sync.requested requires 'plan_id'" }));
        return;
    };
    let Ok(plan_id) = plan_id_text.parse::<Uuid>() else {
        ctx.add_result(json!({ "queued": false, "error": "plan_id must be a UUID" }));
        return;
    };
    let dry_run = ctx
        .data
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    spawn_background(async move {
        let _ = execute_sync(&state, plan_id, dry_run).await;
    });
    ctx.add_result(json!({ "queued": true, "plan_id": plan_id, "dry_run": dry_run }));
}

fn on_sync_retry_due(state: Arc<ClustersState>, ctx: &mut HookContext) {
    let state_clone = state.clone();
    spawn_background(async move {
        let _ = process_due_retries(&state_clone).await;
    });
    ctx.add_result(json!({ "queued": true }));
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ClustersModule {
    state: Arc<ClustersState>,
}

impl ClustersModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ClustersState::new()),
        }
    }
}

impl Default for ClustersModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ClustersModule {
    fn id(&self) -> &'static str {
        "clusters"
    }

    fn name(&self) -> &'static str {
        "Multi-Cluster"
    }

    fn description(&self) -> &'static str {
        "Multi-cluster management: node registration with HMAC authentication, health probing, and mutual sync with last-write-wins conflict handling, visible conflicts with admin resolution, a retry queue with dead-letter handling, idempotent admin writes, and admin-editable sync settings."
    }

    fn requires(&self) -> &[&'static str] {
        &["scheduler", "notifications"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "hmac", "sha2"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            CLUSTER_HEALTH_CHECK_REQUESTED,
            "clusters",
            Arc::new(move |ctx: &mut HookContext| {
                on_health_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            CLUSTER_SYNC_REQUESTED,
            "clusters",
            Arc::new(move |ctx: &mut HookContext| on_sync_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            CLUSTER_SYNC_RETRY_DUE,
            "clusters",
            Arc::new(move |ctx: &mut HookContext| on_sync_retry_due(state.clone(), ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/clusters/health", owner: "clusters", description: "Liveness probe other nodes call when health-checking this instance." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters", owner: "clusters", description: "List clusters (paginated, filter by kind/status)." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/clusters", owner: "clusters", description: "Create a cluster." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters/{id}", owner: "clusters", description: "Get one cluster." },
            RouteInfo { method: "PUT", path: "/api/v1/clusters/clusters/{id}", owner: "clusters", description: "Update a cluster." },
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/clusters/{id}", owner: "clusters", description: "Delete a cluster with its nodes and sync plans." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters/{id}/sync-status", owner: "clusters", description: "Per-cluster sync state: last sync, pending items, conflicts, failures." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/clusters/{id}/nodes", owner: "clusters", description: "Register a node; returns the node secret exactly once." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters/{id}/nodes", owner: "clusters", description: "List nodes of a cluster (paginated, filter by role/health/status)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/nodes/{id}", owner: "clusters", description: "Get one node (secret hash never returned)." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/nodes/{id}/revoke", owner: "clusters", description: "Revoke a node's credentials." },
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/nodes/{id}", owner: "clusters", description: "Remove a node and its sync plans." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/nodes/{id}/rotate-secret", owner: "clusters", description: "Rotate a node secret; returns the new secret exactly once." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/nodes/{id}/health-check", owner: "clusters", description: "Probe a node now and record the result." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/nodes/{id}/health", owner: "clusters", description: "Health-check history of a node (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/plans", owner: "clusters", description: "List sync plans (paginated, filter by cluster/direction/enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/plans", owner: "clusters", description: "Create a sync plan (peer secret is hashed, never stored)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/plans/{id}", owner: "clusters", description: "Get one sync plan." },
            RouteInfo { method: "PUT", path: "/api/v1/clusters/sync/plans/{id}", owner: "clusters", description: "Update a sync plan." },
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/sync/plans/{id}", owner: "clusters", description: "Delete a sync plan." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/plans/{id}/sync", owner: "clusters", description: "Run a sync plan now; body {dry_run} previews without applying. Accepts an Idempotency-Key header." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/health", owner: "clusters", description: "Aggregate sync health: node health counts, failing plans, open conflicts, pending retries and dead-letters." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/conflicts", owner: "clusters", description: "List sync conflicts (paginated, filter by cluster/plan/domain/status/winner)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/conflicts/{id}", owner: "clusters", description: "Get one sync conflict with both payloads." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/conflicts/{id}/resolve", owner: "clusters", description: "Resolve a conflict: keep_local, keep_remote or dismiss. The chosen payload is applied as the newest local version." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/retries", owner: "clusters", description: "List failed-sync retry queue entries (paginated, filter by cluster/plan/domain/status)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/retries/{id}", owner: "clusters", description: "Get one retry queue entry." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/retries/{id}/retry", owner: "clusters", description: "Re-run the sync plan behind a retry entry now (works for pending and dead-letter entries)." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/retries/{id}/discard", owner: "clusters", description: "Discard a retry queue entry so it is never retried." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/retries/process-due", owner: "clusters", description: "Queue a background run of every retry entry whose next_retry_at has passed." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/settings", owner: "clusters", description: "Read the live sync settings (timeouts, thresholds, retry backoff, idempotency TTL)." },
            RouteInfo { method: "PUT", path: "/api/v1/clusters/sync/settings", owner: "clusters", description: "Update the live sync settings (admin-editable; seeded from configurable defaults)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/domains", owner: "clusters", description: "List the registered sync domains." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/domains", owner: "clusters", description: "Register a new sync domain." },
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/sync/domains/{domain}", owner: "clusters", description: "Unregister a sync domain (refused while a plan uses it, and for the last remaining domain)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/logs", owner: "clusters", description: "List sync log entries (paginated, filter by cluster/plan/status/domain/direction)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/logs/{id}", owner: "clusters", description: "Get one sync log entry with its conflicts." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/receive", owner: "clusters", description: "Node-to-node sync receive (HMAC-signed, versioned envelope)." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/export", owner: "clusters", description: "Node-to-node sync export (HMAC-signed, domain + since cursor)." },
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/local-items", owner: "clusters", description: "Stage one local domain item for sync." },
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/local-items", owner: "clusters", description: "List local domain items (paginated, filter by domain)." },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/clusters")
                .route("/health", web::get().to(peer_health))
                .route("/clusters", web::get().to(list_clusters))
                .route("/clusters", web::post().to(create_cluster))
                .route("/clusters/{id}", web::get().to(get_cluster))
                .route("/clusters/{id}", web::put().to(update_cluster))
                .route("/clusters/{id}", web::delete().to(delete_cluster))
                .route("/clusters/{id}/sync-status", web::get().to(cluster_sync_status))
                .route("/clusters/{id}/nodes", web::post().to(register_node))
                .route("/clusters/{id}/nodes", web::get().to(list_nodes))
                .route("/nodes/{id}", web::get().to(get_node))
                .route("/nodes/{id}/revoke", web::post().to(revoke_node))
                .route("/nodes/{id}", web::delete().to(remove_node))
                .route("/nodes/{id}/rotate-secret", web::post().to(rotate_node_secret))
                .route("/nodes/{id}/health-check", web::post().to(health_check_now))
                .route("/nodes/{id}/health", web::get().to(node_health_history))
                .route("/sync/plans", web::get().to(list_plans))
                .route("/sync/plans", web::post().to(create_plan))
                .route("/sync/plans/{id}", web::get().to(get_plan))
                .route("/sync/plans/{id}", web::put().to(update_plan))
                .route("/sync/plans/{id}", web::delete().to(delete_plan))
                .route("/sync/plans/{id}/sync", web::post().to(sync_now))
                .route("/sync/health", web::get().to(sync_health_overview))
                .route("/sync/conflicts", web::get().to(list_conflicts))
                .route("/sync/conflicts/{id}", web::get().to(get_conflict))
                .route("/sync/conflicts/{id}/resolve", web::post().to(resolve_conflict))
                .route("/sync/retries", web::get().to(list_retries))
                .route("/sync/retries/{id}", web::get().to(get_retry))
                .route("/sync/retries/{id}/retry", web::post().to(retry_entry_now))
                .route("/sync/retries/{id}/discard", web::post().to(discard_retry))
                .route("/sync/retries/process-due", web::post().to(process_due_retries_endpoint))
                .route("/sync/settings", web::get().to(get_settings))
                .route("/sync/settings", web::put().to(update_settings))
                .route("/sync/domains", web::get().to(list_domains))
                .route("/sync/domains", web::post().to(register_domain))
                .route("/sync/domains/{domain}", web::delete().to(unregister_domain))
                .route("/sync/logs", web::get().to(list_sync_logs))
                .route("/sync/logs/{id}", web::get().to(get_sync_log))
                .route("/sync/receive", web::post().to(sync_receive))
                .route("/sync/export", web::get().to(sync_export))
                .route("/sync/local-items", web::post().to(stage_local_item))
                .route("/sync/local-items", web::get().to(list_local_items)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ClustersModule::new()));
}
