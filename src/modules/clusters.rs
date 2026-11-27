// Multi-Cluster module - clusters of Unified nodes and provider pools,
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
// payloads) and resolve (`keep_local`, `keep_remote` or `dismiss`) -
// overwrites are never silent. A dry-run mode reports what would change
// without applying anything.
//
// Reliability: a failed sync domain enters a retry queue with
// exponential backoff (settings-driven); after the configured maximum
// attempts it becomes dead-letter - visible to admins, never retried
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

pub mod ai_capabilities;
mod crypto;
mod handlers;
mod handlers_admin;
mod handlers_plans;
mod handlers_sync;
pub mod module;
mod service;
pub mod state;
pub mod types;
mod validation;

pub(crate) use self::crypto::generate_secret;
pub(crate) use self::crypto::hash_secret;
pub(crate) use self::crypto::join_url;
pub(crate) use self::crypto::now;
pub(crate) use self::crypto::secret_fingerprint;
pub(crate) use self::crypto::signed_headers;
pub(crate) use self::crypto::verify_node_request;
pub(crate) use self::handlers::cluster_sync_status;
pub(crate) use self::handlers::create_cluster;
pub(crate) use self::handlers::delete_cluster;
pub(crate) use self::handlers::get_cluster;
pub(crate) use self::handlers::get_node;
pub(crate) use self::handlers::health_check_now;
pub(crate) use self::handlers::list_clusters;
pub(crate) use self::handlers::list_nodes;
pub(crate) use self::handlers::node_health_history;
pub(crate) use self::handlers::peer_health;
pub(crate) use self::handlers::register_node;
pub(crate) use self::handlers::remove_node;
pub(crate) use self::handlers::revoke_node;
pub(crate) use self::handlers::rotate_node_secret;
pub(crate) use self::handlers::update_cluster;
pub(crate) use self::handlers_admin::get_settings;
pub(crate) use self::handlers_admin::list_domains;
pub(crate) use self::handlers_admin::on_health_check_requested;
pub(crate) use self::handlers_admin::on_sync_requested;
pub(crate) use self::handlers_admin::on_sync_retry_due;
pub(crate) use self::handlers_admin::register_domain;
pub(crate) use self::handlers_admin::unregister_domain;
pub(crate) use self::handlers_admin::update_settings;
pub(crate) use self::handlers_plans::create_plan;
pub(crate) use self::handlers_plans::delete_plan;
pub(crate) use self::handlers_plans::get_plan;
pub(crate) use self::handlers_plans::list_plans;
pub(crate) use self::handlers_plans::update_plan;
pub(crate) use self::handlers_sync::discard_retry;
pub(crate) use self::handlers_sync::get_conflict;
pub(crate) use self::handlers_sync::get_retry;
pub(crate) use self::handlers_sync::get_sync_log;
pub(crate) use self::handlers_sync::list_conflicts;
pub(crate) use self::handlers_sync::list_local_items;
pub(crate) use self::handlers_sync::list_retries;
pub(crate) use self::handlers_sync::list_sync_logs;
pub(crate) use self::handlers_sync::process_due_retries_endpoint;
pub(crate) use self::handlers_sync::resolve_conflict;
pub(crate) use self::handlers_sync::retry_entry_now;
pub(crate) use self::handlers_sync::stage_local_item;
pub(crate) use self::handlers_sync::sync_export;
pub(crate) use self::handlers_sync::sync_health_overview;
pub(crate) use self::handlers_sync::sync_now;
pub(crate) use self::handlers_sync::sync_receive;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::ClustersModule;
pub use self::module::register;
pub(crate) use self::service::SyncRunSummary;
pub(crate) use self::service::apply_incoming_items;
pub(crate) use self::service::caller;
pub(crate) use self::service::execute_sync;
pub(crate) use self::service::local_items_for_domain;
pub(crate) use self::service::probe_node;
pub(crate) use self::service::process_due_retries;
pub(crate) use self::service::require_permission;
pub(crate) use self::service::spawn_background;
pub use self::state::ClustersState;
pub use self::types::CLUSTER_CREATED;
pub use self::types::CLUSTER_HEALTH_CHANGED;
pub use self::types::CLUSTER_HEALTH_CHECK_REQUESTED;
pub use self::types::CLUSTER_NODE_REGISTERED;
pub use self::types::CLUSTER_NODE_REVOKED;
pub use self::types::CLUSTER_SYNC_COMPLETED;
pub use self::types::CLUSTER_SYNC_CONFLICT;
pub use self::types::CLUSTER_SYNC_CONFLICT_RESOLVED;
pub use self::types::CLUSTER_SYNC_DEAD_LETTER;
pub use self::types::CLUSTER_SYNC_FAILED;
pub use self::types::CLUSTER_SYNC_REQUESTED;
pub use self::types::CLUSTER_SYNC_RETRY_DUE;
pub use self::types::CLUSTER_SYNC_RETRY_QUEUED;
pub use self::types::CLUSTER_SYNC_SETTINGS_UPDATED;
pub use self::types::CLUSTER_SYNC_STARTED;
pub use self::types::CLUSTER_UPDATED;
pub use self::types::ClusterState;
pub use self::types::DEFAULT_HEALTH_FAILURE_THRESHOLD;
pub use self::types::DEFAULT_HEALTH_TIMEOUT_SECS;
pub use self::types::DEFAULT_IDEMPOTENCY_TTL_SECS;
pub use self::types::DEFAULT_MAX_CONFLICTS;
pub use self::types::DEFAULT_MAX_HEALTH_HISTORY;
pub use self::types::DEFAULT_MAX_IDEMPOTENCY_RECORDS;
pub use self::types::DEFAULT_MAX_RETRIES;
pub use self::types::DEFAULT_MAX_SYNC_LOGS;
pub use self::types::DEFAULT_REPLAY_WINDOW_SECS;
pub use self::types::DEFAULT_RETRY_BACKOFF_MULTIPLIER;
pub use self::types::DEFAULT_RETRY_BASE_DELAY_SECS;
pub use self::types::DEFAULT_RETRY_MAX_ATTEMPTS;
pub use self::types::DEFAULT_RETRY_MAX_DELAY_SECS;
pub use self::types::DEFAULT_SYNC_DOMAINS;
pub use self::types::DEFAULT_SYNC_TIMEOUT_SECS;
pub use self::types::HEADER_IDEMPOTENCY_KEY;
pub use self::types::HEADER_IDEMPOTENCY_KEY_ALT;
pub(crate) use self::types::HEADER_NODE_ID;
pub(crate) use self::types::HEADER_NONCE;
pub(crate) use self::types::HEADER_SIGNATURE;
pub(crate) use self::types::HEADER_TIMESTAMP;
pub use self::types::HealthCheckEntry;
pub(crate) use self::types::HmacSha256;
pub use self::types::IdempotencyRecord;
pub use self::types::MAX_IDEMPOTENCY_KEY_LEN;
pub use self::types::NodeState;
pub use self::types::PERM_CLUSTERS_MANAGE;
pub use self::types::PERM_CLUSTERS_VIEW;
pub use self::types::SYNC_ENVELOPE_VERSION;
pub use self::types::SyncConflict;
pub use self::types::SyncConflictRecord;
pub use self::types::SyncItem;
pub use self::types::SyncLogEntry;
pub use self::types::SyncPlanState;
pub use self::types::SyncRetryEntry;
pub use self::types::SyncSettings;
pub(crate) use self::validation::idempotency_key_from;
pub(crate) use self::validation::paginate;
pub(crate) use self::validation::require_non_empty;
pub(crate) use self::validation::valid_domain_name;
pub(crate) use self::validation::valid_schedule;
pub(crate) use self::validation::validate_base_url;
pub(crate) use self::validation::validate_domain;
pub(crate) use self::validation::validate_domains;
