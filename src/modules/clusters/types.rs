// clusters - Shared types: constants, enums, records, and request/response shapes.

use chrono::{DateTime, Utc};
use hmac::Hmac;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use uuid::Uuid;

pub(crate) type HmacSha256 = Hmac<Sha256>;

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

pub const CLUSTER_NODE_SECRET_ROTATED: &str = "cluster.node.secret_rotated";

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

pub(crate) const HEADER_NODE_ID: &str = "x-cluster-node-id";

pub(crate) const HEADER_TIMESTAMP: &str = "x-cluster-timestamp";

pub(crate) const HEADER_NONCE: &str = "x-cluster-nonce";

pub(crate) const HEADER_SIGNATURE: &str = "x-cluster-signature";

// Configurable sync settings.
//
// All behaviour knobs live here as data, seeded from the defaults above
// and editable at runtime through `GET/PUT /api/v1/clusters/sync/settings`.

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
    pub(crate) fn retry_delay_secs(&self, attempt: u32) -> u64 {
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

// Conflict records, retry-queue entries, idempotency records.

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
/// retried automatically again - an admin must retry or discard them.
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
/// across different operations. Scopes embed a hash of the canonical
/// request body, so the same key with a different body is a different
/// operation and never replays the first response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    pub scope: String,
    pub key: String,
    pub status: u16,
    pub body: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

// State.

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
    /// The node id this instance uses when talking to the peer - the id
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
