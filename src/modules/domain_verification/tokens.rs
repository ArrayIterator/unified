// Domain Verification — Tokens and attempts (in-memory stores; authoritative).

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
// Tokens and attempts (in-memory stores; authoritative).
// ---------------------------------------------------------------------------

/// One issued verification token. The raw token is NEVER stored:
/// only its SHA-256 hex digest (`token_hash`) is kept. The rendered
/// `host` / `record_type` / `expected_value` are snapshotted at
/// issuance so a later admin settings change never rewrites what an
/// already-issued token must match.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationToken {
    pub id: Uuid,
    pub domain: String,
    #[serde(skip_serializing)]
    pub alias_id: Option<Uuid>,
    #[serde(skip_serializing)]
    pub tenant_id: Option<Uuid>,
    #[serde(skip_serializing)]
    pub company_id: Option<Uuid>,
    /// SHA-256 hex digest of the raw token. Never exposed via the API.
    #[serde(skip_serializing)]
    pub token_hash: String,
    /// Record type the token was issued for (`"txt"` / `"cname"`).
    pub record_type: String,
    /// Exact host the tenant must publish the record at.
    pub host: String,
    /// Exact value the record must hold (contains the raw token;
    /// stored so verification compares against issuance-time
    /// settings, not current settings).
    #[serde(skip_serializing)]
    pub expected_value: String,
    /// Settings version the token was issued under.
    pub settings_version: u64,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

/// Public view of a token: metadata only. The hash and the expected
/// value are never exposed after issuance.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationTokenView {
    pub id: Uuid,
    pub domain: String,
    pub record_type: String,
    pub host: String,
    pub settings_version: u64,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

impl From<&VerificationToken> for VerificationTokenView {
    fn from(token: &VerificationToken) -> Self {
        Self {
            id: token.id,
            domain: token.domain.clone(),
            record_type: token.record_type.clone(),
            host: token.host.clone(),
            settings_version: token.settings_version,
            status: token.status.clone(),
            issued_at: token.issued_at,
            expires_at: token.expires_at,
            verified_at: token.verified_at,
        }
    }
}

/// One DNS check attempt inside a verify call.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationAttempt {
    pub id: Uuid,
    pub token_id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub attempt_number: u32,
    /// `"verified"` / `"pending"` / `"failed"` for this attempt.
    pub result: String,
    /// The exact DNS record that matched, when one did.
    pub record_found: Option<String>,
    /// What was looked up (host, type, expected-value prefix).
    pub looked_up: Value,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

pub(crate) static TOKENS: OnceLock<DashMap<Uuid, VerificationToken>> = OnceLock::new();
pub(crate) static ATTEMPTS: OnceLock<DashMap<Uuid, VerificationAttempt>> = OnceLock::new();

pub(crate) fn tokens_store() -> &'static DashMap<Uuid, VerificationToken> {
    TOKENS.get_or_init(DashMap::new)
}

pub(crate) fn attempts_store() -> &'static DashMap<Uuid, VerificationAttempt> {
    ATTEMPTS.get_or_init(DashMap::new)
}
