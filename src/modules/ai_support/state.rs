// AI support state.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::*;

// Shared state

/// In-memory stores for AI Support. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); grounding and handoff degrade honestly
/// when no bus is attached: questions report that no articles were found
/// and handoffs report that no ticket could be requested.
#[derive(Clone, Default)]
pub struct AiSupportState {
    pub conversations: Arc<DashMap<Uuid, Conversation>>,
    pub messages: Arc<DashMap<Uuid, Vec<ConversationMessage>>>,
    pub connectors: Arc<DashMap<Uuid, Connector>>,
    /// Serializes the connector name-uniqueness check plus insert
    /// (create and rename). Holding this lock across scan-and-insert
    /// makes "same name created twice" impossible: two concurrent
    /// creates with one name cannot both pass the check.
    pub connector_write: Arc<Mutex<()>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl AiSupportState {
    pub(crate) fn emit(&self, event: &str, data: Value) -> Option<unified_core::hooks::HookContext> {
        self.hooks
            .read()
            .expect("hooks lock poisoned")
            .clone()
            .map(|bus| bus.emit(event, data))
    }
}
