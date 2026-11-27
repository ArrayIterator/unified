// records - In-memory state and its methods.

use super::*;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

pub struct RecordsState {
    pub(crate) records: DashMap<String, SystemRecord>,
    pub(crate) seq: AtomicU64,
}

impl RecordsState {
    pub(crate) fn new() -> Self {
        Self {
            records: DashMap::new(),
            seq: AtomicU64::new(0),
        }
    }

    /// Append one record derived from a hook event payload. This is
    /// the only way records are created.
    pub(crate) fn record(&self, event: &str, data: &Value) -> SystemRecord {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let actor = data
            .get("actor_id")
            .or_else(|| data.get("actor"))
            .or_else(|| data.get("user_id"))
            .and_then(|value| value.as_str())
            .unwrap_or("system")
            .to_string();
        let resource = data
            .get("resource")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let (entity_type, entity_id) = derive_entity(event, data, resource.as_deref());
        let summary = data
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{event} on {entity_type}:{entity_id}"));
        let ip = data
            .get("ip")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let record = SystemRecord {
            id: Uuid::new_v4().to_string(),
            seq,
            actor,
            action: event.to_string(),
            entity_type,
            entity_id,
            summary,
            details: data.clone(),
            ip,
            created_at: Utc::now(),
        };
        self.records.insert(record.id.clone(), record.clone());
        record
    }
}
