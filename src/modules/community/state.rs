//! In-memory community store.

use unified_core::hooks::{HookBus};
use unified_core::json;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::types::{CommunityAnnouncement, CommunityCategory, CommunityFlag, CommunityMember, CommunityModerationAction, CommunityPost, CommunityPostEdit, CommunitySettings, CommunityTopic, CommunityVote};

// Shared state

/// In-memory stores for the community. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000056_community`; until that migration is executed,
/// community data does not survive a restart.
#[derive(Clone)]
pub struct CommunityState {
    pub categories: Arc<DashMap<Uuid, CommunityCategory>>,
    pub topics: Arc<DashMap<Uuid, CommunityTopic>>,
    pub posts: Arc<DashMap<Uuid, CommunityPost>>,
    pub post_edits: Arc<DashMap<Uuid, CommunityPostEdit>>,
    pub votes: Arc<DashMap<Uuid, CommunityVote>>,
    pub flags: Arc<DashMap<Uuid, CommunityFlag>>,
    pub moderation_actions: Arc<DashMap<Uuid, CommunityModerationAction>>,
    pub members: Arc<DashMap<Uuid, CommunityMember>>,
    pub announcements: Arc<DashMap<Uuid, CommunityAnnouncement>>,
    pub settings: Arc<RwLock<CommunitySettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for CommunityState {
    fn default() -> Self {
        Self {
            categories: Arc::new(DashMap::new()),
            topics: Arc::new(DashMap::new()),
            posts: Arc::new(DashMap::new()),
            post_edits: Arc::new(DashMap::new()),
            votes: Arc::new(DashMap::new()),
            flags: Arc::new(DashMap::new()),
            moderation_actions: Arc::new(DashMap::new()),
            members: Arc::new(DashMap::new()),
            announcements: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(CommunitySettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl CommunityState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "community",
                "category": "community",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn current_settings(&self) -> CommunitySettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}
