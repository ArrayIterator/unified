//! Activity Timeline module. - State.
//!
//! Split from `src/modules/timeline.rs`; behaviour unchanged.

use super::handlers::*;
use super::helpers::*;
use super::module::*;
use super::types::*;

use unified_core::hooks::events;
use unified_core::json;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use uuid::Uuid;

/// Events this module subscribes to: the exact records event set,
/// then the verified failure events.
pub(super) const TIMELINE_EVENTS: &[&str] = &[
    events::USER_CREATED,
    events::USER_UPDATED,
    events::ROLE_CREATED,
    events::LICENSE_ISSUED,
    events::LICENSE_REVOKED,
    events::MODULE_ENABLED,
    events::MODULE_DISABLED,
    "order.created",
    "invoice.paid",
    events::SSL_RENEW,
    events::AUTH_FAILED,
    events::GEO_LOOKUP,
    "cloudflare.dns.changed",
    "cloudflare.cache.purged",
    "cloudflare.connected",
    "google.sso.completed",
    "google.drive.link",
    "google.drive.unlink",
    "user.deleted",
    "task.created",
    "task.assigned",
    "task.status.changed",
    "task.completed",
    "task.comment.added",
    "security.auth.failed",
    "security.bruteforce.locked",
    "cluster.sync.failed",
    "payment.failed",
];

pub struct TimelineState {
    pub(super) entries: DashMap<String, TimelineEntry>,
    pub(super) seq: AtomicU64,
}

impl TimelineState {
    pub(super) fn new() -> Self {
        Self {
            entries: DashMap::new(),
            seq: AtomicU64::new(0),
        }
    }

    /// Append one entry derived from a hook event payload. This is the
    /// only way entries are created.
    pub(super) fn record(&self, event: &str, data: &Value) -> TimelineEntry {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let actor = data
            .get("actor_id")
            .or_else(|| data.get("actor"))
            .or_else(|| data.get("user_id"))
            .or_else(|| data.get("account"))
            .and_then(|value| value.as_str())
            .unwrap_or("system")
            .to_string();
        let (entity_type, entity_id) = derive_entity(event, data);
        let summary = data
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{event} on {entity_type}:{entity_id}"));
        // Only the whitelisted summary is stored: the raw event
        // payload can carry secrets, tokens, passwords, user ids and
        // IPs, and the feed serves `details`, so storing the payload
        // would leak it to every feed reader. IPs are not retained
        // either; the durable audit log remains the source for them.
        // (Computed before the struct literal: the fields below move
        // these values, so they cannot be borrowed after.)
        let details = whitelisted_details(&summary, &entity_type, &entity_id, &actor);
        let entry = TimelineEntry {
            id: Uuid::new_v4().to_string(),
            seq,
            actor,
            action: event.to_string(),
            entity_type,
            entity_id,
            summary,
            source: source_for(event),
            details,
            ip: None,
            created_at: Utc::now(),
        };
        self.entries.insert(entry.id.clone(), entry.clone());
        self.evict_oldest_if_over_cap();
        entry
    }

    /// Drop the oldest entries while the cache is over the cap.
    pub(super) fn evict_oldest_if_over_cap(&self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut seqs: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().seq))
            .collect();
        seqs.sort_by(|a, b| a.1.cmp(&b.1));
        let excess = self.entries.len() - MAX_ENTRIES;
        for (key, _) in seqs.into_iter().take(excess) {
            self.entries.remove(&key);
        }
    }

    pub(super) fn all_entries(&self) -> Vec<TimelineEntry> {
        self.entries
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }
}

/// Source family for an event name, for the `source` field.
pub(super) fn source_for(event: &str) -> String {
    if event.starts_with("task.") {
        "task".to_string()
    } else if event.starts_with("security.") {
        "security".to_string()
    } else if event.starts_with("cluster.") {
        "cluster".to_string()
    } else if event.starts_with("payment.") {
        "payment".to_string()
    } else if event.starts_with("auth.") {
        "audit".to_string()
    } else {
        "record".to_string()
    }
}

