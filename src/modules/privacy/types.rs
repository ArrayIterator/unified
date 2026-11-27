// Privacy types: visibility, records, notices, inputs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::*;

// Visibility

/// Visibility of a record or collection. Ordered narrowest to
/// widest; new records default to the narrowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Private,
    Company,
    Public,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Company => "company",
            Visibility::Public => "public",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "private" => Some(Visibility::Private),
            "company" => Some(Visibility::Company),
            "public" => Some(Visibility::Public),
            _ => None,
        }
    }
}

/// The default visibility for new records: always private.
/// Callers widen it only through an explicitly configured
/// per-module default (see `PrivacyState::effective_visibility`).
pub fn default_visibility() -> Visibility {
    Visibility::Private
}

// Types

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDeclaration {
    pub id: String,
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    pub required: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonymizationRecord {
    pub id: String,
    pub user_id: String,
    /// Derived HMAC-SHA-256 hex token. Not reversible, and no
    /// original value is stored anywhere in this record.
    pub token: String,
    /// Per-field derived tokens, keyed by field name.
    pub field_tokens: HashMap<String, String>,
    pub scope_modules: Vec<String>,
    pub reason: String,
    pub actor: String,
    /// How many times anonymization was applied to this user.
    /// Re-applying yields the same tokens and only bumps this.
    pub times_applied: u64,
    pub anonymized_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityDefault {
    pub module_id: String,
    pub visibility: Visibility,
    /// True only when an admin explicitly widened the default
    /// beyond private. Private defaults are never "explicit".
    pub explicit: bool,
    pub configured_by: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentTypeDef {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub id: String,
    pub user_id: String,
    pub consent_type: String,
    pub granted: bool,
    pub granted_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ConsentRecord {
    /// Current status, derived from the stored flags and expiry:
    /// `withdrawn` wins over expiry, expiry wins over granted.
    pub fn status(&self, now: DateTime<Utc>) -> &'static str {
        if !self.granted || self.withdrawn_at.is_some() {
            return "withdrawn";
        }
        if let Some(expires_at) = self.expires_at {
            if now > expires_at {
                return "expired";
            }
        }
        "granted"
    }

    pub fn is_currently_granted(&self, now: DateTime<Utc>) -> bool {
        self.status(now) == "granted"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentHistoryEntry {
    pub id: String,
    pub consent_id: String,
    pub user_id: String,
    pub consent_type: String,
    pub action: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErasureRequest {
    pub id: String,
    pub user_id: String,
    pub modules: Vec<String>,
    pub reason: String,
    pub status: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub anonymization_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyNotice {
    pub id: String,
    pub module_id: String,
    pub text: String,
    pub version: u64,
    pub effective_from: DateTime<Utc>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyAuditEntry {
    pub id: String,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentNotice {
    pub module_id: String,
    pub notice_id: Option<String>,
    pub text: String,
    pub version: u64,
    pub effective_from: Option<DateTime<Utc>>,
    /// True when no module notice exists and the generic
    /// fallback text is returned instead.
    pub generic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveVisibility {
    pub module_id: String,
    pub visibility: Visibility,
    /// `configured` when an admin set a per-module default,
    /// `default` when the private fallback applies.
    pub source: String,
    pub explicit: bool,
}

// Request inputs

#[derive(Debug, Clone, Deserialize)]
pub struct RedactInput {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub redact_names: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFieldDeclarationInput {
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFieldDeclarationInput {
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidatePayloadInput {
    pub module_id: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnonymizeInput {
    pub user_id: String,
    #[serde(default)]
    pub scope_modules: Vec<String>,
    #[serde(default)]
    pub reason: String,
    /// Personal-data field names to derive per-field tokens for.
    #[serde(default)]
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetVisibilityInput {
    pub module_id: String,
    pub visibility: String,
    /// Must be true to widen the default beyond private.
    #[serde(default)]
    pub explicit: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConsentTypeInput {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantConsentInput {
    pub user_id: String,
    pub consent_type: String,
    pub granted: bool,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateErasureInput {
    pub user_id: String,
    #[serde(default)]
    pub modules: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectErasureInput {
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNoticeInput {
    pub module_id: String,
    pub text: String,
    pub version: u64,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurrentConsentQuery {
    pub user_id: String,
    pub consent_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurrentNoticeQuery {
    pub module_id: String,
}
