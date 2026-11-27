// Diesel models for the approvals domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Approvals (migration 000089_approvals).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_definitions)]
pub struct ApprovalDefinition {
    pub id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub timeout_hours: Option<i32>,
    pub escalation_action: String,
    pub escalate_to: Option<Value>,
    pub escalation_extension_hours: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_definitions)]
pub struct NewApprovalDefinition {
    pub id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub name: String,
    pub description: Option<String>,
    pub timeout_hours: Option<i32>,
    pub escalate_to: Option<Value>,
    pub escalation_extension_hours: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_steps)]
pub struct ApprovalStep {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub position: i32,
    pub name: String,
    pub mode: String,
    pub approvers: Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_steps)]
pub struct NewApprovalStep {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub position: i32,
    pub name: String,
    pub approvers: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_requests)]
pub struct ApprovalRequest {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub record_id: String,
    pub requester_id: String,
    pub summary: String,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub status: String,
    pub current_step_index: i32,
    pub round: i32,
    pub escalation_count: i32,
    pub last_escalated_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_requests)]
pub struct NewApprovalRequest {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub record_id: String,
    pub requester_id: String,
    pub summary: String,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub last_escalated_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_decisions)]
pub struct ApprovalDecision {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub round: i32,
    pub approver_id: String,
    pub acting_for: Option<String>,
    pub slot_kind: String,
    pub slot_ref: String,
    pub decision: String,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_decisions)]
pub struct NewApprovalDecision {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub round: i32,
    pub approver_id: String,
    pub acting_for: Option<String>,
    pub slot_kind: String,
    pub decision: String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_delegations)]
pub struct ApprovalDelegation {
    pub id: Uuid,
    pub delegator_id: String,
    pub delegate_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_delegations)]
pub struct NewApprovalDelegation {
    pub id: Uuid,
    pub delegator_id: String,
    pub delegate_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_escalations)]
pub struct ApprovalEscalation {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub reason: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_escalations)]
pub struct NewApprovalEscalation {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub reason: String,
    pub action: String,
}
