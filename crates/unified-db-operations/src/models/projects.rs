// Diesel models for the projects domain of the unified-db-operations bounded context.
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
// Project Management (2026-11-22-000031_projects).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = projects)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub status: String,
    pub visibility: String,
    pub owner_id: String,
    pub company_id: Option<Uuid>,
    pub client_id: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub labels: Value,
    pub custom_field_definitions: Value,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = projects)]
pub struct NewProject {
    pub id: Uuid,
    pub name: String,
    pub methodology: String,
    pub owner_id: String,
    pub company_id: Option<Uuid>,
    pub client_id: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub color: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_members)]
pub struct ProjectMember {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_members)]
pub struct NewProjectMember {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub role: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_boards)]
pub struct ProjectBoard {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub wip_limit: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_boards)]
pub struct NewProjectBoard {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub wip_limit: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_board_columns)]
pub struct ProjectBoardColumn {
    pub id: Uuid,
    pub board_id: Uuid,
    pub name: String,
    pub position: i32,
    pub status_key: String,
    pub wip_limit: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_board_columns)]
pub struct NewProjectBoardColumn {
    pub id: Uuid,
    pub board_id: Uuid,
    pub name: String,
    pub status_key: String,
    pub wip_limit: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_milestones)]
pub struct ProjectMilestone {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    pub due_date: Option<DateTime<Utc>>,
    pub reached: bool,
    pub reached_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_milestones)]
pub struct NewProjectMilestone {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub due_date: Option<DateTime<Utc>>,
    pub reached_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_sprints)]
pub struct ProjectSprint {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub goal: String,
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
    pub velocity_actual: Option<f64>,
    pub standup_notes: Value,
    pub review_notes: Option<String>,
    pub retrospective_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_sprints)]
pub struct NewProjectSprint {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
    pub velocity_actual: Option<f64>,
    pub review_notes: Option<String>,
    pub retrospective_notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_phases)]
pub struct ProjectPhase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub position: i32,
    pub status: String,
    pub gate_status: String,
    pub gate_decision: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_phases)]
pub struct NewProjectPhase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub gate_decision: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_cycles)]
pub struct ProjectCycle {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub appetite_weeks: i32,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub cooldown_weeks: i32,
    pub pitch: Option<String>,
    pub betting_decision: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_cycles)]
pub struct NewProjectCycle {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub pitch: Option<String>,
    pub betting_decision: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_program_increments)]
pub struct ProjectProgramIncrement {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub objectives: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_program_increments)]
pub struct NewProjectProgramIncrement {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_business_cases)]
pub struct ProjectBusinessCase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub justification: String,
    pub benefits: Value,
    pub costs: Value,
    pub risks: Value,
    pub status: String,
    pub exception_report: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_business_cases)]
pub struct NewProjectBusinessCase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub exception_report: Option<String>,
}
