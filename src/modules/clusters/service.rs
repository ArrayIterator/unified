// clusters - Internal service helpers shared by the handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

// Caller identity and capability checks.
//
// Admin endpoints require `X-User-Id` and `X-Role` headers. The role is
// checked through the `role.check` hook answered by the roles module.
// The check fails closed: no answer means not allowed. Node-to-node
// endpoints do not use this; they authenticate with HMAC signatures.

pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
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

pub(crate) fn require_permission(
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

// Health probing.

pub(crate) async fn probe_node(
    state: &Arc<ClustersState>,
    node_id: Uuid,
) -> UnifiedResult<HealthCheckEntry> {
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
    let started = Instant::now();
    let outcome: Result<u16, String> = match state
        .shared_http_client()
        .get(&url)
        .timeout(Duration::from_secs(state.settings().health_timeout_secs))
        .send()
        .await
    {
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

// Conflict handling: last-write-wins on `updated_at`, never silent.

/// Apply incoming items to the local domain store. Returns
/// (applied, skipped, conflicts). A conflict is recorded whenever both
/// sides hold different payloads and neither timestamp wins cleanly, or
/// the local version is newer and must be kept.
pub(crate) fn apply_incoming_items(
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
                // `local` borrows `domain_store`, so the timestamp used
                // by the conflict records below is copied out before
                // any insert (which needs a mutable borrow).
                let local_updated_at = local.updated_at;
                if local.payload == incoming.payload {
                    skipped += 1;
                } else if incoming.updated_at > local.updated_at {
                    domain_store.insert(incoming.key.clone(), incoming.clone());
                    applied += 1;
                } else if local.updated_at > incoming.updated_at {
                    conflicts.push(SyncConflict {
                        domain: domain.to_string(),
                        key: incoming.key.clone(),
                        local_updated_at: Some(local_updated_at),
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
                            local_updated_at: Some(local_updated_at),
                            remote_updated_at: Some(incoming.updated_at),
                            winner: "remote".to_string(),
                            reason: "Equal timestamps with different payloads; lexicographically larger payload won".to_string(),
                        });
                    } else {
                        conflicts.push(SyncConflict {
                            domain: domain.to_string(),
                            key: incoming.key.clone(),
                            local_updated_at: Some(local_updated_at),
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
/// Returns (would_send, would_receive, conflicts). The plan direction
/// bounds the preview: a pull-only plan never sends, a push-only plan
/// never receives, so the counts only report what the plan would
/// actually do.
fn compare_items(
    local: &[SyncItem],
    remote: &[SyncItem],
    direction: &str,
) -> (u64, u64, Vec<SyncConflict>) {
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
    // A one-way plan only reports the direction it actually runs.
    if direction == "pull" {
        would_send = 0;
    } else if direction == "push" {
        would_receive = 0;
    }
    (would_send, would_receive, conflicts)
}

pub(crate) fn local_items_for_domain(state: &Arc<ClustersState>, domain: &str) -> Vec<SyncItem> {
    state
        .local_items
        .read()
        .expect("items lock poisoned")
        .get(domain)
        .map(|entries| entries.values().cloned().collect())
        .unwrap_or_default()
}

// Sync execution.

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SyncRunSummary {
    pub(crate) plan_id: Uuid,
    pub(crate) cluster_id: Uuid,
    pub(crate) direction: String,
    pub(crate) dry_run: bool,
    pub(crate) domains: Vec<SyncDomainSummary>,
    pub(crate) conflicts: Vec<SyncConflict>,
    pub(crate) errors: Vec<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct SyncDomainSummary {
    pub(crate) domain: String,
    pub(crate) status: String,
    items_sent: u64,
    items_received: u64,
    items_applied: u64,
    items_skipped: u64,
    pub(crate) conflicts: u64,
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
    let headers = signed_headers(state, plan, &[])?;
    let mut request = state
        .shared_http_client()
        .get(&url)
        .timeout(Duration::from_secs(state.settings().sync_timeout_secs));
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
    let client = state.shared_http_client();
    let headers = match signed_headers(state, plan, &body) {
        Ok(headers) => headers,
        Err(error) => return (0, 0, 0, vec![error.message]),
    };
    let url = join_url(&node.base_url, "/api/v1/clusters/sync/receive");
    let mut request = client
        .post(&url)
        .body(body)
        .timeout(Duration::from_secs(state.settings().sync_timeout_secs));
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
pub(crate) async fn execute_sync(
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
                    let (would_send, would_receive, conflicts) =
                        compare_items(&local, &remote, &plan.direction);
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
            errors: domain_errors.clone(),
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
    // A dry run is a preview, not a run: it must not stamp
    // `last_run_at` / `last_success_at`, or a preview would count as a
    // completed run (and move the incremental `since` cursor).
    if !dry_run {
        let mut plans = state.plans.write().expect("plans lock poisoned");
        if let Some(entry) = plans.get_mut(&plan_id) {
            entry.last_run_at = Some(finished_at);
            if !any_failed {
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
pub(crate) fn spawn_background<F>(future: F)
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

/// Run every retry entry whose `next_retry_at` has passed. Dead-letter
/// entries are never retried here; only pending ones are due.
pub(crate) async fn process_due_retries(state: &Arc<ClustersState>) -> Vec<Uuid> {
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
        // When it fails before the domain loop (plan missing or
        // disabled, no peer credential, node revoked) no domain ran,
        // so the entry would otherwise stay `retrying` forever: put
        // it back to `pending` (or dead-letter it) instead of
        // dropping the failure.
        if let Err(error) = execute_sync(state, entry.plan_id, false).await {
            state.requeue_stuck_retries(entry.plan_id, &error.message);
        }
        processed.push(entry.id);
    }
    processed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(domain: &str, key: &str, payload: &str, at: DateTime<Utc>) -> SyncItem {
        SyncItem {
            domain: domain.to_string(),
            key: key.to_string(),
            payload: serde_json::json!(payload),
            updated_at: at,
        }
    }

    #[test]
    fn join_url_trims_trailing_slash() {
        assert_eq!(join_url("https://x.example.com/", "/a"), "https://x.example.com/a");
        assert_eq!(join_url("https://x.example.com", "/a"), "https://x.example.com/a");
        assert_eq!(join_url("https://x.example.com///", "/a"), "https://x.example.com/a");
    }

    #[test]
    fn compare_items_counts_send_receive_conflicts() {
        let older = Utc::now() - chrono::Duration::seconds(60);
        let newer = Utc::now();
        // Local-only item is sent; remote-only item is received.
        let (send, receive, conflicts) = compare_items(
            &[item("config", "k1", "a", older)],
            &[item("config", "k2", "b", older)],
            "bidirectional",
        );
        assert_eq!((send, receive), (1, 1));
        assert!(conflicts.is_empty());
        // Same key, different payload, local newer: send.
        let (send, receive, conflicts) = compare_items(
            &[item("config", "k1", "a", newer)],
            &[item("config", "k1", "b", older)],
            "bidirectional",
        );
        assert_eq!((send, receive), (1, 0));
        assert!(conflicts.is_empty());
        // Same key, different payload, remote newer: receive.
        let (send, receive, conflicts) = compare_items(
            &[item("config", "k1", "a", older)],
            &[item("config", "k1", "b", newer)],
            "bidirectional",
        );
        assert_eq!((send, receive), (0, 1));
        assert!(conflicts.is_empty());
        // Same key, different payload, equal timestamps: conflict.
        let (send, receive, conflicts) = compare_items(
            &[item("config", "k1", "a", newer)],
            &[item("config", "k1", "b", newer)],
            "bidirectional",
        );
        assert_eq!((send, receive), (0, 0));
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, "k1");
        assert_eq!(conflicts[0].winner, "undecided");
        // Identical items: nothing to do.
        let (send, receive, conflicts) = compare_items(
            &[item("config", "k1", "a", newer)],
            &[item("config", "k1", "a", newer)],
            "bidirectional",
        );
        assert_eq!((send, receive), (0, 0));
        assert!(conflicts.is_empty());
    }

    #[test]
    fn compare_items_respects_plan_direction() {
        let at = Utc::now();
        let local = [item("config", "k1", "a", at)];
        let remote = [item("config", "k2", "b", at)];
        // A pull-only plan never sends, even for local-only items.
        let (send, receive, _) = compare_items(&local, &remote, "pull");
        assert_eq!((send, receive), (0, 1));
        // A push-only plan never receives, even for remote-only items.
        let (send, receive, _) = compare_items(&local, &remote, "push");
        assert_eq!((send, receive), (1, 0));
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime builds")
    }

    fn seed_plan(state: &Arc<ClustersState>) -> Uuid {
        let cluster_id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        state
            .clusters
            .write()
            .expect("clusters lock poisoned")
            .insert(
                cluster_id,
                ClusterState {
                    id: cluster_id,
                    name: "test".to_string(),
                    description: String::new(),
                    kind: "unified".to_string(),
                    status: "active".to_string(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            );
        state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .insert(
                node_id,
                NodeState {
                    id: node_id,
                    cluster_id,
                    display_name: "peer".to_string(),
                    base_url: "https://peer.example.com".to_string(),
                    role: "member".to_string(),
                    status: "active".to_string(),
                    secret_hash: "aabbcc".to_string(),
                    secret_fingerprint: "sha256:masked".to_string(),
                    health: "unknown".to_string(),
                    consecutive_failures: 0,
                    last_seen_at: None,
                    last_latency_ms: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            );
        let plan_id = Uuid::now_v7();
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .insert(
                plan_id,
                SyncPlanState {
                    id: plan_id,
                    cluster_id,
                    target_node_id: node_id,
                    local_node_id: "local-node".to_string(),
                    direction: "bidirectional".to_string(),
                    domains: Vec::new(),
                    schedule: "3600".to_string(),
                    enabled: true,
                    peer_secret_hash: "aabbcc".to_string(),
                    peer_secret_fingerprint: None,
                    last_run_at: None,
                    last_success_at: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            );
        plan_id
    }

    #[test]
    fn dry_run_does_not_stamp_plan_run_timestamps() {
        let state = Arc::new(ClustersState::new());
        // Empty domains: the run touches no network, so the timestamp
        // behaviour is exercised in isolation.
        let plan_id = seed_plan(&state);
        let rt = runtime();

        let summary = rt
            .block_on(execute_sync(&state, plan_id, true))
            .expect("dry run succeeds");
        assert!(summary.dry_run);
        let plan = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .get(&plan_id)
            .cloned()
            .expect("plan still present");
        assert!(plan.last_run_at.is_none());
        assert!(plan.last_success_at.is_none());

        rt.block_on(execute_sync(&state, plan_id, false))
            .expect("real run succeeds");
        let plan = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .get(&plan_id)
            .cloned()
            .expect("plan still present");
        assert!(plan.last_run_at.is_some());
        assert!(plan.last_success_at.is_some());
    }

    #[test]
    fn process_due_retries_requeues_entry_when_plan_is_missing() {
        let state = Arc::new(ClustersState::new());
        // A retry entry whose plan no longer exists: execute_sync
        // fails before the domain loop, so without the re-queue the
        // entry would stay `retrying` forever.
        let missing_plan = Uuid::now_v7();
        let entry = SyncRetryEntry {
            id: Uuid::now_v7(),
            plan_id: missing_plan,
            cluster_id: Uuid::now_v7(),
            node_id: None,
            domain: "config".to_string(),
            direction: "pull".to_string(),
            attempt: 1,
            max_attempts: DEFAULT_RETRY_MAX_ATTEMPTS,
            next_retry_at: Utc::now() - chrono::Duration::seconds(1),
            last_error: Some("boom".to_string()),
            status: "pending".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let entry_id = entry.id;
        state
            .retries
            .write()
            .expect("retries lock poisoned")
            .push(entry);

        let processed = runtime().block_on(process_due_retries(&state));
        assert_eq!(processed, vec![entry_id]);
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
        assert!(stored.next_retry_at > Utc::now());
    }
}
