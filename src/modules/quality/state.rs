//! quality module - In-memory quality state.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::Capa;
use super::types::Inspection;
use super::types::QualityAlert;
use super::types::QualityCriterion;
use super::types::QualityPoint;
use super::types::QualitySettings;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct QualityState {
    pub points: Arc<DashMap<Uuid, QualityPoint>>,
    pub criteria: Arc<DashMap<Uuid, QualityCriterion>>,
    pub inspections: Arc<DashMap<Uuid, Inspection>>,
    pub alerts: Arc<DashMap<Uuid, QualityAlert>>,
    pub capas: Arc<DashMap<Uuid, Capa>>,
    pub settings: Arc<RwLock<QualitySettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for QualityState {
    fn default() -> Self {
        Self {
            points: Arc::new(DashMap::new()),
            criteria: Arc::new(DashMap::new()),
            inspections: Arc::new(DashMap::new()),
            alerts: Arc::new(DashMap::new()),
            capas: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(QualitySettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl QualityState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> QualitySettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
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
                "module": "quality",
                "category": "quality",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn criteria_for_point(&self, point_id: Uuid) -> Vec<QualityCriterion> {
        let mut rows: Vec<QualityCriterion> = self
            .criteria
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.point_id == point_id)
            .collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        rows
    }
}
