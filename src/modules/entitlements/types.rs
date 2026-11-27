// Entitlement types.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::*;

// Types.

/// One entitlement row: one module granted to one tenant from one
/// source. The effective entitlement for a module is the union of
/// its active rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entitlement {
    pub id: Uuid,
    /// Tenant namespace id (uuidv7) the row belongs to. Rows are
    /// keyed by `company_id` for lookups, but the tenant id is
    /// stored on every row so a tenant-schema move never has to
    /// guess which tenant a row belongs to.
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapsed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapse_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One source contributing to an effective entitlement, as shown to
/// callers. A license-derived source has no stored entitlement id:
/// the licensing verification API exposes no license id, so the
/// source is identified by kind only and `source_id` is null.
#[derive(Debug, Clone, Serialize)]
pub struct EntitlementSourceView {
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement_id: Option<Uuid>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// One entry of the append-only entitlement history. History is
/// never rewritten: a correction is a new entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitlementHistoryEntry {
    pub id: Uuid,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement_id: Option<Uuid>,
    pub company_id: String,
    pub module_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

/// One denied enable attempt, recorded for audit and for the AI
/// suggestion (which modules the tenant tried to use most).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeniedAttempt {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub reason: String,
    pub error_code: String,
    pub created_at: DateTime<Utc>,
}

/// One package/product-set change: the computed module diff plus
/// the decision. `requires_confirmation` modules are never
/// disabled until the confirm call names them explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageChange {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub new_module_ids: Vec<String>,
    pub newly_entitled: Vec<String>,
    pub kept: Vec<String>,
    pub kept_via_other_source: Vec<String>,
    pub requires_confirmation: Vec<String>,
    #[serde(default)]
    pub disabled_module_ids: Vec<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whether this module enabled a module for a tenant, so the
/// dashboard can tell an enabled module from an entitled-but-
/// never-activated one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleActivation {
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub enabled: bool,
    pub enabled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Admin-editable entitlement settings. The values below are
/// CONFIGURABLE DEFAULTS: `Default` is what a fresh install uses,
/// and `PUT /settings` replaces them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitlementSettings {
    /// Read-only grace period in days after an entitlement lapses.
    /// 0 means no grace: a lapsed module is disabled at once.
    pub grace_period_days: i64,
    /// When false, a lapsed module skips grace and is disabled at
    /// once even when `grace_period_days` is positive.
    pub read_only_grace_enabled: bool,
    /// Recorded default-deny policy. Fail-closed is an invariant
    /// and is enforced regardless of this value: no entitlement
    /// record always means not entitled. This setting only governs
    /// the reported policy for unregistered-module lookups.
    pub default_deny: bool,
}

impl Default for EntitlementSettings {
    fn default() -> Self {
        Self {
            grace_period_days: DEFAULT_GRACE_PERIOD_DAYS,
            read_only_grace_enabled: true,
            default_deny: true,
        }
    }
}

impl EntitlementSettings {
    pub(crate) fn validate(&self) -> UnifiedResult<()> {
        if self.grace_period_days < 0 || self.grace_period_days > MAX_GRACE_PERIOD_DAYS {
            return Err(UnifiedError::bad_request(
                "grace_period_days must be between 0 and 365",
            ));
        }
        Ok(())
    }
}

/// Effective access state of one module for one tenant, computed
/// from the union of active sources plus the licensing check.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveEntitlement {
    pub module_id: String,
    pub company_id: String,
    /// `entitled`, `grace_read_only`, or `locked`.
    pub state: String,
    pub entitled: bool,
    /// True while the module is in read-only grace: reads and data
    /// export are allowed, writes are rejected.
    pub read_only: bool,
    pub write_allowed: bool,
    pub sources: Vec<EntitlementSourceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// One module as listed by `GET /my-modules`, with its
/// per-tenant state and, when locked, its upgrade path.
#[derive(Debug, Clone, Serialize)]
pub struct MyModuleView {
    pub module_id: String,
    pub name: String,
    pub description: String,
    pub available_as_product: bool,
    pub state: String,
    pub entitled: bool,
    pub read_only: bool,
    pub write_allowed: bool,
    pub enabled: bool,
    pub sources: Vec<EntitlementSourceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub upgrade_path: UpgradePath,
}

/// The upgrade path for a locked module: the sellable products or
/// packages that unlock it, resolved from product/licensing data
/// through the hook bus - never a hard-coded map. When no provider
/// answers, the path is null with a clear reason, never a
/// fabricated product.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradePath {
    pub available: bool,
    pub products: Vec<UpgradeProduct>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One product or package that unlocks a module, as reported by a
/// hook handler. `kind` is `package`, `product`, or `license`, and
/// is data from the answering module, not a list kept here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeProduct {
    pub kind: String,
    pub id: String,
    pub name: String,
}

/// One AI package suggestion for a tenant, grounded in the denied
/// attempt log. A suggestion only: it never purchases anything.
#[derive(Debug, Clone, Serialize)]
pub struct PackageSuggestion {
    pub module_id: String,
    pub denied_attempts: u64,
    pub upgrade_path: UpgradePath,
    pub note: String,
}

/// Tenant query for tenant-scoped reads: the requested company
/// only. Caller identity is NOT part of this shape: `viewer_id`
/// / `viewer_role` query parameters used to live here and were
/// read as identity, which let any caller authorize themselves
/// with `?viewer_role=admin`. Identity now comes only from the
/// `X-User-Id` / `X-Role` request headers (see `caller_of` in
/// `helpers.rs`); a `viewer_*` query parameter is not even
/// deserialized, so it can never be mistaken for the caller.
#[derive(Debug, Clone, Deserialize)]
pub struct ViewerQuery {
    #[serde(default)]
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGrantRequest {
    pub company_id: String,
    #[serde(default)]
    pub tenant_id: Option<String>,
    pub module_id: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub granted_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateGrantRequest {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnableModuleRequest {
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyPackageChangeRequest {
    pub company_id: String,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    pub new_module_ids: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfirmPackageChangeRequest {
    #[serde(default)]
    pub confirm: Option<bool>,
    #[serde(default)]
    pub module_ids: Option<Vec<String>>,
    #[serde(default)]
    pub decided_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiSuggestRequest {
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SweepSummary {
    pub scanned: usize,
    pub entered_grace: usize,
    pub disabled: usize,
    pub already_settled: usize,
}
