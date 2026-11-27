// Diesel models for the health domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_health_checks)]
pub struct HealthCheck {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    pub detail: String,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_health_checks)]
pub struct NewHealthCheck {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub ok: bool,
}

// Advisor + Risk (2026-11-22-000039_advisor_risk). The notes' structs
// omit `Selectable` and cite a `check_for_backend` convention that
// does not exist in this file (0 occurrences); normalised to the
// file's `Queryable, Selectable, Serialize` / `Insertable,
// Deserialize` convention.

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = advisor_suggestions)]
pub struct AdvisorSuggestion {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    pub source_ids: Value,
    pub figures: Value,
    pub dedupe_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = advisor_suggestions)]
pub struct NewAdvisorSuggestion {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    pub source_ids: Value,
    pub figures: Value,
    pub dedupe_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = advisor_rule_runs)]
pub struct AdvisorRuleRun {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: i32,
    pub suggestions_updated: i32,
    pub inputs_summary: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = advisor_rule_runs)]
pub struct NewAdvisorRuleRun {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: i32,
    pub suggestions_updated: i32,
    pub inputs_summary: Value,
}
