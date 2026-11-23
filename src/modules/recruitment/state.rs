//! Recruitment - In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, helpers::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for recruitment. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000051_recruitment`; until that migration is
/// executed, recruitment data does not survive a restart.
#[derive(Clone)]
pub struct RecruitmentState {
    pub jobs: Arc<DashMap<Uuid, Job>>,
    pub applicants: Arc<DashMap<Uuid, Applicant>>,
    pub stage_history: Arc<DashMap<Uuid, ApplicantStageHistory>>,
    pub interviews: Arc<DashMap<Uuid, Interview>>,
    pub evaluations: Arc<DashMap<Uuid, Evaluation>>,
    pub offers: Arc<DashMap<Uuid, Offer>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for RecruitmentState {
    fn default() -> Self {
        Self {
            jobs: Arc::new(DashMap::new()),
            applicants: Arc::new(DashMap::new()),
            stage_history: Arc::new(DashMap::new()),
            interviews: Arc::new(DashMap::new()),
            evaluations: Arc::new(DashMap::new()),
            offers: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl RecruitmentState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id`, `title`, `body`,
    /// `module: "recruitment"`, `link`, `priority`, `channels`.
    /// See `notifications.rs` `on_notification_requested` and
    /// `resolve_recipients`: a payload needs `user_id`, `user_ids`,
    /// or `audience`.
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "recruitment",
                "category": "recruitment",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
