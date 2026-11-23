// Domain Verification — Request types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSettingsRequest {
    #[serde(default)]
    pub(crate) record_type: Option<String>,
    #[serde(default)]
    pub(crate) host_template: Option<String>,
    #[serde(default)]
    pub(crate) value_template: Option<String>,
    #[serde(default)]
    pub(crate) token_length: Option<usize>,
    #[serde(default)]
    pub(crate) token_alphabet: Option<String>,
    #[serde(default)]
    pub(crate) verify_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) max_attempts: Option<u32>,
    #[serde(default)]
    pub(crate) retry_delay_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) token_ttl_seconds: Option<i64>,
    #[serde(default)]
    pub(crate) rate_limit_window_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueTokenRequest {
    pub(crate) domain: String,
    #[serde(default)]
    pub(crate) alias_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) tenant_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VerifyRequest {
    pub(crate) domain: String,
    #[serde(default)]
    pub(crate) alias_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) tenant_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StatusQuery {
    pub(crate) domain: String,
}

/// Issuance response: the EXACT DNS record the user dashboard must
/// display, plus the raw token exactly once. Nothing in this payload
/// is stored anywhere else.
#[derive(Debug, Serialize)]
pub(crate) struct TokenIssuance {
    pub(crate) token_id: Uuid,
    pub(crate) domain: String,
    pub(crate) alias_id: Option<Uuid>,
    pub(crate) tenant_id: Option<Uuid>,
    pub(crate) company_id: Option<Uuid>,
    pub(crate) record_type: String,
    pub(crate) host: String,
    pub(crate) value: String,
    /// The raw token, shown exactly once. It is never stored.
    pub(crate) token: String,
    pub(crate) settings_version: u64,
    pub(crate) issued_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) status: String,
}

/// Outcome of one verify call: the result, the exact record found
/// (or an explicit "not found" with what was looked up), and the
/// attempt history summary.
#[derive(Debug, Serialize)]
pub(crate) struct VerifyOutcome {
    pub(crate) domain: String,
    pub(crate) alias_id: Option<Uuid>,
    pub(crate) tenant_id: Option<Uuid>,
    pub(crate) company_id: Option<Uuid>,
    pub(crate) status: String,
    pub(crate) record_type: String,
    pub(crate) host: String,
    pub(crate) record_found: Option<String>,
    pub(crate) looked_up: Value,
    pub(crate) attempts_made: u32,
    pub(crate) verified_at: Option<DateTime<Utc>>,
    pub(crate) error: Option<String>,
}
