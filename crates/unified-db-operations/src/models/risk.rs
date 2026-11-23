// Diesel models for the risk domain of the unified-db-operations bounded context.
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
#[diesel(table_name = risks)]
pub struct Risk {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub category: String,
    pub likelihood: i16,
    pub impact: i16,
    pub score: i16,
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: i16,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risks)]
pub struct NewRisk {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub category: String,
    pub likelihood: i16,
    pub impact: i16,
    pub score: i16,
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: i16,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_mitigation_plans)]
pub struct RiskMitigationPlan {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_mitigation_plans)]
pub struct NewRiskMitigationPlan {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_mitigation_tasks)]
pub struct RiskMitigationTask {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub external_project_task_id: Option<String>,
    pub awaiting_projects_handler: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_mitigation_tasks)]
pub struct NewRiskMitigationTask {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub external_project_task_id: Option<String>,
    pub awaiting_projects_handler: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_templates)]
pub struct RiskChecklistTemplate {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_templates)]
pub struct NewRiskChecklistTemplate {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_template_items)]
pub struct RiskChecklistTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_template_items)]
pub struct NewRiskChecklistTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_instances)]
pub struct RiskChecklistInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub company_id: Option<Uuid>,
    pub risk_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_instances)]
pub struct NewRiskChecklistInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub company_id: Option<Uuid>,
    pub risk_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_instance_items)]
pub struct RiskChecklistInstanceItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub template_item_id: Uuid,
    pub title: String,
    pub required: bool,
    pub sort_order: i32,
    pub completed: bool,
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_instance_items)]
pub struct NewRiskChecklistInstanceItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub template_item_id: Uuid,
    pub title: String,
    pub required: bool,
    pub sort_order: i32,
    pub completed: bool,
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_alerts)]
pub struct RiskAlert {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub company_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub score: Option<i16>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_alerts)]
pub struct NewRiskAlert {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub company_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub score: Option<i16>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
}
