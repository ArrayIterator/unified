//! incidents - inputs.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;
use super::{constants::*, types::*, state::*, helpers::*, handlers_incidents::*, handlers_rotations::*, handlers_policies::*, handlers_postmortems::*, handlers_auto_open::*, module::*};



#[derive(Debug, Deserialize)]
// Request bodies
pub struct CreateIncidentRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub severity: Option<IncidentSeverity>,
    #[serde(default)]
    pub affected_service_ids: Vec<String>,
    #[serde(default)]
    pub responders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_policy_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_id: Option<Uuid>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateIncidentRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub severity: Option<IncidentSeverity>,
    pub affected_service_ids: Option<Vec<String>>,
    pub escalation_policy_id: Option<Option<Uuid>>,
    pub rotation_id: Option<Option<Uuid>>,
}


#[derive(Debug, Deserialize)]
pub struct ChangeStatusRequest {
    pub status: IncidentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct AddTimelineRequest {
    pub message: String,
}


#[derive(Debug, Deserialize)]
pub struct AddResponderRequest {
    pub user_id: String,
}


#[derive(Debug, Deserialize)]
pub struct PageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default = "default_page_channel")]
    pub channel: String,
}


pub(crate) fn default_page_channel() -> String {
    "notification".to_string()
}


#[derive(Debug, Deserialize)]
pub struct CreateRotationRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default = "default_handover_interval")]
    pub handover_interval_days: i64,
    #[serde(default = "default_handover_time")]
    pub handover_time: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}


pub(crate) fn default_handover_interval() -> i64 {
    7
}


pub(crate) fn default_handover_time() -> String {
    "09:00".to_string()
}


pub(crate) fn default_timezone() -> String {
    "UTC".to_string()
}


#[derive(Debug, Deserialize)]
pub struct UpdateRotationRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub handover_interval_days: Option<i64>,
    pub handover_time: Option<String>,
    pub timezone: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: String,
}


#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default = "default_schedule_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_entry_id: Option<Uuid>,
}


pub(crate) fn default_schedule_kind() -> String {
    "regular".to_string()
}


#[derive(Debug, Deserialize)]
pub struct CreateEscalationPolicyRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub applies_to_severities: Vec<String>,
    pub steps: Vec<EscalationStep>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateEscalationPolicyRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub applies_to_severities: Option<Vec<String>>,
    pub steps: Option<Vec<EscalationStep>>,
}


#[derive(Debug, Deserialize)]
pub struct CreatePostmortemRequest {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    #[serde(default)]
    pub action_items: Vec<CreateActionItemRequest>,
}


#[derive(Debug, Deserialize)]
pub struct CreateActionItemRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateActionItemRequest {
    pub title: Option<String>,
    pub assignee_id: Option<Option<String>>,
    pub status: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct SetAutoOpenRequest {
    pub enabled: bool,
    #[serde(default)]
    pub severity: Option<IncidentSeverity>,
    #[serde(default)]
    pub auto_resolve_on_recovery: bool,
}
