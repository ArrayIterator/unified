// Diesel models for the maintenance_status domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Maintenance, status page, announcements, and auth (2026-11-22-000019_maintenance_statuspage_announcements_auth).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = maintenance_windows)]
pub struct MaintenanceWindow {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub affected_services: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = maintenance_windows)]
pub struct NewMaintenanceWindow {
    pub id: Uuid,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub activated_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = maintenance_history)]
pub struct MaintenanceHistory {
    pub id: Uuid,
    pub window_id: Uuid,
    pub event: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = maintenance_history)]
pub struct NewMaintenanceHistory {
    pub id: Uuid,
    pub window_id: Uuid,
    pub event: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_components)]
pub struct StatusComponent {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub group: Option<String>,
    pub status: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_components)]
pub struct NewStatusComponent {
    pub id: Uuid,
    pub name: String,
    pub group: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_daily_uptime)]
pub struct StatusDailyUptime {
    pub component_id: Uuid,
    pub date: String,
    pub uptime_percent: f64,
    pub minutes_down: i64,
    pub total_minutes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_daily_uptime)]
pub struct NewStatusDailyUptime {
    pub component_id: Uuid,
    pub date: String,
    pub uptime_percent: f64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_incidents)]
pub struct StatusIncident {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: String,
    pub severity: String,
    pub affected_components: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_incidents)]
pub struct NewStatusIncident {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub severity: String,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_incident_updates)]
pub struct StatusIncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub status: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_incident_updates)]
pub struct NewStatusIncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub status: String,
    pub message: String,
}
