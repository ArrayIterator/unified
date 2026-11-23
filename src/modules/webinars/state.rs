// webinars -- Shared in-memory state.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::types::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for webinars. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission, except the paid entitlement check,
/// which fails closed when there is no bus to ask. Durable rows for
/// these types are defined by migration 000077; until that migration
/// is applied and a persistence writer lands, webinars do not
/// survive a restart.
#[derive(Clone)]
pub struct WebinarsState {
    pub webinars: Arc<DashMap<Uuid, Webinar>>,
    pub registrations: Arc<DashMap<Uuid, WebinarRegistration>>,
    pub attendance: Arc<DashMap<Uuid, WebinarAttendance>>,
    pub polls: Arc<DashMap<Uuid, WebinarPoll>>,
    /// Keyed by `"{poll_id}:{user_id}"`.
    pub votes: Arc<DashMap<String, WebinarPollVote>>,
    pub questions: Arc<DashMap<Uuid, WebinarQuestion>>,
    pub recordings: Arc<DashMap<Uuid, WebinarRecording>>,
    /// Keyed by `"{webinar_id}:{user_id}"`.
    pub mutes: Arc<DashMap<String, WebinarMuteState>>,
    /// Per-webinar seat locks: one mutex per webinar id, taken
    /// around the capacity check-and-insert in registration so two
    /// concurrent registrations cannot both observe a free seat and
    /// both insert. Entries are created on demand and never removed;
    /// one small mutex per webinar is the bounded cost of the guard.
    pub registration_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
    pub config: Arc<RwLock<WebinarConfig>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for WebinarsState {
    fn default() -> Self {
        Self {
            webinars: Arc::new(DashMap::new()),
            registrations: Arc::new(DashMap::new()),
            attendance: Arc::new(DashMap::new()),
            polls: Arc::new(DashMap::new()),
            votes: Arc::new(DashMap::new()),
            questions: Arc::new(DashMap::new()),
            recordings: Arc::new(DashMap::new()),
            mutes: Arc::new(DashMap::new()),
            registration_locks: Arc::new(DashMap::new()),
            config: Arc::new(RwLock::new(WebinarConfig::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl WebinarsState {
    /// The seat lock for one webinar, creating it on first use.
    /// Callers hold the returned mutex across the capacity
    /// check-and-insert so the seat count cannot change under them.
    pub(crate) fn registration_lock(&self, webinar_id: Uuid) -> Arc<Mutex<()>> {
        self.registration_locks
            .entry(webinar_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .value()
            .clone()
    }

    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn bus_has(&self, event: &str) -> bool {
        self.bus().is_some_and(|bus| bus.has(event))
    }

    pub(crate) fn config(&self) -> WebinarConfig {
        self.config.read().expect("config lock poisoned").clone()
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape. Delivery is owned by the
    /// notifications module; when no handler is registered the event
    /// is simply not delivered, and this module never pretends
    /// otherwise.
    pub(crate) fn notify(&self, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": "webinar",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "webinars",
                "category": "webinars",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
