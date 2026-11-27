// clusters - In-memory state and its methods.

use super::*;
use unified_core::hooks::HookBus;
use unified_core::json;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

pub struct ClustersState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) clusters: RwLock<HashMap<Uuid, ClusterState>>,
    pub(crate) nodes: RwLock<HashMap<Uuid, NodeState>>,
    pub(crate) plans: RwLock<HashMap<Uuid, SyncPlanState>>,
    pub(crate) logs: RwLock<Vec<SyncLogEntry>>,
    pub(crate) health_history: RwLock<Vec<HealthCheckEntry>>,
    /// Local domain state: domain -> key -> item.
    pub(crate) local_items: RwLock<HashMap<String, HashMap<String, SyncItem>>>,
    /// Nonces seen within the replay window, for replay protection.
    pub(crate) used_nonces: RwLock<HashMap<String, DateTime<Utc>>>,
    /// Live sync settings (admin-editable; seeded from the defaults).
    pub(crate) settings: RwLock<SyncSettings>,
    /// Registered sync domains. Seeded with `DEFAULT_SYNC_DOMAINS`;
    /// admins can register or unregister domains through the API.
    pub(crate) domains: RwLock<HashSet<String>>,
    /// Conflict records, independent of the rotating sync log.
    pub(crate) conflicts: RwLock<Vec<SyncConflictRecord>>,
    /// Failed-sync retry queue, including dead-letter entries.
    pub(crate) retries: RwLock<Vec<SyncRetryEntry>>,
    /// Shared HTTP client for health probes and peer sync calls.
    /// Built once at startup and reused so connections are pooled
    /// instead of rebuilt (and re-resolved) on every probe.
    http_client: reqwest::Client,
    /// Stored responses for idempotent admin writes, keyed by
    /// (scope, key).
    idempotency: RwLock<HashMap<(String, String), IdempotencyRecord>>,
}

