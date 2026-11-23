// Inbox - In-memory inbox state.

use unified_core::hooks::HookBus;
use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::{InboxAssignment, InboxCannedReply, InboxThread};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the inbox. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000054_inbox`; until that migration is executed,
/// inbox threads do not survive a restart. `reads` maps
/// `"{thread_id}:{user_id}"` to the time that user last marked the
/// thread read; it backs the per-agent `unread` value and the
/// `unread` queue filter.
#[derive(Clone)]
pub struct InboxState {
    pub threads: Arc<DashMap<Uuid, InboxThread>>,
    pub assignments: Arc<DashMap<Uuid, InboxAssignment>>,
    pub canned_replies: Arc<DashMap<Uuid, InboxCannedReply>>,
    pub reads: Arc<DashMap<String, DateTime<Utc>>>,
    /// One mutex per source reference (`(source_channel,
    /// source_id)`), so the find-then-insert in `upsert_thread`
    /// (and the duplicate check in `register_thread`) is atomic
    /// for that source: concurrent ticket/chat/mail events for the
    /// same source serialize on the same lock instead of creating
    /// two live threads. Only one source lock is ever held at a
    /// time, so no lock ordering hazard exists. Locks are never
    /// removed; one entry per distinct source is bounded by the
    /// thread index itself.
    pub upsert_locks: Arc<DashMap<(String, String), Arc<std::sync::Mutex<()>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for InboxState {
    fn default() -> Self {
        Self {
            threads: Arc::new(DashMap::new()),
            assignments: Arc::new(DashMap::new()),
            canned_replies: Arc::new(DashMap::new()),
            reads: Arc::new(DashMap::new()),
            upsert_locks: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl InboxState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "inbox",
                "category": "inbox",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Read receipt key for one agent on one thread.
    pub(crate) fn read_key(thread_id: Uuid, user_id: &str) -> String {
        format!("{thread_id}:{user_id}")
    }

    /// True when `user_id` has not read the thread since its last
    /// activity (no receipt, or a receipt older than last activity).
    pub(crate) fn unread_for(&self, thread: &InboxThread, user_id: &str) -> bool {
        match self.reads.get(&Self::read_key(thread.id, user_id)) {
            Some(receipt) => *receipt.value() < thread.last_activity_at,
            None => true,
        }
    }
}
