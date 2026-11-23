//! incidents - types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;



#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------
/// Incident severity: `critical`, `high`, `medium`, `low`.
pub enum IncidentSeverity {
    Critical,
    High,
    Medium,
    Low,
}


impl Default for IncidentSeverity {
    fn default() -> Self {
        Self::Medium
    }
}


impl IncidentSeverity {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}


/// Incident status. The machine is:
///
///   open -> acknowledged -> investigating -> mitigating ->
///   resolved -> closed
///
/// with two step-backs: `investigating` -> `acknowledged` and
/// `resolved` -> `investigating` (reopen). Every other transition
/// is rejected. `closed` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Open,
    Acknowledged,
    Investigating,
    Mitigating,
    Resolved,
    Closed,
}


impl Default for IncidentStatus {
    fn default() -> Self {
        Self::Open
    }
}


impl IncidentStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acknowledged => "acknowledged",
            Self::Investigating => "investigating",
            Self::Mitigating => "mitigating",
            Self::Resolved => "resolved",
            Self::Closed => "closed",
        }
    }

    /// Allowed next statuses from this status.
    pub(crate) fn allowed_next(&self) -> &'static [IncidentStatus] {
        match self {
            Self::Open => &[Self::Acknowledged],
            Self::Acknowledged => &[Self::Investigating],
            Self::Investigating => &[Self::Mitigating, Self::Acknowledged],
            Self::Mitigating => &[Self::Resolved, Self::Investigating],
            Self::Resolved => &[Self::Closed, Self::Investigating],
            Self::Closed => &[],
        }
    }
}


/// One incident. `affected_service_ids` links to Service Monitor
/// service ids (plain strings - the Service Monitor owns services).
/// `source` is `manual` or `auto` (opened from a Service Monitor
/// alert); `source_service_id` names the service for auto
/// incidents. `responders` are user ids; `current_responder` is
/// the user id currently paged / owning the incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub severity: IncidentSeverity,
    pub status: IncidentStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_service_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub responders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_responder: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_policy_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_id: Option<Uuid>,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One append-only timeline entry. `kind` is `note`,
/// `status_change`, `responder_action`, `page`, `escalation`, or
/// `system`. Entries are never updated or deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentTimelineEntry {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub actor_id: String,
    pub kind: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}


/// One on-call rotation: an ordered list of responder user ids.
/// Handover happens every `handover_interval_days` at
/// `handover_time` (HH:MM, UTC) in `timezone` (format-checked
/// only). `current_index` is the member currently on call when no
/// schedule entry covers now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallRotation {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    pub handover_interval_days: i64,
    /// Handover time of day, "HH:MM" (UTC).
    pub handover_time: String,
    pub timezone: String,
    pub current_index: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One on-call schedule entry: `member_id` is on call from
/// `starts_at` to `ends_at` for `rotation_id`. `kind` is `regular`
/// or `override`; an `override` entry wins over a `regular` entry
/// covering the same time. `overrides_entry_id` names the regular
/// entry this override replaces, when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallSchedule {
    pub id: Uuid,
    pub rotation_id: Uuid,
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One step of an escalation policy: after `after_minutes`
/// unacknowledged, page `target` - `responder` (the next rotation
/// member), `manager` (the `manager_id`), or `user` (the
/// `user_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationStep {
    pub after_minutes: i64,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_id: Option<String>,
}


/// One escalation policy: an ordered list of steps applied to
/// incidents of the listed severities (`applies_to_severities`
/// empty means every severity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationPolicy {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to_severities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<EscalationStep>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// Per-incident escalation state: which policy step the incident
/// is on and when the next escalation is due. `status` is
/// `active`, `completed` (all steps exhausted), or `stopped` (the
/// incident was acknowledged or resolved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentEscalation {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub policy_id: Uuid,
    pub step_index: usize,
    pub next_escalation_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One page attempt: paging a responder for an incident. `status`
/// is `sent`, `acknowledged`, or `expired`. Acknowledgement is
/// recorded on the attempt and on the incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageAttempt {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub paged_user_id: String,
    pub channel: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One postmortem action item. `status` is `open`, `in_progress`,
/// or `done`. `task_reference` is the id the tasks module returns
/// once it consumes the `tasks.create.requested` event (empty
/// until then - see the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostmortemActionItem {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One postmortem, linked to a resolved or closed incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentPostmortem {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_items: Vec<PostmortemActionItem>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// Per-service auto-open rule for Service Monitor alerts.
/// `enabled` defaults to `false` (off) - auto-open is opt-in per
/// service. `auto_resolve_on_recovery` resolves open auto
/// incidents for the service when it recovers (also off by
/// default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoOpenRule {
    pub service_id: String,
    pub enabled: bool,
    pub severity: IncidentSeverity,
    #[serde(default)]
    pub auto_resolve_on_recovery: bool,
    pub updated_at: DateTime<Utc>,
}
