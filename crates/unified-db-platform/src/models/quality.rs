// Diesel models for the quality domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Import / export hardening (migration 000083_import_export_hardening).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mapping_templates)]
pub struct MappingTemplate {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub mapping: Value,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mapping_templates)]
pub struct NewMappingTemplate {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub company_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Quality management (migration 000087_quality).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_points)]
pub struct QualityPoint {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub scope: String,
    pub target_ref: Option<String>,
    pub target_kind: Option<String>,
    pub check_kind: String,
    pub on_fail_policy: String,
    pub alert_severity: String,
    pub active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_points)]
pub struct NewQualityPoint {
    pub id: Uuid,
    pub name: String,
    pub scope: String,
    pub target_ref: Option<String>,
    pub target_kind: Option<String>,
    pub check_kind: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_criteria)]
pub struct QualityCriterion {
    pub id: Uuid,
    pub point_id: Uuid,
    pub name: String,
    pub kind: String,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub expected_bool: Option<bool>,
    pub expected_text: Option<String>,
    pub failure_cause: Option<String>,
    pub weight: f64,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_criteria)]
pub struct NewQualityCriterion {
    pub id: Uuid,
    pub point_id: Uuid,
    pub name: String,
    pub kind: String,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub expected_bool: Option<bool>,
    pub expected_text: Option<String>,
    pub failure_cause: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_inspections)]
pub struct QualityInspection {
    pub id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub inspector_id: Option<String>,
    pub status: String,
    pub result: Option<String>,
    pub measurements: Value,
    pub failure_causes: Vec<String>,
    pub notes: Option<String>,
    pub evidence_refs: Vec<String>,
    pub inspected_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_inspections)]
pub struct NewQualityInspection {
    pub id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub inspector_id: Option<String>,
    pub status: String,
    pub result: Option<String>,
    pub notes: Option<String>,
    pub inspected_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_alerts)]
pub struct QualityAlert {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub blocked: bool,
    pub acknowledged_by: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_alerts)]
pub struct NewQualityAlert {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub acknowledged_by: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_capas)]
pub struct QualityCapa {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub alert_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub root_cause: Option<String>,
    pub owner_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub state: String,
    pub verification_notes: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_capas)]
pub struct NewQualityCapa {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub alert_id: Option<Uuid>,
    pub title: String,
    pub root_cause: Option<String>,
    pub owner_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub state: String,
    pub verification_notes: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
}
