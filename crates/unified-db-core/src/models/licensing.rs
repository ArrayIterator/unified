// Diesel models for the licensing domain of the unified-db-core bounded context.
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
#[diesel(table_name = license_products)]
pub struct LicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_products)]
pub struct NewLicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_distributions)]
pub struct LicenseDistribution {
    pub id: Uuid,
    pub product: String,
    pub provider: String,
    pub repo: String,
    pub git_ref: String,
    pub webhook_secret_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_distributions)]
pub struct NewLicenseDistribution {
    pub id: Uuid,
    pub product: String,
    pub provider: String,
    pub repo: String,
    pub git_ref: String,
    pub webhook_secret_hash: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_releases)]
pub struct LicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_releases)]
pub struct NewLicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_events)]
pub struct LicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_events)]
pub struct NewLicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_offline_tokens)]
pub struct LicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_offline_tokens)]
pub struct NewLicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_activation_history)]
pub struct LicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_activation_history)]
pub struct NewLicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
}
