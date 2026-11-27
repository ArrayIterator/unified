// Diesel models for the bi_portals domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Business Intelligence (migration 000042_bi).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bi_reports)]
pub struct BiReport {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub fields: Value,
    pub filters: Value,
    pub group_by: Option<String>,
    pub chart_type: String,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bi_reports)]
pub struct NewBiReport {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub group_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bi_report_schedules)]
pub struct BiReportSchedule {
    pub id: Uuid,
    pub report_id: Uuid,
    pub company_id: String,
    pub schedule: String,
    pub recipients: Value,
    pub channel: String,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub last_delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bi_report_schedules)]
pub struct NewBiReportSchedule {
    pub id: Uuid,
    pub report_id: Uuid,
    pub company_id: String,
    pub schedule: String,
    pub scheduler_job_id: Option<String>,
    pub last_delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bi_kpi_dashboards)]
pub struct BiKpiDashboard {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
    pub kpis: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bi_kpi_dashboards)]
pub struct NewBiKpiDashboard {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
}

// Client portals (migration 000046_portals).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = portals)]
pub struct Portal {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub slug: String,
    pub custom_domain: Option<String>,
    pub domain_status: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub primary_color: String,
    pub secondary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    pub sections: Vec<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = portals)]
pub struct NewPortal {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub slug: String,
    pub custom_domain: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = portal_menus)]
pub struct PortalMenu {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub label: String,
    pub target_kind: String,
    pub target: String,
    pub position: i64,
    pub visible: bool,
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = portal_menus)]
pub struct NewPortalMenu {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub label: String,
    pub target: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = portal_views)]
pub struct PortalView {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub slug: String,
    pub viewer_user_id: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub viewed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = portal_views)]
pub struct NewPortalView {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub slug: String,
    pub viewer_user_id: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}
