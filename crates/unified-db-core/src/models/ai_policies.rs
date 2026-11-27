// Diesel models for the ai_policies domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// AI access allowlists (migration 000036_ai_allowlists; batch 2 notes sec. 8).
// Money structs carry no serde derives (PgNumeric has none); the notes'
// `New*` snippets show `Deserialize` on money structs, which cannot
// compile for PgNumeric fields, so those derives are dropped here.

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_company_policies)]
pub struct AiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub ai_enabled: bool,
    pub disabled_features: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = ai_company_policies)]
pub struct NewAiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_user_policies)]
pub struct AiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub ai_enabled: bool,
    pub disabled_features: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = ai_user_policies)]
pub struct NewAiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_personal_keys)]
pub struct AiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_encrypted: String,
    pub key_fingerprint: String,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = ai_personal_keys)]
pub struct NewAiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_encrypted: String,
    pub key_fingerprint: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_user_mcp_servers)]
pub struct AiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    #[serde(skip_serializing)]
    pub auth_token_encrypted: Option<String>,
    pub token_fingerprint: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_user_mcp_servers)]
pub struct NewAiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    pub auth_token_encrypted: Option<String>,
    pub token_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_accounts)]
pub struct AuditorAccountRow {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub is_auditor: bool,
    pub auditor_of: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub revoke_reason: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_accounts)]
pub struct NewAuditorAccountRow {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub auditor_of: String,
    pub expires_at: DateTime<Utc>,
    pub password_hash: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_scopes)]
pub struct AuditorScopeRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub module: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub allow_export: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_scopes)]
pub struct NewAuditorScopeRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub module: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub allow_export: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_sessions)]
pub struct AuditorSessionRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_sessions)]
pub struct NewAuditorSessionRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
}

// AI gating (migration 000064_ai_gating; batch 5 notes sec. c/sec. d).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_global_settings)]
pub struct AiGlobalSettingsRow {
    pub key: String,
    pub enabled: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_global_settings)]
pub struct NewAiGlobalSettingsRow {
    pub key: String,
    pub enabled: bool,
}
