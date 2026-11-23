// Gamification — Shared state.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for gamification. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000058_gamification`; until that migration is
/// executed, gamification data does not survive a restart.
#[derive(Clone)]
pub struct GamificationState {
    pub rules: Arc<DashMap<Uuid, GamificationRule>>,
    pub ledger: Arc<DashMap<Uuid, PointsLedgerEntry>>,
    pub badges: Arc<DashMap<Uuid, GamificationBadge>>,
    pub awards: Arc<DashMap<Uuid, GamificationAward>>,
    pub seasons: Arc<DashMap<Uuid, GamificationSeason>>,
    pub rewards: Arc<DashMap<Uuid, GamificationReward>>,
    pub reward_requests: Arc<DashMap<Uuid, GamificationRewardRequest>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for GamificationState {
    fn default() -> Self {
        Self {
            rules: Arc::new(DashMap::new()),
            ledger: Arc::new(DashMap::new()),
            badges: Arc::new(DashMap::new()),
            awards: Arc::new(DashMap::new()),
            seasons: Arc::new(DashMap::new()),
            rewards: Arc::new(DashMap::new()),
            reward_requests: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl GamificationState {
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
                "module": "gamification",
                "category": "gamification",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
