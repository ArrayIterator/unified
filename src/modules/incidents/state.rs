//! incidents - state.

use unified_core::hooks::HookBus;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, helpers::*, handlers_incidents::*, handlers_rotations::*, handlers_policies::*, handlers_postmortems::*, handlers_auto_open::*, module::*};



#[derive(Clone)]
// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------
/// In-memory stores for incidents. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration `2026-11-22-000053_incidents`;
/// until that migration is executed, incidents do not survive a
/// restart.
pub struct IncidentsState {
    pub incidents: Arc<DashMap<Uuid, Incident>>,
    pub timeline: Arc<DashMap<Uuid, IncidentTimelineEntry>>,
    pub rotations: Arc<DashMap<Uuid, OnCallRotation>>,
    pub schedules: Arc<DashMap<Uuid, OnCallSchedule>>,
    pub escalation_policies: Arc<DashMap<Uuid, EscalationPolicy>>,
    pub escalations: Arc<DashMap<Uuid, IncidentEscalation>>,
    pub page_attempts: Arc<DashMap<Uuid, PageAttempt>>,
    pub postmortems: Arc<DashMap<Uuid, IncidentPostmortem>>,
    pub auto_open_rules: Arc<DashMap<String, AutoOpenRule>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}


impl Default for IncidentsState {
    fn default() -> Self {
        Self {
            incidents: Arc::new(DashMap::new()),
            timeline: Arc::new(DashMap::new()),
            rotations: Arc::new(DashMap::new()),
            schedules: Arc::new(DashMap::new()),
            escalation_policies: Arc::new(DashMap::new()),
            escalations: Arc::new(DashMap::new()),
            page_attempts: Arc::new(DashMap::new()),
            postmortems: Arc::new(DashMap::new()),
            auto_open_rules: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}


impl IncidentsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape. Paging and escalation use high
    /// priority; new-incident notices use normal priority.
    pub(crate) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
        priority: &str,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "incidents",
                "category": "incidents",
                "priority": priority,
                "link": link,
            }),
        );
    }

    /// Append one row to an incident's timeline. The timeline is
    /// append-only: this is the only writer, and no handler ever
    /// updates or removes a row.
    pub(crate) fn add_timeline(&self, incident_id: Uuid, actor_id: &str, kind: &str, message: String) {
        self.timeline.insert(
            Uuid::now_v7(),
            IncidentTimelineEntry {
                id: Uuid::now_v7(),
                incident_id,
                actor_id: actor_id.to_string(),
                kind: kind.to_string(),
                message,
                created_at: Utc::now(),
            },
        );
    }
}
