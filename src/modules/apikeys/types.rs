//! apikeys module - Key records, views and status.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Lifecycle of a key. `rotating` means a replacement was issued and
/// the key is inside its grace period; it becomes `revoked` when the
/// grace period ends (lazily, on the next authentication attempt).
/// A rotating key must always carry `grace_until: Some(..)`; a
/// rotating record without a grace period is denied on
/// authentication (see `service::authenticate_key`) and can never
/// authenticate indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyStatus {
    Active,
    Rotating,
    Revoked,
    Expired,
}

impl ApiKeyStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Rotating => "rotating",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}

/// One stored key. The full key value is never stored here.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub name: String,
    /// Optional owner (user id) the key was issued for.
    pub owner_id: Option<Uuid>,
    /// Masked display prefix, for example `unf_ab12...`.
    pub prefix: String,
    /// Last four characters of the key, for operator recognition.
    pub last4: String,
    /// HMAC-SHA256 hex digest of the full key under the server-side
    /// pepper, prefixed with `hmac1:` (keys issued after the pepper
    /// fix), or a bare plain SHA-256 hex digest (keys issued before
    /// it; still accepted on verification). Never serialized.
    pub key_hash: String,
    pub scopes: Vec<String>,
    /// Rate-limit tier name: `standard`, `elevated`, or `high`.
    pub rate_tier: String,
    /// Effective requests-per-minute limit for this key.
    pub requests_per_minute: u32,
    pub status: ApiKeyStatus,
    pub expires_at: Option<DateTime<Utc>>,
    /// End of the rotation grace period (rotating keys only).
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub usage_count: u64,
}

/// Public view of a key: everything except the secret material.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub prefix: String,
    pub last4: String,
    pub scopes: Vec<String>,
    pub rate_tier: String,
    pub requests_per_minute: u32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub usage_count: u64,
}

/// Create/rotate response: the public view plus the full key value,
/// shown exactly once. Callers must store it now; it cannot be
/// retrieved again through any endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyCreatedView {
    #[serde(flatten)]
    pub key: ApiKeyView,
    /// The full key value. Shown exactly once, at creation/rotation.
    pub full_key: String,
}

impl ApiKeyRecord {
    pub(crate) fn view(&self) -> ApiKeyView {
        ApiKeyView {
            id: self.id,
            name: self.name.clone(),
            owner_id: self.owner_id,
            prefix: self.prefix.clone(),
            last4: self.last4.clone(),
            scopes: self.scopes.clone(),
            rate_tier: self.rate_tier.clone(),
            requests_per_minute: self.requests_per_minute,
            status: self.status.as_str().to_string(),
            expires_at: self.expires_at,
            grace_until: self.grace_until,
            rotated_from: self.rotated_from,
            rotated_to: self.rotated_to,
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_used_at: self.last_used_at,
            usage_count: self.usage_count,
        }
    }
}

/// Result of a successful key authentication, for middleware use.
#[derive(Debug, Clone)]
pub struct AuthenticatedKey {
    pub id: Uuid,
    pub owner_id: Option<Uuid>,
    pub scopes: Vec<String>,
    pub rate_tier: String,
    pub requests_per_minute: u32,
}
