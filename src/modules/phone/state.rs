//! In-memory phone store.

use super::types::{
    AutoResponseScript, CallParticipant, CallRecording, IvrMenu, IvrNode, PhoneCall, PhoneQueue,
    PhoneSettings, PresenceEntry, QueueMember, QueueWaiter, RoutingRule, SignalMessage,
};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the phone module. The hook bus is
/// attached in `init` (the bus lives in `AppContext`);
/// handlers tolerate a missing bus by skipping event emission.
/// Durable rows for these types are defined by migration
/// `2026-11-22-000116_phone`; until that migration is executed,
/// phone data does not survive a restart.
#[derive(Clone)]
pub struct PhoneState {
    pub calls: Arc<DashMap<Uuid, PhoneCall>>,
    pub participants: Arc<DashMap<Uuid, CallParticipant>>,
    pub signals: Arc<DashMap<Uuid, SignalMessage>>,
    pub queues: Arc<DashMap<Uuid, PhoneQueue>>,
    pub queue_members: Arc<DashMap<Uuid, QueueMember>>,
    pub queue_waiters: Arc<DashMap<Uuid, QueueWaiter>>,
    pub routing_rules: Arc<DashMap<Uuid, RoutingRule>>,
    pub presence: Arc<DashMap<String, PresenceEntry>>,
    pub ivr_menus: Arc<DashMap<Uuid, IvrMenu>>,
    pub ivr_nodes: Arc<DashMap<Uuid, IvrNode>>,
    pub scripts: Arc<DashMap<Uuid, AutoResponseScript>>,
    pub recordings: Arc<DashMap<Uuid, CallRecording>>,
    pub settings: Arc<DashMap<String, PhoneSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PhoneState {
    fn default() -> Self {
        Self {
            calls: Arc::new(DashMap::new()),
            participants: Arc::new(DashMap::new()),
            signals: Arc::new(DashMap::new()),
            queues: Arc::new(DashMap::new()),
            queue_members: Arc::new(DashMap::new()),
            queue_waiters: Arc::new(DashMap::new()),
            routing_rules: Arc::new(DashMap::new()),
            presence: Arc::new(DashMap::new()),
            ivr_menus: Arc::new(DashMap::new()),
            ivr_nodes: Arc::new(DashMap::new()),
            scripts: Arc::new(DashMap::new()),
            recordings: Arc::new(DashMap::new()),
            settings: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PhoneState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "phone",
                "category": "phone",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Per-tenant settings, defaulting to
    /// `PhoneSettings::default` (recording consent required)
    /// when the tenant has no row yet.
    pub(crate) fn current_settings(&self, tenant_id: &str) -> PhoneSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    pub(crate) fn save_settings(&self, tenant_id: &str, settings: PhoneSettings) {
        self.settings.insert(tenant_id.to_string(), settings);
    }

    /// Participants of one call, in creation order.
    pub(crate) fn call_participants(&self, call_id: Uuid) -> Vec<CallParticipant> {
        let mut rows: Vec<CallParticipant> = self
            .participants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|participant| participant.call_id == call_id)
            .collect();
        rows.sort_by_key(|participant| participant.created_at);
        rows
    }

    /// Signalling messages of one call, in creation order.
    pub(crate) fn call_signals(&self, call_id: Uuid) -> Vec<SignalMessage> {
        let mut rows: Vec<SignalMessage> = self
            .signals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|signal| signal.call_id == call_id)
            .collect();
        rows.sort_by_key(|signal| signal.created_at);
        rows
    }

    /// Active members of one queue, in ring position order.
    pub(crate) fn queue_members(&self, queue_id: Uuid) -> Vec<QueueMember> {
        let mut rows: Vec<QueueMember> = self
            .queue_members
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|member| member.queue_id == queue_id)
            .collect();
        rows.sort_by_key(|member| member.position);
        rows
    }

    /// Callers currently waiting in one queue, oldest first.
    pub(crate) fn queue_waiting(&self, queue_id: Uuid) -> Vec<QueueWaiter> {
        let mut rows: Vec<QueueWaiter> = self
            .queue_waiters
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|waiter| waiter.queue_id == queue_id && waiter.status == "waiting")
            .collect();
        rows.sort_by_key(|waiter| waiter.joined_at);
        rows
    }

    /// Active routing rules of a tenant, in priority order.
    pub(crate) fn active_routing_rules(&self, tenant_id: &str) -> Vec<RoutingRule> {
        let mut rows: Vec<RoutingRule> = self
            .routing_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id && rule.active)
            .collect();
        rows.sort_by_key(|rule| rule.priority);
        rows
    }

    /// Nodes of one IVR menu, in creation order.
    pub(crate) fn menu_nodes(&self, menu_id: Uuid) -> Vec<IvrNode> {
        let mut rows: Vec<IvrNode> = self
            .ivr_nodes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|node| node.menu_id == menu_id)
            .collect();
        rows.sort_by_key(|node| node.created_at);
        rows
    }

    /// Active auto-response scripts of a tenant, in priority
    /// order (lowest first).
    pub(crate) fn active_scripts(&self, tenant_id: &str) -> Vec<AutoResponseScript> {
        let mut rows: Vec<AutoResponseScript> = self
            .scripts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|script| script.tenant_id == tenant_id && script.active)
            .collect();
        rows.sort_by_key(|script| script.priority);
        rows
    }

    /// Recordings of one call, in creation order.
    pub(crate) fn call_recordings(&self, call_id: Uuid) -> Vec<CallRecording> {
        let mut rows: Vec<CallRecording> = self
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|recording| recording.call_id == call_id)
            .collect();
        rows.sort_by_key(|recording| recording.created_at);
        rows
    }

    /// A queue by its code within a tenant, if one exists.
    pub(crate) fn queue_by_code(&self, tenant_id: &str, code: &str) -> Option<PhoneQueue> {
        self.queues
            .iter()
            .map(|entry| entry.value().clone())
            .find(|queue| queue.tenant_id == tenant_id && queue.code == code)
    }

    /// Remove every presence row of one user, across tenants.
    /// Used by the `user.deleted` hook: presence is live state,
    /// not history, so the row goes away instead of being
    /// anonymized.
    pub(crate) fn remove_presence_for_user(&self, user_id: &str) -> i64 {
        let keys: Vec<String> = self
            .presence
            .iter()
            .map(|entry| entry.key().clone())
            .filter(|key| key.ends_with(&format!(":{user_id}")))
            .collect();
        let removed = keys.len() as i64;
        for key in keys {
            self.presence.remove(&key);
        }
        removed
    }

    /// An IVR menu by its code within a tenant, if one exists.
    pub(crate) fn menu_by_code(&self, tenant_id: &str, code: &str) -> Option<IvrMenu> {
        self.ivr_menus
            .iter()
            .map(|entry| entry.value().clone())
            .find(|menu| menu.tenant_id == tenant_id && menu.code == code)
    }
}
