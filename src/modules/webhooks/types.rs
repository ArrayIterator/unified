// Webhooks - Types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// Types.

/// One subscribed endpoint. The signing secret is never serialized.
#[derive(Debug, Clone)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    /// HMAC signing secret. Never returned by any API response.
    pub secret: String,
    pub active: bool,
    pub paused: bool,
    pub max_attempts: u32,
    pub backoff_base_secs: u64,
    pub timeout_secs: u64,
    pub delivery_count: u64,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookEndpointView {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub secret_configured: bool,
    pub active: bool,
    pub paused: bool,
    pub max_attempts: u32,
    pub backoff_base_secs: u64,
    pub timeout_secs: u64,
    pub delivery_count: u64,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WebhookEndpoint {
    pub(crate) fn view(&self) -> WebhookEndpointView {
        WebhookEndpointView {
            id: self.id,
            name: self.name.clone(),
            url: self.url.clone(),
            events: self.events.clone(),
            secret_configured: !self.secret.is_empty(),
            active: self.active,
            paused: self.paused,
            max_attempts: self.max_attempts,
            backoff_base_secs: self.backoff_base_secs,
            timeout_secs: self.timeout_secs,
            delivery_count: self.delivery_count,
            last_delivery_at: self.last_delivery_at,
            last_status: self.last_status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Lifecycle of one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Queued,
    Delivering,
    Delivered,
    Failed,
    Retrying,
}

impl DeliveryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivering => "delivering",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Retrying => "retrying",
        }
    }
}

/// One delivery attempt record. The payload is kept so a failed
/// delivery can be redelivered manually with the exact same body.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event: String,
    pub payload: Value,
    /// SHA-256 hex digest of the serialized payload body.
    pub payload_hash: String,
    pub status: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub status_code: Option<u16>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

impl WebhookDelivery {
    pub(crate) fn set_status(&mut self, status: DeliveryStatus) {
        self.status = status.as_str().to_string();
        self.updated_at = Utc::now();
    }
}
