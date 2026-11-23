// surveys module — Shared state.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::{Survey, SurveyAnswer, SurveyDistribution, SurveyQuestion, SurveyResponse};
use super::requests::DistributionRecipient;
use super::helpers::created;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for surveys. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000055_surveys`; until that
/// migration is executed, surveys do not survive a restart.
#[derive(Clone)]
pub struct SurveysState {
    pub surveys: Arc<DashMap<Uuid, Survey>>,
    pub questions: Arc<DashMap<Uuid, SurveyQuestion>>,
    pub distributions: Arc<DashMap<Uuid, SurveyDistribution>>,
    pub responses: Arc<DashMap<Uuid, SurveyResponse>>,
    pub answers: Arc<DashMap<Uuid, SurveyAnswer>>,
    /// Recipients supplied at distribution creation, waiting for
    /// the send (manual distributions send them on
    /// `POST /distributions/{id}/send`; scheduled distributions
    /// send them when the `surveys.send` job fires). Keyed by
    /// distribution id; cleared once the invitations are issued.
    pub pending_recipients: Arc<DashMap<Uuid, Vec<DistributionRecipient>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SurveysState {
    fn default() -> Self {
        Self {
            surveys: Arc::new(DashMap::new()),
            questions: Arc::new(DashMap::new()),
            distributions: Arc::new(DashMap::new()),
            responses: Arc::new(DashMap::new()),
            answers: Arc::new(DashMap::new()),
            pending_recipients: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SurveysState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit an event and return the final hook context, so callers
    /// can read handler results (for example the task created by
    /// the tasks module in answer to `task.create_requested`).
    /// With no bus, returns an empty context for the event.
    pub(crate) fn emit_ctx(&self, event: &str, data: Value) -> HookContext {
        match self.bus() {
            Some(bus) => bus.emit(event, data),
            None => HookContext::new(event, data),
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
                "module": "surveys",
                "category": "surveys",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
