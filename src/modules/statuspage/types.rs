//! Public data types, request/response shapes and their impls.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub const OVERALL_OPERATIONAL: &str = "operational";
pub const OVERALL_DEGRADED: &str = "degraded";
pub const OVERALL_OUTAGE: &str = "outage";
pub const OVERALL_MAINTENANCE: &str = "maintenance";

pub const COMPONENT_OPERATIONAL: &str = "operational";
pub const COMPONENT_DEGRADED: &str = "degraded";
pub const COMPONENT_PARTIAL_OUTAGE: &str = "partial_outage";
pub const COMPONENT_MAJOR_OUTAGE: &str = "major_outage";
pub const COMPONENT_MAINTENANCE: &str = "maintenance";

pub const INCIDENT_INVESTIGATING: &str = "investigating";
pub const INCIDENT_IDENTIFIED: &str = "identified";
pub const INCIDENT_MONITORING: &str = "monitoring";
pub const INCIDENT_RESOLVED: &str = "resolved";

/// Capability a caller must hold (via the `role.check` hook) to use
/// any statuspage write route.
pub const PERM_STATUSPAGE_MANAGE: &str = "statuspage.manage";

/// Length caps for user-supplied text, enforced on both create and
/// update paths.
pub(crate) const MAX_INCIDENT_TITLE_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusComponent {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub group: Option<String>,
    pub status: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUptime {
    pub component_id: Uuid,
    /// Calendar day in `YYYY-MM-DD` (UTC).
    pub date: String,
    pub uptime_percent: f64,
    pub minutes_down: i64,
    pub total_minutes: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: String,
    pub severity: String,
    pub affected_components: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    /// Incident status after this update.
    pub status: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateComponentRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateComponentRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordUptimeRequest {
    /// Calendar day in `YYYY-MM-DD` (UTC). Defaults to today.
    #[serde(default)]
    pub date: Option<String>,
    pub uptime_percent: f64,
    /// Minutes down that day. When present it is authoritative and
    /// `uptime_percent` is derived from it; when absent it is
    /// derived from `uptime_percent`. Either way the stored pair is
    /// always consistent (see `derive_uptime`).
    #[serde(default)]
    pub minutes_down: Option<i64>,
    /// Replace an existing aggregate for the same component and day.
    /// Without it, recording a second aggregate for a day is a 409.
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateIncidentRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub affected_components: Vec<Uuid>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateIncidentRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub affected_components: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddIncidentUpdateRequest {
    pub message: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentStatusView {
    #[serde(flatten)]
    pub component: StatusComponent,
    pub uptime_30d: f64,
    pub uptime_90d: f64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct StatusPageState {
    // Visible crate-wide: the sibling handler/helper modules read
    // these stores and call these methods; without `pub(crate)`
    // they could not (private items are only visible inside this
    // file's module).
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) components: DashMap<Uuid, StatusComponent>,
    /// Keyed by `{component_id}:{date}`.
    pub(crate) uptime: DashMap<String, DailyUptime>,
    pub(crate) incidents: DashMap<Uuid, Incident>,
    pub(crate) updates: DashMap<Uuid, IncidentUpdate>,
    /// Last derived overall status, used to emit
    /// `statuspage.status.changed` only on real changes.
    last_overall: RwLock<String>,
}

impl StatusPageState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            components: DashMap::new(),
            uptime: DashMap::new(),
            incidents: DashMap::new(),
            updates: DashMap::new(),
            last_overall: RwLock::new(OVERALL_OPERATIONAL.to_string()),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Last derived overall status, for tests that need to observe
    /// whether a handler refreshed it (without going through a
    /// public read, which refreshes on its own).
    #[cfg(test)]
    pub(crate) fn last_overall(&self) -> String {
        self.last_overall.read().expect("overall lock poisoned").clone()
    }

    /// Derive the overall status from the maintenance flag, open
    /// incidents, and component states.
    fn derive_overall(&self, ctx: &AppContext) -> String {
        if ctx.is_maintenance() {
            return OVERALL_MAINTENANCE.to_string();
        }
        let mut worst = OVERALL_OPERATIONAL.to_string();
        for entry in self.incidents.iter() {
            let incident = entry.value();
            if !is_open_incident(&incident.status) {
                continue;
            }
            match incident.severity.as_str() {
                "critical" => return OVERALL_OUTAGE.to_string(),
                _ => worst = OVERALL_DEGRADED.to_string(),
            }
        }
        for entry in self.components.iter() {
            match entry.value().status.as_str() {
                COMPONENT_MAJOR_OUTAGE => return OVERALL_OUTAGE.to_string(),
                COMPONENT_PARTIAL_OUTAGE | COMPONENT_DEGRADED => {
                    worst = OVERALL_DEGRADED.to_string();
                }
                _ => {}
            }
        }
        worst
    }

    /// Recompute the overall status and emit
    /// `statuspage.status.changed` when it moved.
    pub(crate) fn refresh_overall(&self, ctx: &AppContext) -> String {
        let overall = self.derive_overall(ctx);
        let mut last = self.last_overall.write().expect("overall lock poisoned");
        if *last != overall {
            let previous = last.clone();
            *last = overall.clone();
            drop(last);
            self.emit(
                "statuspage.status.changed",
                json!({ "previous": previous, "current": overall }),
            );
        }
        overall
    }

    fn uptime_for_range(&self, component_id: Uuid, days: i64) -> (f64, u64) {
        let today = Utc::now().date_naive();
        let mut sum = 0f64;
        let mut points = 0u64;
        for offset in 0..days {
            let date = today - chrono::Duration::days(offset);
            let key = uptime_key(component_id, &date);
            if let Some(entry) = self.uptime.get(&key) {
                sum += entry.value().uptime_percent;
                points += 1;
            }
        }
        if points == 0 {
            (100.0, 0)
        } else {
            (sum / points as f64, points)
        }
    }

    pub(crate) fn component_view(&self, component: &StatusComponent) -> ComponentStatusView {
        let (uptime_30d, _) = self.uptime_for_range(component.id, 30);
        let (uptime_90d, _) = self.uptime_for_range(component.id, 90);
        ComponentStatusView {
            component: component.clone(),
            uptime_30d: round_percent(uptime_30d),
            uptime_90d: round_percent(uptime_90d),
        }
    }

    /// Add downtime minutes to a component's aggregate for one day,
    /// recomputing its uptime percentage from the minutes.
    fn add_downtime(&self, component_id: Uuid, date: NaiveDate, minutes_down: i64) {
        if minutes_down <= 0 {
            return;
        }
        let minutes_down = minutes_down.min(1440);
        let key = uptime_key(component_id, &date);
        let mut entry = self.uptime.entry(key).or_insert_with(|| DailyUptime {
            component_id,
            date: date.format("%Y-%m-%d").to_string(),
            uptime_percent: 100.0,
            minutes_down: 0,
            total_minutes: 1440,
            updated_at: Utc::now(),
        });
        entry.minutes_down = (entry.minutes_down + minutes_down).min(1440);
        entry.uptime_percent =
            round_percent(100.0 * (1440 - entry.minutes_down) as f64 / 1440.0);
        entry.updated_at = Utc::now();
    }

    /// Spread an incident's open time across the days it covers as
    /// downtime for every affected component.
    pub(crate) fn record_incident_downtime(&self, incident: &Incident, until: DateTime<Utc>) {
        if incident.affected_components.is_empty() {
            return;
        }
        let start = incident.created_at;
        if until <= start {
            return;
        }
        let mut day = start.date_naive();
        let last_day = until.date_naive();
        loop {
            let day_start = day.and_hms_opt(0, 0, 0).expect("valid midnight");
            let day_end = day_start + chrono::Duration::days(1);
            let from = start.max(day_start.and_utc());
            let to = until.min(day_end.and_utc());
            let minutes = (to - from).num_minutes().max(0);
            for component_id in &incident.affected_components {
                self.add_downtime(*component_id, day, minutes);
            }
            if day >= last_day {
                break;
            }
            day += chrono::Duration::days(1);
        }
    }
}
