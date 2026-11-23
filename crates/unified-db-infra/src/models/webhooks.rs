// Diesel models for the webhooks domain of the unified-db-infra bounded context.
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
// Webhooks, import, and export (2026-11-22-000018_webhooks_import_export).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webhook_endpoints)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Value,
    #[serde(skip_serializing)]
    pub secret: String,
    pub active: bool,
    pub paused: bool,
    pub max_attempts: i32,
    pub backoff_base_secs: i64,
    pub timeout_secs: i64,
    pub delivery_count: i64,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webhook_endpoints)]
pub struct NewWebhookEndpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webhook_deliveries)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event: String,
    pub payload: Value,
    pub payload_hash: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webhook_deliveries)]
pub struct NewWebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event: String,
    pub payload_hash: String,
    pub status: String,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
}
