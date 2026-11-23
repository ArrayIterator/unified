// Diesel models for the privacy domain of the unified-db-infra bounded context.
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
// Privacy (migration 000066_privacy).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_field_declarations)]
pub struct PrivacyFieldDeclaration {
    pub id: Uuid,
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    pub required: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_field_declarations)]
pub struct NewPrivacyFieldDeclaration {
    pub id: Uuid,
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_anonymizations)]
pub struct PrivacyAnonymization {
    pub id: Uuid,
    pub user_id: String,
    pub token: String,
    pub field_tokens: Value,
    pub scope_modules: Vec<String>,
    pub reason: Option<String>,
    pub actor: String,
    pub times_applied: i64,
    pub anonymized_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_anonymizations)]
pub struct NewPrivacyAnonymization {
    pub id: Uuid,
    pub user_id: String,
    pub token: String,
    pub reason: Option<String>,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_visibility_defaults)]
pub struct PrivacyVisibilityDefault {
    pub id: Uuid,
    pub module_id: String,
    pub visibility: String,
    pub explicit: bool,
    pub configured_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_visibility_defaults)]
pub struct NewPrivacyVisibilityDefault {
    pub id: Uuid,
    pub module_id: String,
    pub configured_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_consents)]
pub struct PrivacyConsent {
    pub id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub granted: bool,
    pub granted_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_consents)]
pub struct NewPrivacyConsent {
    pub id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub granted_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_consent_history)]
pub struct PrivacyConsentHistory {
    pub id: Uuid,
    pub consent_id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub action: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_consent_history)]
pub struct NewPrivacyConsentHistory {
    pub id: Uuid,
    pub consent_id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub action: String,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_erasure_requests)]
pub struct PrivacyErasureRequest {
    pub id: Uuid,
    pub user_id: String,
    pub modules: Vec<String>,
    pub reason: String,
    pub status: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub anonymization_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_erasure_requests)]
pub struct NewPrivacyErasureRequest {
    pub id: Uuid,
    pub user_id: String,
    pub reason: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub anonymization_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_notices)]
pub struct PrivacyNotice {
    pub id: Uuid,
    pub module_id: String,
    pub text: String,
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_notices)]
pub struct NewPrivacyNotice {
    pub id: Uuid,
    pub module_id: String,
    pub text: String,
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_audit_entries)]
pub struct PrivacyAuditEntry {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_audit_entries)]
pub struct NewPrivacyAuditEntry {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource: String,
}
