// Diesel models for the api domain of the unified-db-core bounded context.
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
// API Management (2026-11-22-000004_api_management).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_versions)]
pub struct ApiVersion {
    pub id: String,
    pub number: i32,
    pub enabled: bool,
    pub deprecated: bool,
    pub sunset_date: Option<DateTime<Utc>>,
    pub rate_limit_per_minute: i32,
    pub auth_required: bool,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_versions)]
pub struct NewApiVersion {
    pub id: String,
    pub number: i32,
    pub sunset_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_endpoints)]
pub struct ApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: String,
    pub description: String,
    pub rate_limit: i32,
    pub enabled: bool,
    pub rules: Value,
    pub owner_module: String,
    pub auth_required: bool,
    pub source: String,
    pub version_support: Value,
    pub execution_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_endpoints)]
pub struct NewApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_prefix_settings)]
pub struct ApiPrefixSettings {
    pub key: String,
    pub prefix: String,
    pub migrate_internal: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_prefix_settings)]
pub struct NewApiPrefixSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_keys)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub prefix: String,
    pub last4: String,
    #[serde(skip_serializing)]
    pub key_hash: String,
    pub scopes: Value,
    pub rate_tier: String,
    pub requests_per_minute: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub usage_count: i64,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_keys)]
pub struct NewApiKey {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub prefix: String,
    pub last4: String,
    pub key_hash: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_access_rules)]
pub struct ApiAccessRule {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: Option<String>,
    pub allowed: bool,
    pub public_access: bool,
    pub actor: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_access_rules)]
pub struct NewApiAccessRule {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_access_audit)]
pub struct ApiAccessAudit {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_access_audit)]
pub struct NewApiAccessAudit {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub target: String,
}
