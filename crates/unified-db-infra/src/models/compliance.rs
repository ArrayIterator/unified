// Diesel models for the compliance domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Compliance (migration 000058_compliance).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_export_requests)]
pub struct ComplianceExportRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub scope: Vec<String>,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub download_expires_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_export_requests)]
pub struct NewComplianceExportRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub download_expires_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_deletion_requests)]
pub struct ComplianceDeletionRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub reason: String,
    pub status: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_deletion_requests)]
pub struct NewComplianceDeletionRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub reason: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_deletion_items)]
pub struct ComplianceDeletionItem {
    pub id: Uuid,
    pub deletion_request_id: Uuid,
    pub module_id: String,
    pub subject: String,
    pub status: String,
    pub detail: Option<String>,
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_deletion_items)]
pub struct NewComplianceDeletionItem {
    pub id: Uuid,
    pub deletion_request_id: Uuid,
    pub module_id: String,
    pub subject: String,
    pub detail: Option<String>,
    pub executed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_consents)]
pub struct ComplianceConsent {
    pub id: Uuid,
    pub user_id: String,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_consents)]
pub struct NewComplianceConsent {
    pub id: Uuid,
    pub user_id: String,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_retention_policies)]
pub struct ComplianceRetentionPolicy {
    pub id: Uuid,
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_retention_policies)]
pub struct NewComplianceRetentionPolicy {
    pub id: Uuid,
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_retention_reviews)]
pub struct ComplianceRetentionReview {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub items_flagged: i64,
    pub notes: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub reviewed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_retention_reviews)]
pub struct NewComplianceRetentionReview {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub notes: Option<String>,
    pub scheduler_job_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_processing_records)]
pub struct ComplianceProcessingRecord {
    pub id: Uuid,
    pub purpose: String,
    pub data_categories: Vec<String>,
    pub recipients: Vec<String>,
    pub retention: String,
    pub lawful_basis: String,
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_processing_records)]
pub struct NewComplianceProcessingRecord {
    pub id: Uuid,
    pub purpose: String,
    pub retention: String,
    pub lawful_basis: String,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_checklists)]
pub struct ComplianceChecklist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub evidence_storage_target: Option<String>,
    pub evidence_storage_key: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_checklists)]
pub struct NewComplianceChecklist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub evidence_storage_target: Option<String>,
    pub evidence_storage_key: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_breach_log)]
pub struct ComplianceBreachLog {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    pub status: String,
    pub notification_status: String,
    pub reported_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_breach_log)]
pub struct NewComplianceBreachLog {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    pub reported_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_breach_timeline)]
pub struct ComplianceBreachTimeline {
    pub id: Uuid,
    pub breach_id: Uuid,
    pub author_id: String,
    pub entry: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_breach_timeline)]
pub struct NewComplianceBreachTimeline {
    pub id: Uuid,
    pub breach_id: Uuid,
    pub author_id: String,
    pub entry: String,
}
