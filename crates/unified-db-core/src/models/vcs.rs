// Diesel models for the vcs domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// VCS repository connections (GitHub / GitLab / Bitbucket).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_connections)]
pub struct VcsConnection {
    pub id: Uuid,
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub host: Option<String>,
    pub software_id: Option<Uuid>,
    pub enabled: bool,
    pub sync_status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_connections)]
pub struct NewVcsConnection {
    pub id: Uuid,
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub host: Option<String>,
    pub software_id: Option<Uuid>,
    pub enabled: bool,
    pub sync_status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

// VCS version drafts imported from a provider (never auto-published).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_version_drafts)]
pub struct VcsVersionDraft {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub software_id: Option<Uuid>,
    pub provider: String,
    pub version: String,
    pub tag: String,
    pub title: Option<String>,
    pub changelog: String,
    pub published_at: Option<DateTime<Utc>>,
    pub status: String,
    pub source: String,
    pub assets: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_version_drafts)]
pub struct NewVcsVersionDraft {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub software_id: Option<Uuid>,
    pub provider: String,
    pub version: String,
    pub tag: String,
    pub title: Option<String>,
    pub changelog: String,
    pub published_at: Option<DateTime<Utc>>,
    pub status: String,
    pub source: String,
    pub assets: Value,
}

// VCS sync history (manual and webhook-triggered sync runs).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_sync_history)]
pub struct VcsSyncHistory {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub status: String,
    pub releases_found: i32,
    pub drafts_created: i32,
    pub error: Option<String>,
    pub rate_limit_remaining: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_sync_history)]
pub struct NewVcsSyncHistory {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub status: String,
    pub releases_found: i32,
    pub drafts_created: i32,
    pub error: Option<String>,
    pub rate_limit_remaining: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
}

// VCS provider settings. Tokens live in `*_encrypted` columns and must
// be encrypted before insert; the encryption helper is still open work.

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_provider_settings)]
pub struct VcsProviderSettings {
    pub provider: String,
    pub token_encrypted: Option<String>,
    pub webhook_secret_encrypted: Option<String>,
    pub api_base_override: Option<String>,
    pub gitlab_hosts: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_provider_settings)]
pub struct NewVcsProviderSettings {
    pub provider: String,
    pub token_encrypted: Option<String>,
    pub webhook_secret_encrypted: Option<String>,
    pub api_base_override: Option<String>,
    pub gitlab_hosts: Value,
}
