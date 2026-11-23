// Diesel models for the ESG domain of the unified-db-platform bounded context.
//
// Tables are defined by migration 2026-11-22-000119_esg. They had
// no Diesel bindings before C11 (task 271): the ESG module kept
// its live data in memory and the tables were persistence targets
// only. The structs below mirror that migration exactly; the
// runtime types live in `src/modules/esg/types.rs` and the
// hydrate path in `src/modules/esg/state.rs` converts between
// the two (the DB stores the fixed vocabularies — category,
// data type, direction, period status — as plain TEXT).

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ESG (migration 000119_esg).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_frameworks)]
pub struct EsgFramework {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_frameworks)]
pub struct NewEsgFramework {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_metrics)]
pub struct EsgMetric {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub unit: String,
    pub data_type: String,
    pub frequency: String,
    pub framework_ids: Value,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_metrics)]
pub struct NewEsgMetric {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub unit: String,
    pub data_type: String,
    pub frequency: String,
    pub framework_ids: Value,
    pub active: bool,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_periods)]
pub struct EsgPeriod {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub name: Option<String>,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_periods)]
pub struct NewEsgPeriod {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub name: Option<String>,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub status: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_entries)]
pub struct EsgEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub scope_type: String,
    pub scope_id: String,
    pub value_text: String,
    pub source_note: Option<String>,
    pub document_ids: Value,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_entries)]
pub struct NewEsgEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub scope_type: String,
    pub scope_id: String,
    pub value_text: String,
    pub source_note: Option<String>,
    pub document_ids: Value,
    pub entered_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_entry_history)]
pub struct EsgEntryHistory {
    pub id: Uuid,
    pub tenant_id: String,
    pub entry_id: Uuid,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub value_text: String,
    pub source_note: Option<String>,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_entry_history)]
pub struct NewEsgEntryHistory {
    pub id: Uuid,
    pub tenant_id: String,
    pub entry_id: Uuid,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub value_text: String,
    pub source_note: Option<String>,
    pub entered_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_targets)]
pub struct EsgTarget {
    pub id: Uuid,
    pub tenant_id: String,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub scope_type: String,
    pub scope_id: String,
    pub target_value: String,
    pub direction: String,
    pub deadline: NaiveDate,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_targets)]
pub struct NewEsgTarget {
    pub id: Uuid,
    pub tenant_id: String,
    pub metric_id: Uuid,
    pub period_id: Uuid,
    pub scope_type: String,
    pub scope_id: String,
    pub target_value: String,
    pub direction: String,
    pub deadline: NaiveDate,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = esg_audit)]
pub struct EsgAudit {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub actor: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = esg_audit)]
pub struct NewEsgAudit {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub actor: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}
