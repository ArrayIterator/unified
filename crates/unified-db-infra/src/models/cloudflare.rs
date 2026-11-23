// Diesel models for the cloudflare domain of the unified-db-infra bounded context.
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
// Cloudflare (2026-11-22-000022_cloudflare).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_settings)]
pub struct CloudflareSettings {
    pub key: String,
    #[serde(skip_serializing)]
    pub api_token_encrypted: String,
    pub account_id: Option<String>,
    pub default_zone_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_settings)]
pub struct NewCloudflareSettings {
    pub key: String,
    pub account_id: Option<String>,
    pub default_zone_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_purge_history)]
pub struct CloudflarePurgeHistory {
    pub id: Uuid,
    pub zone_id: String,
    pub kind: String,
    pub targets: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_purge_history)]
pub struct NewCloudflarePurgeHistory {
    pub id: Uuid,
    pub zone_id: String,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_detections)]
pub struct CloudflareDetection {
    pub id: Uuid,
    pub domain: String,
    pub detected: bool,
    pub methods: Value,
    pub ssl_mode: Option<String>,
    pub end_to_end_encrypted: Option<bool>,
    pub flexible_warning: Option<String>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_detections)]
pub struct NewCloudflareDetection {
    pub id: Uuid,
    pub domain: String,
    pub ssl_mode: Option<String>,
    pub end_to_end_encrypted: Option<bool>,
    pub flexible_warning: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_ip_ranges)]
pub struct CloudflareIpRanges {
    pub key: String,
    pub v4: Value,
    pub v6: Value,
    pub fetched_at: Option<DateTime<Utc>>,
    pub source_v4: String,
    pub source_v6: String,
    pub fetch_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_ip_ranges)]
pub struct NewCloudflareIpRanges {
    pub key: String,
    pub fetched_at: Option<DateTime<Utc>>,
    pub fetch_error: Option<String>,
}
