// Contracts - Types.

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

// Types

/// Lifecycle of a contract: `draft` -> `sent` ->
/// `partially_signed` -> `signed` -> `active` -> `expired` /
/// `terminated`; `voided` is reachable from draft / sent /
/// `partially_signed` / `signed` / `active` and is terminal.
/// `expired` is materialised by the scheduler listener (or the
/// manual check-expiry endpoint) once the end date passed while
/// the contract was `active`; it is also derived at read time so
/// a list never shows a stale `active` past its end date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    Draft,
    Sent,
    PartiallySigned,
    Signed,
    Active,
    Expired,
    Terminated,
    Voided,
}

impl Default for ContractStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl ContractStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Sent => "sent",
            Self::PartiallySigned => "partially_signed",
            Self::Signed => "signed",
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Terminated => "terminated",
            Self::Voided => "voided",
        }
    }
}

/// Per-signer status: `pending` -> `viewed` -> `signed`, or
/// `declined` (terminal for that signer; the contract stays
/// signable by the remaining signers unless it is voided).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerStatus {
    Pending,
    Viewed,
    Signed,
    Declined,
}

impl Default for SignerStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl SignerStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Viewed => "viewed",
            Self::Signed => "signed",
            Self::Declined => "declined",
        }
    }
}

/// Signing order: `sequential` signs in `order_index` order
/// (a signer may only sign once every lower index signed);
/// `parallel` lets every signer sign in any order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningOrder {
    Sequential,
    Parallel,
}

impl Default for SigningOrder {
    fn default() -> Self {
        Self::Sequential
    }
}

impl SigningOrder {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
        }
    }
}

/// One contract template. `body` holds the template text with
/// `{{placeholder}}` markers; `placeholders` is the closed list
/// of names the body may use - any other marker in the body is
/// rejected at save time. Values are substituted server-side and
/// HTML-escaped at render time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractTemplate {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placeholders: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    pub title: String,
    /// The company side of the contract.
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    /// Monetary value in `currency` (NUMERIC(19,4) in the
    /// migration; non-negative, rounded to 4 decimals here).
    pub value: f64,
    pub currency: String,
    pub signing_order: SigningOrder,
    pub status: ContractStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    /// SHA-256 hash of the sealed content, recorded when the last
    /// signer signs. While set, the contract cannot be edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate_reason: Option<String>,
    /// Id of the contract this one was renewed from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewed_from_id: Option<Uuid>,
    /// Expiry alert offsets (days before `ends_at`) armed for
    /// this contract; defaults to the module settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alert_days: Vec<i64>,
    /// `not_scheduled`, `queued`, or `declined` (see the scheduler
    /// note in the module header).
    pub expiry_alert_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One signer of a contract. Only the SHA-256 hash of the signing
/// token is stored - the raw token is returned once, at creation
/// and at resend, and never persisted or logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSigner {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub name: String,
    pub email: String,
    /// 0-based position in the signing order.
    pub order_index: i64,
    pub status: SignerStatus,
    /// SHA-256 hex of the current signing token. Serialized as
    /// `token_hash` (never the raw token).
    pub token_hash: String,
    pub token_expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audit-trail entry. `ip_hash` and `user_agent_hash` are
/// SHA-256 hex digests; the raw IP address and raw user agent are
/// never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractAuditEntry {
    pub id: Uuid,
    pub contract_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub ip_hash: String,
    pub user_agent_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}
