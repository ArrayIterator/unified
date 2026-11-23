// Diesel models for the holidays domain of the unified-db-people bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Holidays (migration 000104_holidays).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_providers)]
pub struct HolidayProvider {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub allowed_hosts: Value,
    pub api_key_required: bool,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub min_interval_secs: i64,
    pub max_requests_per_minute: i64,
    pub calendar_ids: Value,
    pub enabled: bool,
    pub builtin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_providers)]
pub struct NewHolidayProvider {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_countries)]
pub struct HolidayCountry {
    pub code: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_countries)]
pub struct NewHolidayCountry {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_sync_configs)]
pub struct HolidaySyncConfig {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub provider_id: String,
    pub fallback_provider_ids: Value,
    pub countries: Value,
    pub years: Value,
    pub schedule: String,
    pub feed_url: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_sync_configs)]
pub struct NewHolidaySyncConfig {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub provider_id: String,
    pub schedule: String,
    pub feed_url: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_sync_runs)]
pub struct HolidaySyncRun {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub config_id: Uuid,
    pub provider_id: String,
    pub country: String,
    pub year: i64,
    pub status: String,
    pub trigger: String,
    pub holidays_found: i64,
    pub holidays_stored: i64,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_sync_runs)]
pub struct NewHolidaySyncRun {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub config_id: Uuid,
    pub provider_id: String,
    pub country: String,
    pub year: i64,
    pub status: String,
    pub trigger: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holidays)]
pub struct Holiday {
    pub id: Uuid,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub local_name: Option<String>,
    pub provider_id: String,
    pub source: String,
    pub global: bool,
    pub types: Value,
    pub counties: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holidays)]
pub struct NewHoliday {
    pub id: Uuid,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub local_name: Option<String>,
    pub provider_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_overrides)]
pub struct HolidayOverride {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub override_kind: String,
    pub note: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_overrides)]
pub struct NewHolidayOverride {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub override_kind: String,
    pub note: Option<String>,
    pub created_by: String,
}
