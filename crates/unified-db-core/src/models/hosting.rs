// Diesel models for the hosting domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Dashboards and reseller links (2026-11-22-000006_dashboards_reseller).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = reseller_links)]
pub struct ResellerLink {
    pub reseller_id: Uuid,
    pub client_id: Uuid,
    pub verified: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = reseller_links)]
pub struct NewResellerLink {
    pub reseller_id: Uuid,
    pub client_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = view_as_sessions)]
pub struct ViewAsSession {
    pub admin_id: Uuid,
    pub target_user_id: Uuid,
    pub reason: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = view_as_sessions)]
pub struct NewViewAsSession {
    pub admin_id: Uuid,
    pub target_user_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = dashboard_settings)]
pub struct DashboardSettings {
    pub key: String,
    pub admin_prefix: String,
    pub user_prefix: String,
    pub view_as_ttl_minutes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = dashboard_settings)]
pub struct NewDashboardSettings {
    pub key: String,
}

// ---------------------------------------------------------------------------
// Hosting (2026-11-22-000007_hosting).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hosting_plans)]
pub struct HostingPlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub disk_gb: i64,
    pub cpu_cores: f64,
    pub ram_mb: i64,
    pub bandwidth_gb: i64,
    pub price: PgNumeric,
    pub currency: String,
    pub billing_cycle: String,
    pub store_product_id: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hosting_plans)]
pub struct NewHostingPlan {
    pub id: Uuid,
    pub name: String,
    pub disk_gb: i64,
    pub cpu_cores: f64,
    pub ram_mb: i64,
    pub bandwidth_gb: i64,
    pub price: PgNumeric,
    pub currency: String,
    pub billing_cycle: String,
    pub store_product_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hosting_accounts)]
pub struct HostingAccount {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub domain: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub ssl_state: String,
    pub disk_used_mb: i64,
    pub bandwidth_used_mb: i64,
    pub store_order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hosting_accounts)]
pub struct NewHostingAccount {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub domain: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub store_order_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hosting_order_claims)]
pub struct HostingOrderClaim {
    pub order_id: String,
    pub plan_id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hosting_order_claims)]
pub struct NewHostingOrderClaim {
    pub order_id: String,
    pub plan_id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
}
