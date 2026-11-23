// Elearning — Shared state.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for e-learning. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000076_elearning`; until that migration is
/// executed, e-learning data does not survive a restart.
#[derive(Clone)]
pub struct ElearningState {
    pub courses: Arc<DashMap<Uuid, Course>>,
    pub modules: Arc<DashMap<Uuid, CourseModule>>,
    pub lessons: Arc<DashMap<Uuid, Lesson>>,
    pub enrollments: Arc<DashMap<Uuid, Enrollment>>,
    pub progress: Arc<DashMap<Uuid, LessonProgress>>,
    pub quizzes: Arc<DashMap<Uuid, Quiz>>,
    pub questions: Arc<DashMap<Uuid, QuizQuestion>>,
    pub attempts: Arc<DashMap<Uuid, QuizAttempt>>,
    /// Quiz clocks: when a learner first fetched a timed quiz's
    /// questions, keyed by (quiz_id, user_id). The entry is
    /// removed once the attempt is submitted, so a fresh attempt
    /// starts a fresh clock.
    pub quiz_attempt_starts: Arc<DashMap<(Uuid, String), DateTime<Utc>>>,
    pub certificates: Arc<DashMap<Uuid, Certificate>>,
    pub reviews: Arc<DashMap<Uuid, Review>>,
    pub paths: Arc<DashMap<Uuid, LearningPath>>,
    pub path_courses: Arc<DashMap<Uuid, LearningPathCourse>>,
    pub settings: Arc<RwLock<ElearningSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ElearningState {
    fn default() -> Self {
        Self {
            courses: Arc::new(DashMap::new()),
            modules: Arc::new(DashMap::new()),
            lessons: Arc::new(DashMap::new()),
            enrollments: Arc::new(DashMap::new()),
            progress: Arc::new(DashMap::new()),
            quizzes: Arc::new(DashMap::new()),
            questions: Arc::new(DashMap::new()),
            attempts: Arc::new(DashMap::new()),
            quiz_attempt_starts: Arc::new(DashMap::new()),
            certificates: Arc::new(DashMap::new()),
            reviews: Arc::new(DashMap::new()),
            paths: Arc::new(DashMap::new()),
            path_courses: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ElearningSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ElearningState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> ElearningSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "elearning",
                "category": "elearning",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