/// Derive the entity type and id from the event payload, using the
/// same precedence the records module uses: explicit `entity_type` /
/// `entity_id` first, then a `resource` value of the form `type:id`,
/// then an `id` key paired with the event name prefix, and finally
/// the whole event name as the type with an empty id.
pub(super) fn derive_entity(event: &str, data: &Value) -> (String, String) {
    if let Some(entity_type) = data.get("entity_type").and_then(|v| v.as_str()) {
        let entity_id = data
            .get("entity_id")
            .and_then(|v| v.as_str())
            .or_else(|| data.get("id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        return (entity_type.to_string(), entity_id);
    }
    if let Some(resource) = data.get("resource").and_then(|v| v.as_str()) {
        if let Some((kind, id)) = resource.split_once(':') {
            if !kind.is_empty() && !id.is_empty() {
                return (kind.to_string(), id.to_string());
            }
        }
        return (event.to_string(), resource.to_string());
    }
    let entity_type = event.split('.').next().unwrap_or(event).to_string();
    let entity_id = data
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("order_id").and_then(|v| v.as_str()))
        .or_else(|| data.get("cluster_id").and_then(|v| v.as_str()))
        .or_else(|| data.get("plan_id").and_then(|v| v.as_str()))
        .or_else(|| data.get("module_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    (entity_type, entity_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_entity_prefers_explicit_type_and_id() {
        let data = json!({"entity_type": "order", "entity_id": "42"});
        assert_eq!(
            derive_entity("order.paid", &data),
            ("order".to_string(), "42".to_string())
        );
        let with_id = json!({"entity_type": "order", "id": "7"});
        assert_eq!(
            derive_entity("order.paid", &with_id),
            ("order".to_string(), "7".to_string())
        );
    }

    #[test]
    fn derive_entity_parses_resource_values() {
        let data = json!({"resource": "invoice:99"});
        assert_eq!(
            derive_entity("invoice.paid", &data),
            ("invoice".to_string(), "99".to_string())
        );
        let bare = json!({"resource": "nocolon"});
        assert_eq!(
            derive_entity("user.created", &bare),
            ("user.created".to_string(), "nocolon".to_string())
        );
    }

    #[test]
    fn derive_entity_falls_back_to_event_prefix_and_id_keys() {
        let data = json!({"id": "7"});
        assert_eq!(
            derive_entity("order.created", &data),
            ("order".to_string(), "7".to_string())
        );
        assert_eq!(
            derive_entity("payment.failed", &json!({})),
            ("payment".to_string(), String::new())
        );
    }

    #[test]
    fn source_for_families() {
        assert_eq!(source_for("task.created"), "task");
        assert_eq!(source_for("task.comment.added"), "task");
        assert_eq!(source_for("security.auth.failed"), "security");
        assert_eq!(source_for("cluster.sync.failed"), "cluster");
        assert_eq!(source_for("payment.failed"), "payment");
        assert_eq!(source_for("auth.failed"), "audit");
        assert_eq!(source_for("user.created"), "record");
        assert_eq!(source_for("order.created"), "record");
    }

    #[test]
    fn record_stores_only_the_whitelisted_summary() {
        let state = TimelineState::new();
        let entry = state.record(
            "user.created",
            &json!({
                "actor_id": "alice",
                "summary": "User alice created",
                "entity_type": "user",
                "entity_id": "u-9",
                "ip": "10.0.0.9",
                "password": "s3cr3t",
                "token": "tok-123",
                "email": "alice@example.com",
                "nested": {"secret": "also-secret"},
            }),
        );
        let details = entry.details.as_object().unwrap();
        assert_eq!(details.len(), 4);
        assert_eq!(entry.details["summary"], "User alice created");
        assert_eq!(entry.details["entity_type"], "user");
        assert_eq!(entry.details["entity_id"], "u-9");
        assert_eq!(entry.details["actor"], "alice");
        for leaked in ["ip", "password", "token", "email", "nested"] {
            assert!(details.get(leaked).is_none(), "{leaked} must not be stored");
        }
        // IPs are not retained in the timeline cache at all.
        assert_eq!(entry.ip, None);
        assert_eq!(entry.actor, "alice");
        assert_eq!(entry.action, "user.created");
    }

    #[test]
    fn record_resolves_actor_and_increments_sequence() {
        let state = TimelineState::new();
        let first = state.record(
            "task.created",
            &json!({"actor_id": "a1", "actor": "a2", "user_id": "a3"}),
        );
        assert_eq!(first.actor, "a1");
        assert_eq!(first.seq, 1);
        assert_eq!(first.action, "task.created");
        assert_eq!(first.source, "task");
        let second = state.record("user.created", &json!({"user_id": "u9"}));
        assert_eq!(second.actor, "u9");
        assert_eq!(second.seq, 2);
        let fallback = state.record("payment.failed", &json!({}));
        assert_eq!(fallback.actor, "system");
        assert!(fallback.summary.contains("payment.failed"));
        assert_eq!(state.all_entries().len(), 3);
    }

    #[test]
    fn evict_oldest_caps_the_cache() {
        let state = TimelineState::new();
        for _ in 0..MAX_ENTRIES + 5 {
            state.record("task.created", &json!({"actor_id": "bulk"}));
        }
        assert_eq!(state.all_entries().len(), MAX_ENTRIES);
        let min_seq = state
            .all_entries()
            .iter()
            .map(|entry| entry.seq)
            .min()
            .unwrap();
        assert!(min_seq > 5);
    }
}
