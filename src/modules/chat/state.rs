//! chat - state.

use unified_core::context::AppContext;
use unified_core::hooks::{HookBus, HookContext};
use dashmap::DashMap;
use serde_json::{Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



#[derive(Clone)]
// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------
/// In-memory stores for chat. The hook bus is attached in `init` (the bus
/// lives in `AppContext`); handlers tolerate a missing bus by skipping
/// event emission, but still store and broadcast locally.
pub struct ChatState {
    pub channels: Arc<DashMap<Uuid, Channel>>,
    pub messages: Arc<DashMap<Uuid, ChatMessage>>,
    pub files: Arc<DashMap<Uuid, ChatFile>>,
    pub presence: Arc<DashMap<String, PresenceEntry>>,
    pub hub: Arc<ChatHub>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Live support sessions, keyed by session id.
    pub sessions: Arc<DashMap<Uuid, SupportSession>>,
    /// Support agent roster, keyed by agent user id.
    pub agents: Arc<DashMap<String, SupportAgent>>,
    /// Canned responses, keyed by canned response id.
    pub canned: Arc<DashMap<Uuid, CannedResponse>>,
    /// Chat ratings, keyed by rating id.
    pub ratings: Arc<DashMap<Uuid, ChatRating>>,
    /// Offline messages, keyed by message id.
    pub offline_messages: Arc<DashMap<Uuid, OfflineMessage>>,
    /// Widget configs, keyed by company id or `"default"`.
    pub widget_configs: Arc<DashMap<String, WidgetConfig>>,
}


impl Default for ChatState {
    fn default() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
            messages: Arc::new(DashMap::new()),
            files: Arc::new(DashMap::new()),
            presence: Arc::new(DashMap::new()),
            hub: Arc::new(ChatHub::new()),
            hooks: Arc::new(RwLock::new(None)),
            sessions: Arc::new(DashMap::new()),
            agents: Arc::new(DashMap::new()),
            canned: Arc::new(DashMap::new()),
            ratings: Arc::new(DashMap::new()),
            offline_messages: Arc::new(DashMap::new()),
            widget_configs: Arc::new(DashMap::new()),
        }
    }
}


impl ChatState {
    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.hooks.read().expect("hooks lock poisoned").clone() {
            bus.emit(event, data);
        }
    }

    /// Emit an event and return the final hook context, so callers can
    /// read handler results (for example the ticket created by the
    /// tickets module in answer to `ticket.create_requested`). With no
    /// bus attached, returns an empty context for the event.
    pub(crate) fn emit_ctx(&self, event: &str, data: Value) -> HookContext {
        match self.hooks.read().expect("hooks lock poisoned").clone() {
            Some(bus) => bus.emit(event, data),
            None => HookContext::new(event, data),
        }
    }
}
