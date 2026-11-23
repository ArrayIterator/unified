//! In-memory forums store.

use super::types::{
    Forum, ForumCategory, ForumModerationAction, ForumPoll, ForumPollOption, ForumPollVote,
    ForumPost, ForumPostEdit, ForumReport, ForumReputation, ForumSettings, ForumSubscription,
    ForumThread, ForumVote,
};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the forums. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000110_forums`; until that migration is executed,
/// forums data does not survive a restart.
#[derive(Clone)]
pub struct ForumState {
    pub forums: Arc<DashMap<Uuid, Forum>>,
    pub categories: Arc<DashMap<Uuid, ForumCategory>>,
    pub threads: Arc<DashMap<Uuid, ForumThread>>,
    pub posts: Arc<DashMap<Uuid, ForumPost>>,
    pub post_edits: Arc<DashMap<Uuid, ForumPostEdit>>,
    pub votes: Arc<DashMap<Uuid, ForumVote>>,
    pub reports: Arc<DashMap<Uuid, ForumReport>>,
    pub moderation_actions: Arc<DashMap<Uuid, ForumModerationAction>>,
    pub subscriptions: Arc<DashMap<Uuid, ForumSubscription>>,
    pub polls: Arc<DashMap<Uuid, ForumPoll>>,
    pub poll_options: Arc<DashMap<Uuid, ForumPollOption>>,
    pub poll_votes: Arc<DashMap<Uuid, ForumPollVote>>,
    pub reputations: Arc<DashMap<Uuid, ForumReputation>>,
    pub settings: Arc<RwLock<ForumSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ForumState {
    fn default() -> Self {
        Self {
            forums: Arc::new(DashMap::new()),
            categories: Arc::new(DashMap::new()),
            threads: Arc::new(DashMap::new()),
            posts: Arc::new(DashMap::new()),
            post_edits: Arc::new(DashMap::new()),
            votes: Arc::new(DashMap::new()),
            reports: Arc::new(DashMap::new()),
            moderation_actions: Arc::new(DashMap::new()),
            subscriptions: Arc::new(DashMap::new()),
            polls: Arc::new(DashMap::new()),
            poll_options: Arc::new(DashMap::new()),
            poll_votes: Arc::new(DashMap::new()),
            reputations: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ForumSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ForumState {
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
                "module": "forums",
                "category": "forums",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn current_settings(&self) -> ForumSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}
