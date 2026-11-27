// Diesel models for the okr domain of the unified-db-people bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// OKRs (migration 000043_okrs).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = okr_cycles)]
pub struct OkrCycle {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = okr_cycles)]
pub struct NewOkrCycle {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = okr_objectives)]
pub struct OkrObjective {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub company_id: String,
    pub parent_objective_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub owner_id: String,
    pub final_score: Option<PgNumeric>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = okr_objectives)]
pub struct NewOkrObjective {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub company_id: String,
    pub parent_objective_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub owner_id: String,
    pub final_score: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = okr_key_results)]
pub struct OkrKeyResult {
    pub id: Uuid,
    pub objective_id: Uuid,
    pub title: String,
    pub unit: String,
    pub target_value: PgNumeric,
    pub start_value: PgNumeric,
    pub current_value: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = okr_key_results)]
pub struct NewOkrKeyResult {
    pub id: Uuid,
    pub objective_id: Uuid,
    pub title: String,
    pub unit: String,
    pub target_value: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = okr_checkins)]
pub struct OkrCheckin {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub author_id: String,
    pub note: String,
    pub confidence: String,
    pub kind: String,
    pub reason: Option<String>,
    pub value_snapshot: Option<PgNumeric>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = okr_checkins)]
pub struct NewOkrCheckin {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub author_id: String,
    pub note: String,
    pub confidence: String,
    pub reason: Option<String>,
    pub value_snapshot: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = okr_links)]
pub struct OkrLink {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub link_type: String,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = okr_links)]
pub struct NewOkrLink {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub link_type: String,
    pub target_id: Uuid,
}
