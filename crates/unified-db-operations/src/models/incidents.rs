// Diesel models for the incidents domain of the unified-db-operations bounded context.
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
// Incidents & on-call (migration 000053_incidents).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incidents)]
pub struct Incident {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub severity: String,
    pub status: String,
    pub affected_service_ids: Vec<String>,
    pub responders: Vec<String>,
    pub current_responder: Option<String>,
    pub source: String,
    pub source_service_id: Option<String>,
    pub escalation_policy_id: Option<Uuid>,
    pub rotation_id: Option<Uuid>,
    pub created_by: String,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incidents)]
pub struct NewIncident {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub current_responder: Option<String>,
    pub source_service_id: Option<String>,
    pub escalation_policy_id: Option<Uuid>,
    pub rotation_id: Option<Uuid>,
    pub created_by: String,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incident_timeline)]
pub struct IncidentTimeline {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub actor_id: String,
    pub kind: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incident_timeline)]
pub struct NewIncidentTimeline {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub actor_id: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = on_call_rotations)]
pub struct OnCallRotation {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub members: Vec<String>,
    pub handover_interval_days: i64,
    pub handover_time: String,
    pub timezone: String,
    pub current_index: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = on_call_rotations)]
pub struct NewOnCallRotation {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = on_call_schedules)]
pub struct OnCallSchedule {
    pub id: Uuid,
    pub rotation_id: Uuid,
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub kind: String,
    pub overrides_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = on_call_schedules)]
pub struct NewOnCallSchedule {
    pub id: Uuid,
    pub rotation_id: Uuid,
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub overrides_entry_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = escalation_policies)]
pub struct EscalationPolicy {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub applies_to_severities: Vec<String>,
    pub steps: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = escalation_policies)]
pub struct NewEscalationPolicy {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incident_escalations)]
pub struct IncidentEscalation {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub policy_id: Uuid,
    pub step_index: i64,
    pub next_escalation_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incident_escalations)]
pub struct NewIncidentEscalation {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub policy_id: Uuid,
    pub next_escalation_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = page_attempts)]
pub struct PageAttempt {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub paged_user_id: String,
    pub channel: String,
    pub status: String,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = page_attempts)]
pub struct NewPageAttempt {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub paged_user_id: String,
    pub acknowledged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incident_postmortems)]
pub struct IncidentPostmortem {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub summary: String,
    pub timeline_summary: Option<String>,
    pub root_cause: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incident_postmortems)]
pub struct NewIncidentPostmortem {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub summary: String,
    pub timeline_summary: Option<String>,
    pub root_cause: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = postmortem_action_items)]
pub struct PostmortemActionItem {
    pub id: Uuid,
    pub postmortem_id: Uuid,
    pub title: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub task_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = postmortem_action_items)]
pub struct NewPostmortemActionItem {
    pub id: Uuid,
    pub postmortem_id: Uuid,
    pub title: String,
    pub assignee_id: Option<String>,
    pub task_reference: Option<String>,
}