impl ClustersState {
    pub(crate) fn new() -> Self {
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
            http_client: reqwest::Client::new(),
            idempotency: RwLock::new(HashMap::new()),
        }
    }

    /// The shared HTTP client for health probes and peer sync calls.
    /// `reqwest::Client` is cheap to clone (it shares one pool), and
    /// callers set their own per-request timeout on the request.
    pub(crate) fn shared_http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn request_notification(&self, title: &str, body: &str, kind: &str) {
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

    pub(crate) fn settings(&self) -> SyncSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn push_log(&self, entry: SyncLogEntry) {
        let max = self.settings().max_sync_logs;
        let mut logs = self.logs.write().expect("logs lock poisoned");
        logs.push(entry);
        if logs.len() > max {
            let excess = logs.len() - max;
            logs.drain(..excess);
        }
    }

    pub(crate) fn push_health(&self, entry: HealthCheckEntry) {
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
    pub(crate) fn idempotency_lookup(&self, scope: &str, key: &str) -> Option<(u16, Value)> {
        let mut store = self.idempotency.write().expect("idempotency lock poisoned");
        let now = Utc::now();
        store.retain(|_, record| record.expires_at > now);
        store
            .get(&(scope.to_string(), key.to_string()))
            .map(|record| (record.status, record.body.clone()))
    }

    /// Store the response of an idempotent write for later replays.
    pub(crate) fn idempotency_store(&self, scope: &str, key: &str, status: u16, body: Value) {
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
    pub(crate) fn record_conflicts(
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
    pub(crate) fn enqueue_retry(
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
            // The entry keeps the attempt limit that was current when
            // it was first queued; refresh it from the live settings
            // so an admin raising or lowering the limit actually
            // applies to entries already in the queue.
            entry.max_attempts = settings.retry_max_attempts;
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
    pub(crate) fn complete_retry(&self, plan_id: Uuid, domain: &str) {
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

    /// Re-queue retry entries left in `retrying` after a sync run
    /// failed before its domain loop (plan not found or disabled, no
    /// peer credential, node revoked). The per-domain loop is the only
    /// place that normally re-queues or completes an entry, so without
    /// this a pre-loop failure leaves the entry stuck in `retrying`
    /// forever: due processing only selects `pending` entries. Routing
    /// through `enqueue_retry` counts the failed attempt, applies
    /// backoff, and dead-letters at the attempt limit.
    pub(crate) fn requeue_stuck_retries(&self, plan_id: Uuid, error: &str) {
        let stuck: Vec<SyncRetryEntry> = {
            let queue = self.retries.read().expect("retries lock poisoned");
            queue
                .iter()
                .filter(|entry| entry.plan_id == plan_id && entry.status == "retrying")
                .cloned()
                .collect()
        };
        for entry in stuck {
            self.enqueue_retry(
                entry.plan_id,
                entry.cluster_id,
                entry.node_id,
                &entry.domain,
                &entry.direction,
                error,
            );
        }
    }

    /// Delete every record owned by a cluster: sync logs, conflict
    /// records and retry entries. Call after the cluster's nodes and
    /// plans are removed so deleting a cluster leaves no orphans.
    pub(crate) fn delete_cluster_records(&self, cluster_id: Uuid) {
        self.logs
            .write()
            .expect("logs lock poisoned")
            .retain(|log| log.cluster_id != cluster_id);
        self.conflicts
            .write()
            .expect("conflicts lock poisoned")
            .retain(|conflict| conflict.cluster_id != cluster_id);
        self.retries
            .write()
            .expect("retries lock poisoned")
            .retain(|entry| entry.cluster_id != cluster_id);
    }

    /// Delete every record owned by a sync plan: sync logs, conflict
    /// records and retry entries. Call when the plan is deleted so no
    /// retry entry keeps pointing at a plan that no longer exists.
    pub(crate) fn delete_plan_records(&self, plan_id: Uuid) {
        self.logs
            .write()
            .expect("logs lock poisoned")
            .retain(|log| log.plan_id != Some(plan_id));
        self.conflicts
            .write()
            .expect("conflicts lock poisoned")
            .retain(|conflict| conflict.plan_id != Some(plan_id));
        self.retries
            .write()
            .expect("retries lock poisoned")
            .retain(|entry| entry.plan_id != plan_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retry_entry(plan_id: Uuid, cluster_id: Uuid, status: &str) -> SyncRetryEntry {
        SyncRetryEntry {
            id: Uuid::now_v7(),
            plan_id,
            cluster_id,
            node_id: None,
            domain: "config".to_string(),
            direction: "pull".to_string(),
            attempt: 1,
            max_attempts: DEFAULT_RETRY_MAX_ATTEMPTS,
            next_retry_at: Utc::now(),
            last_error: None,
            status: status.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn log_entry(plan_id: Option<Uuid>, cluster_id: Uuid) -> SyncLogEntry {
        SyncLogEntry {
            id: Uuid::now_v7(),
            plan_id,
            cluster_id,
            node_id: None,
            direction: "pull".to_string(),
            domain: "config".to_string(),
            status: "failed".to_string(),
            items_sent: 0,
            items_received: 0,
            items_applied: 0,
            items_skipped: 0,
            conflicts: Vec::new(),
            errors: Vec::new(),
            dry_run: false,
            started_at: Utc::now(),
            finished_at: Utc::now(),
        }
    }

    fn conflict_record(plan_id: Option<Uuid>, cluster_id: Uuid) -> SyncConflictRecord {
        SyncConflictRecord {
            id: Uuid::now_v7(),
            log_id: None,
            plan_id,
            cluster_id,
            node_id: None,
            domain: "config".to_string(),
            key: "k1".to_string(),
            local_updated_at: None,
            remote_updated_at: None,
            winner: "local".to_string(),
            reason: "test".to_string(),
            local_payload: None,
            remote_payload: None,
            status: "open".to_string(),
            resolution: None,
            resolved_by: None,
            resolved_at: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn enqueue_retry_refreshes_max_attempts_from_settings() {
        let state = ClustersState::new();
        let plan_id = Uuid::now_v7();
        let cluster_id = Uuid::now_v7();
        let first = state.enqueue_retry(plan_id, cluster_id, None, "config", "pull", "boom");
        assert_eq!(first.max_attempts, DEFAULT_RETRY_MAX_ATTEMPTS);
        assert_eq!(first.status, "pending");

        // An admin lowers the limit: the re-queued entry picks it up
        // and dead-letters once the new limit is reached.
        state.settings.write().expect("settings lock poisoned").retry_max_attempts = 2;
        let second = state.enqueue_retry(plan_id, cluster_id, None, "config", "pull", "boom again");
        assert_eq!(second.id, first.id);
        assert_eq!(second.max_attempts, 2);
        assert_eq!(second.attempt, 2);
        assert_eq!(second.status, "dead_letter");

        // Raising the limit is picked up the same way.
        let other = state.enqueue_retry(plan_id, cluster_id, None, "modules", "pull", "boom");
        assert_eq!(other.max_attempts, 2);
        state.settings.write().expect("settings lock poisoned").retry_max_attempts = 10;
        let other = state.enqueue_retry(plan_id, cluster_id, None, "modules", "pull", "boom again");
        assert_eq!(other.max_attempts, 10);
        assert_eq!(other.attempt, 2);
        assert_eq!(other.status, "pending");
    }

    #[test]
    fn requeue_stuck_retries_resets_retrying_entries() {
        let state = ClustersState::new();
        let plan_id = Uuid::now_v7();
        let cluster_id = Uuid::now_v7();
        let entry = retry_entry(plan_id, cluster_id, "retrying");
        let entry_id = entry.id;
        state.retries.write().expect("retries lock poisoned").push(entry);

        state.requeue_stuck_retries(plan_id, "Sync plan not found");
        let stored = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .find(|candidate| candidate.id == entry_id)
            .cloned()
            .expect("entry still present");
        assert_eq!(stored.status, "pending");
        assert_eq!(stored.attempt, 2);
        assert_eq!(stored.last_error.as_deref(), Some("Sync plan not found"));

        // An entry already at the attempt limit dead-letters instead.
        let mut doomed = retry_entry(plan_id, cluster_id, "retrying");
        doomed.attempt = DEFAULT_RETRY_MAX_ATTEMPTS;
        doomed.domain = "modules".to_string();
        let doomed_id = doomed.id;
        state.retries.write().expect("retries lock poisoned").push(doomed);
        state.requeue_stuck_retries(plan_id, "Sync plan not found");
        let stored = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .find(|candidate| candidate.id == doomed_id)
            .cloned()
            .expect("entry still present");
        assert_eq!(stored.status, "dead_letter");
    }

    #[test]
    fn delete_cluster_records_removes_only_that_clusters_records() {
        let state = ClustersState::new();
        let keep = Uuid::now_v7();
        let drop = Uuid::now_v7();
        let plan_id = Uuid::now_v7();
        state.push_log(log_entry(Some(plan_id), drop));
        state.push_log(log_entry(Some(plan_id), keep));
        state.conflicts.write().expect("conflicts lock poisoned").push(conflict_record(Some(plan_id), drop));
        state.conflicts.write().expect("conflicts lock poisoned").push(conflict_record(Some(plan_id), keep));
        state.retries.write().expect("retries lock poisoned").push(retry_entry(plan_id, drop, "pending"));
        state.retries.write().expect("retries lock poisoned").push(retry_entry(plan_id, keep, "pending"));

        state.delete_cluster_records(drop);

        assert!(state.logs.read().expect("logs lock poisoned").iter().all(|log| log.cluster_id == keep));
        assert_eq!(state.logs.read().expect("logs lock poisoned").len(), 1);
        assert!(state.conflicts.read().expect("conflicts lock poisoned").iter().all(|c| c.cluster_id == keep));
        assert_eq!(state.conflicts.read().expect("conflicts lock poisoned").len(), 1);
        assert!(state.retries.read().expect("retries lock poisoned").iter().all(|e| e.cluster_id == keep));
        assert_eq!(state.retries.read().expect("retries lock poisoned").len(), 1);
    }

    #[test]
    fn delete_plan_records_removes_only_that_plans_records() {
        let state = ClustersState::new();
        let cluster_id = Uuid::now_v7();
        let keep_plan = Uuid::now_v7();
        let drop_plan = Uuid::now_v7();
        state.push_log(log_entry(Some(drop_plan), cluster_id));
        state.push_log(log_entry(Some(keep_plan), cluster_id));
        state.push_log(log_entry(None, cluster_id));
        state.conflicts.write().expect("conflicts lock poisoned").push(conflict_record(Some(drop_plan), cluster_id));
        state.conflicts.write().expect("conflicts lock poisoned").push(conflict_record(Some(keep_plan), cluster_id));
        state.retries.write().expect("retries lock poisoned").push(retry_entry(drop_plan, cluster_id, "pending"));
        state.retries.write().expect("retries lock poisoned").push(retry_entry(keep_plan, cluster_id, "pending"));

        state.delete_plan_records(drop_plan);

        assert_eq!(state.logs.read().expect("logs lock poisoned").len(), 2);
        assert!(state.logs.read().expect("logs lock poisoned").iter().all(|log| log.plan_id != Some(drop_plan)));
        assert_eq!(state.conflicts.read().expect("conflicts lock poisoned").len(), 1);
        assert!(state.conflicts.read().expect("conflicts lock poisoned").iter().all(|c| c.plan_id != Some(drop_plan)));
        assert_eq!(state.retries.read().expect("retries lock poisoned").len(), 1);
        assert!(state.retries.read().expect("retries lock poisoned").iter().all(|e| e.plan_id != drop_plan));
    }

    #[test]
    fn shared_http_client_is_built_once_and_reusable() {
        let state = ClustersState::new();
        // Building the client must not panic, and clones share the
        // pool (two clones from the same state are the same client).
        let first = state.shared_http_client();
        let second = state.shared_http_client();
        drop(first);
        drop(second);
    }
}
