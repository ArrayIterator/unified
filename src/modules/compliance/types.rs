//! Compliance Center module — GDPR / ISO support: data export — Types.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;

use chrono::DateTime;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle of an export request: `requested` -> `processing` ->
/// `ready` (download link attached, expiry set); `failed` when
/// assembly fails; `expired` once the link expiry passes (derived
/// at read time, see `export_view`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportStatus {
    Requested,
    Processing,
    Ready,
    Failed,
    Expired,
}

impl Default for ExportStatus {
    fn default() -> Self {
        Self::Requested
    }
}

impl ExportStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }
}

/// One data export request. `scope` names the modules / data
/// categories the export covers (free-form list, validated as
/// short strings). The file itself is assembled by the owning
/// modules; this row tracks the request and, once ready, the
/// storage link (`storage_target` + `storage_key`) and its expiry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub id: Uuid,
    pub requester_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    pub status: ExportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of a deletion request: `requested` -> `approved` (or
/// `rejected`) -> `executing` (items dispatched) -> `executed`
/// (every item confirmed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionStatus {
    Requested,
    Approved,
    Rejected,
    Executing,
    Executed,
}

impl Default for DeletionStatus {
    fn default() -> Self {
        Self::Requested
    }
}

impl DeletionStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Executing => "executing",
            Self::Executed => "executed",
        }
    }
}

/// One data deletion request. This module never deletes another
/// module's rows directly: it tracks the request, the approval,
/// and an audit record of WHAT was deleted, per module, in
/// `DeletionItem` rows. `decided_by` / `decision_reason` record
/// the approve/reject decision itself on the row, so the GDPR
/// decision trail survives beyond the emitted event payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub reason: String,
    pub status: DeletionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,
    /// Who approved or rejected the request (manager actor id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// The reason the decider gave for approving or rejecting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One per-module item of a deletion request: which module owns
/// the data and what was deleted. `status` is `pending`,
/// `dispatched`, or `executed`; `detail` records what was deleted
/// (for example "3 posts, 12 votes") once the owning module
/// confirms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionItem {
    pub id: Uuid,
    pub deletion_request_id: Uuid,
    /// Id of the module that owns the data (for example "community").
    pub module_id: String,
    /// What is to be deleted, as named by the requester (for
    /// example "all forum posts and votes").
    pub subject: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One append-only consent record: who consented to what, when,
/// and the version of the consent text. `granted` true means
/// consent given, false means withdrawn. Rows are never updated or
/// deleted; a withdrawal is a new row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentLog {
    pub id: Uuid,
    pub user_id: String,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
    pub recorded_at: DateTime<Utc>,
}

/// One retention policy: how long one data category is kept and
/// what the review action is. Policies are records only;
/// enforcement is the `compliance.retention.review` scheduler job,
/// which flags items and never auto-deletes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub id: Uuid,
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One retention-review run: a policy reviewed at a point in time,
/// with how many items were flagged for review. `scheduler_status`
/// is `not_scheduled`, `queued`, or `declined` (see the scheduler
/// note in the module header). `reviewed_at` is `None` until the
/// scheduler dispatch actually runs the review; it must never be
/// set at creation time, or every review would look already
/// reviewed before anything happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionReview {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub items_flagged: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One processing-activity record (RoPA): purpose, data
/// categories, recipients, retention, and lawful basis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingRecord {
    pub id: Uuid,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<String>,
    pub retention: String,
    pub lawful_basis: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One compliance checklist item. Evidence is a storage-module
