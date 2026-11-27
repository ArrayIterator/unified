// Diesel models for the security_mcp_ai domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auth_recovery_codes)]
pub struct AuthRecoveryCode {
    pub user_id: String,
    pub code_hash: String,
    pub used: bool,
    pub created_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auth_recovery_codes)]
pub struct NewAuthRecoveryCode {
    pub user_id: String,
    pub code_hash: String,
    pub used_at: Option<DateTime<Utc>>,
}

// Security (2026-11-22-000020_security).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = security_ip_lists)]
pub struct SecurityIpList {
    pub list: String,
    pub cidr: String,
    pub network: String,
    pub prefix: i32,
    pub added_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = security_ip_lists)]
pub struct NewSecurityIpList {
    pub list: String,
    pub cidr: String,
    pub network: String,
    pub prefix: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = security_reputation)]
pub struct SecurityReputation {
    pub ip: String,
    pub score: i32,
    pub flagged: bool,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = security_reputation)]
pub struct NewSecurityReputation {
    pub ip: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = security_failed_auth_clusters)]
pub struct SecurityFailedAuthCluster {
    pub key: String,
    pub ip: Option<String>,
    pub account: Option<String>,
    pub count: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = security_failed_auth_clusters)]
pub struct NewSecurityFailedAuthCluster {
    pub key: String,
    pub ip: Option<String>,
    pub account: Option<String>,
}

// MCP servers (2026-11-22-000029_mcp).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mcp_servers)]
pub struct McpServer {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_id: Uuid,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub product_id: Option<Uuid>,
    pub status: String,
    pub is_public: bool,
    pub allowed_tools: Value,
    pub rate_limit_per_minute: i32,
    pub read_only_role: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mcp_servers)]
pub struct NewMcpServer {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_id: Uuid,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub product_id: Option<Uuid>,
    pub read_only_role: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mcp_settings)]
pub struct McpSettings {
    pub key: String,
    pub read_only_role: Option<String>,
    pub default_rate_limit_per_minute: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mcp_settings)]
pub struct NewMcpSettings {
    pub key: String,
    pub read_only_role: Option<String>,
}

// AI (2026-11-22-000030_ai).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_provider_connections)]
pub struct AiProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub enabled: bool,
    pub rate_limit_per_minute: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_provider_connections)]
pub struct NewAiProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_usage_records)]
pub struct AiUsageRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub provider_kind: String,
    pub actor: String,
    pub use_case: String,
    pub model: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub estimated_cost_usd: Option<f64>,
    pub latency_ms: i64,
    pub status: String,
    pub error_code: Option<i32>,
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub denial_reason: Option<String>,
    pub used_personal_key: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_usage_records)]
pub struct NewAiUsageRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub provider_kind: String,
    pub actor: String,
    pub use_case: String,
    pub model: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub estimated_cost_usd: Option<f64>,
    pub status: String,
    pub error_code: Option<i32>,
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub denial_reason: Option<String>,
    pub used_personal_key: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_drafts)]
pub struct AiDraft {
    pub id: Uuid,
    pub kind: String,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub prompt: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_kind: String,
    pub model: String,
    pub actor: String,
    pub status: String,
    pub ai_generated: bool,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub sources: Value,
    pub usage_id: Option<Uuid>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_drafts)]
pub struct NewAiDraft {
    pub id: Uuid,
    pub kind: String,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub prompt: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_kind: String,
    pub model: String,
    pub actor: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub usage_id: Option<Uuid>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}
