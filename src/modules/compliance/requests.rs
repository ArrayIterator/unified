//! Compliance Center module - GDPR / ISO support: data export - Request bodies.
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
use super::retention::*;
use super::state::*;
use super::types::*;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CreateExportRequest {
    #[serde(default)]
    pub scope: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteExportRequest {
    pub storage_target: String,
    pub storage_key: String,
    /// Link lifetime in hours; defaults to 72.
    pub expires_in_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct FailExportRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDeletionRequest {
    pub reason: String,
    /// Per-module items: which module owns what data.
    pub items: Vec<DeletionItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct DeletionItemInput {
    pub module_id: String,
    pub subject: String,
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarkItemExecutedRequest {
    /// What was deleted, for the audit record (for example
    /// "3 posts, 12 votes").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordConsentRequest {
    /// The user the consent is for. Defaults to the caller;
    /// recording for another user requires the manage capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateRetentionPolicyRequest {
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRetentionPolicyRequest {
    pub data_category: Option<String>,
    pub retain_days: Option<i64>,
    pub review_action: Option<String>,
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProcessingRecordRequest {
    pub purpose: String,
    #[serde(default)]
    pub data_categories: Vec<String>,
    #[serde(default)]
    pub recipients: Vec<String>,
    pub retention: String,
    pub lawful_basis: String,
    pub owner: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProcessingRecordRequest {
    pub purpose: Option<String>,
    pub data_categories: Option<Vec<String>>,
    pub recipients: Option<Vec<String>>,
    pub retention: Option<String>,
    pub lawful_basis: Option<String>,
    pub owner: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateChecklistRequest {
    pub title: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub evidence_storage_target: Option<String>,
    pub evidence_storage_key: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateChecklistRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub status: Option<String>,
    pub evidence_storage_target: Option<Option<String>>,
    pub evidence_storage_key: Option<Option<String>>,
    pub due_date: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBreachRequest {
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    /// Optional user to notify through `notification.requested`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBreachRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub affected_scope: Option<String>,
    pub status: Option<String>,
    pub notification_status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddTimelineRequest {
    pub entry: String,
}