/// reference (target + key); no file bytes live in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceChecklist {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `open`, `in_progress`, `done`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One breach-log entry with its timeline. `notification_status`
/// is `not_started`, `in_progress`, or `notified`. Timeline
/// entries are append-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreachLog {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    /// `reported`, `investigating`, `contained`, `notified`, `closed`.
    pub status: String,
    pub notification_status: String,
    pub reported_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One append-only timeline entry on a breach.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreachTimelineEntry {
    pub id: Uuid,
    pub breach_id: Uuid,
    pub author_id: String,
    pub entry: String,
    pub recorded_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_status_as_str() {
        assert_eq!(ExportStatus::Requested.as_str(), "requested");
        assert_eq!(ExportStatus::Processing.as_str(), "processing");
        assert_eq!(ExportStatus::Ready.as_str(), "ready");
        assert_eq!(ExportStatus::Failed.as_str(), "failed");
        assert_eq!(ExportStatus::Expired.as_str(), "expired");
        assert_eq!(ExportStatus::default(), ExportStatus::Requested);
    }

    #[test]
    fn deletion_status_as_str() {
        assert_eq!(DeletionStatus::Requested.as_str(), "requested");
        assert_eq!(DeletionStatus::Approved.as_str(), "approved");
        assert_eq!(DeletionStatus::Rejected.as_str(), "rejected");
        assert_eq!(DeletionStatus::Executing.as_str(), "executing");
        assert_eq!(DeletionStatus::Executed.as_str(), "executed");
        assert_eq!(DeletionStatus::default(), DeletionStatus::Requested);
    }

    fn sample_deletion() -> DeletionRequest {
        let now = Utc::now();
        DeletionRequest {
            id: Uuid::new_v4(),
            requester_id: "user-1".to_string(),
            reason: "leave".to_string(),
            status: DeletionStatus::Requested,
            approved_by: None,
            approved_at: None,
            decided_by: None,
            decision_reason: None,
            executed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn sample_review() -> RetentionReview {
        let now = Utc::now();
        RetentionReview {
            id: Uuid::new_v4(),
            policy_id: Uuid::new_v4(),
            items_flagged: 0,
            notes: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn decision_fields_round_trip_and_omit_when_unset() {
        let decided = DeletionRequest {
            status: DeletionStatus::Rejected,
            decided_by: Some("manager-9".to_string()),
            decision_reason: Some("not enough evidence".to_string()),
            ..sample_deletion()
        };
        let value = serde_json::to_value(&decided).expect("serialize deletion");
        assert_eq!(value["decided_by"].as_str(), Some("manager-9"));
        assert_eq!(
            value["decision_reason"].as_str(),
            Some("not enough evidence")
        );
        let back: DeletionRequest =
            serde_json::from_value(value).expect("deserialize deletion");
        assert_eq!(back.decided_by.as_deref(), Some("manager-9"));
        assert_eq!(
            back.decision_reason.as_deref(),
            Some("not enough evidence")
        );

        // Rows written before the decision fields existed (or still
        // undecided) must deserialize with both fields as None, and
        // serialize without them.
        let undecided = serde_json::to_value(&sample_deletion()).expect("serialize deletion");
        assert!(undecided.get("decided_by").is_none());
        assert!(undecided.get("decision_reason").is_none());
        let back: DeletionRequest =
            serde_json::from_value(undecided).expect("deserialize deletion");
        assert_eq!(back.decided_by, None);
        assert_eq!(back.decision_reason, None);
    }

    #[test]
    fn reviewed_at_is_none_until_the_review_runs() {
        // A fresh review carries no reviewed_at: it has not been
        // reviewed yet, and the field is omitted from the payload.
        let fresh = serde_json::to_value(&sample_review()).expect("serialize review");
        assert!(fresh.get("reviewed_at").is_none());

        // Once the scheduler dispatch completes the review, the
        // timestamp is present and round-trips.
        let now = Utc::now();
        let done = RetentionReview {
            reviewed_at: Some(now),
            ..sample_review()
        };
        let value = serde_json::to_value(&done).expect("serialize review");
        assert_eq!(
            value["reviewed_at"].as_str(),
            Some(now.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true).as_str())
        );
        let back: RetentionReview =
            serde_json::from_value(value).expect("deserialize review");
        assert_eq!(back.reviewed_at, Some(now));

        // Rows written before the field became optional (plain
        // timestamp) still deserialize.
        let legacy = serde_json::json!({
            "id": Uuid::new_v4().to_string(),
            "policy_id": Uuid::new_v4().to_string(),
            "items_flagged": 3,
            "scheduler_status": "queued",
            "reviewed_at": now.to_rfc3339(),
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339(),
        });
        let back: RetentionReview =
            serde_json::from_value(legacy).expect("deserialize legacy review");
        assert_eq!(back.reviewed_at, Some(now));
    }
}
