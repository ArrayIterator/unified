// Contracts - Request bodies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    #[serde(default)]
    pub placeholders: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub body: Option<String>,
    pub placeholders: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RenderTemplateRequest {
    #[serde(default)]
    pub values: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContractRequest {
    pub template_id: Option<Uuid>,
    pub title: String,
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    pub value: f64,
    pub currency: Option<String>,
    pub signing_order: Option<SigningOrder>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub alert_days: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContractRequest {
    pub title: Option<String>,
    pub company_party: Option<String>,
    pub counterparty_name: Option<String>,
    pub counterparty_email: Option<String>,
    pub terms: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub signing_order: Option<SigningOrder>,
    pub starts_at: Option<Option<DateTime<Utc>>>,
    pub ends_at: Option<Option<DateTime<Utc>>>,
    pub alert_days: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct AddSignerRequest {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub order_index: Option<i64>,
    /// Signing-link lifetime in days (1..=90, default 30).
    pub expires_in_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SignRequest {
    /// Optional confirmation that the signer reviewed the terms.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeclineRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct VoidRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct TerminateRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct RenewRequest {
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub alert_days: Vec<i64>,
}
