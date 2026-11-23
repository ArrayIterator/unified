// Diesel models for the storage_plans domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Storage product (storage product notes §4). The notes mirror the
// module structs field-for-field, so `MeteringReport`,
// `ByoTermsAcceptance` and `ByoHealthCheck` carry no
// created_at/updated_at even though the tables have them.
// `ByoConnection.credentials_encrypted` is marked
// `#[serde(skip_serializing)]` per the notes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_plans)]
pub struct StoragePlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub quota_bytes: i64,
    pub max_files: i64,
    pub included_provider_kinds: Vec<String>,
    pub product_id: Option<String>,
    pub billing_cycle: Option<String>,
    pub is_public: bool,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_plans)]
pub struct NewStoragePlan {
    pub id: Uuid,
    pub name: String,
    pub product_id: Option<String>,
    pub billing_cycle: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_plan_assignments)]
pub struct PlanAssignment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_plan_assignments)]
pub struct NewPlanAssignment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_usage_metering)]
pub struct MeteringReport {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub plan_id: Option<Uuid>,
    pub period: String,
    pub bytes_stored: i64,
    pub files_stored: i64,
    pub reported_by: String,
    pub reported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_usage_metering)]
pub struct NewMeteringReport {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub plan_id: Option<Uuid>,
    pub period: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_connections)]
pub struct ByoConnection {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub s3_compatible: bool,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: i32,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub path_prefix: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub status: String,
    pub status_reason: Option<String>,
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub used_bytes: i64,
    pub used_files: i64,
    pub encryption_required: bool,
    pub allowed_extensions: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_connections)]
pub struct NewByoConnection {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub status_reason: Option<String>,
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_terms)]
pub struct ByoTerms {
    pub id: Uuid,
    pub version: String,
    pub title: String,
    pub body: String,
    pub is_current: bool,
    pub reacceptance_deadline: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_terms)]
pub struct NewByoTerms {
    pub id: Uuid,
    pub version: String,
    pub title: String,
    pub reacceptance_deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_terms_acceptances)]
pub struct ByoTermsAcceptance {
    pub id: Uuid,
    pub terms_id: Uuid,
    pub terms_version: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub connection_id: Option<Uuid>,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_terms_acceptances)]
pub struct NewByoTermsAcceptance {
    pub id: Uuid,
    pub terms_id: Uuid,
    pub terms_version: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub connection_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_objects)]
pub struct ByoObject {
    pub connection_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    pub mime: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_objects)]
pub struct NewByoObject {
    pub connection_id: Uuid,
    pub key: String,
    pub owner_user: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_health_checks)]
pub struct ByoHealthCheck {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    pub detail: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_health_checks)]
pub struct NewByoHealthCheck {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ok: bool,
}
