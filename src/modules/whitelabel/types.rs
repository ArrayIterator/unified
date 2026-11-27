// Whitelabel module - shared types and request/response shapes.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::constants::{DOMAIN_NONE, MODE_ALLOWED, MODE_DENIED, MODE_INHERIT};

// Types

/// Per-company allowance override mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AllowanceMode {
    Allowed,
    Denied,
    Inherit,
}

impl AllowanceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AllowanceMode::Allowed => MODE_ALLOWED,
            AllowanceMode::Denied => MODE_DENIED,
            AllowanceMode::Inherit => MODE_INHERIT,
        }
    }

    pub fn parse(value: &str) -> UnifiedResult<Self> {
        match value {
            MODE_ALLOWED => Ok(AllowanceMode::Allowed),
            MODE_DENIED => Ok(AllowanceMode::Denied),
            MODE_INHERIT => Ok(AllowanceMode::Inherit),
            other => Err(UnifiedError::bad_request(format!(
                "allowance mode must be \"allowed\", \"denied\" or \"inherit\", got \"{other}\""
            ))),
        }
    }
}

/// Global admin setting: whether white labelling is allowed by
/// default. Default `false` (deny) - fail closed.
#[derive(Debug, Clone, Serialize)]
pub struct GlobalAllowance {
    pub allow_whitelabel: bool,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Per-company allowance override.
#[derive(Debug, Clone, Serialize)]
pub struct CompanyAllowance {
    pub company_id: String,
    pub mode: AllowanceMode,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// The admin-editable platform defaults record. This is the source of
/// truth for every default brand value; `seed()` only provides its
/// initial contents on a fresh install (see the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalDefaults {
    pub display_name: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    pub sender_name: String,
    pub sender_email: String,
    pub login_tagline: String,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub powered_by_text: String,
}

impl GlobalDefaults {
    /// Initial contents of the admin-editable defaults record on a
    /// fresh install. These are seed values only: once an admin saves
    /// the defaults record, the stored record is the source of truth
    /// and these values are never consulted again. They exist so the
    /// server has something to render before first configuration.
    pub fn seed() -> Self {
        Self {
            display_name: "Unified".to_string(),
            logo_storage_target: None,
            logo_storage_key: None,
            favicon_storage_target: None,
            favicon_storage_key: None,
            primary_color: "#2563EB".to_string(),
            accent_color: "#0EA5E9".to_string(),
            background_color: "#FFFFFF".to_string(),
            text_color: "#111827".to_string(),
            sender_name: "Unified".to_string(),
            sender_email: String::new(),
            login_tagline: "Manage your business in one place.".to_string(),
            login_background_storage_target: None,
            login_background_storage_key: None,
            powered_by_text: "Powered by Unified".to_string(),
        }
    }
}

/// One tenant branding profile. Every brand field is optional so
/// resolution can fall through per field: tenant override, then
/// company default, then the global defaults record. A profile with
/// `tenant_id == None` is the company default; a profile with a
/// `tenant_id` is that tenant's override inside the company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrandingProfile {
    pub id: Uuid,
    pub company_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_domain: Option<String>,
    pub domain_status: String,
    pub hide_powered_by: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_logo_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_logo_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_background_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_background_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_tagline: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BrandingProfile {
    pub(crate) fn new(company_id: &str, tenant_id: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            company_id: company_id.to_string(),
            tenant_id,
            display_name: None,
            logo_storage_target: None,
            logo_storage_key: None,
            favicon_storage_target: None,
            favicon_storage_key: None,
            primary_color: None,
            accent_color: None,
            background_color: None,
            text_color: None,
            custom_domain: None,
            domain_status: DOMAIN_NONE.to_string(),
            hide_powered_by: false,
            sender_name: None,
            sender_email: None,
            login_logo_storage_target: None,
            login_logo_storage_key: None,
            login_background_storage_target: None,
            login_background_storage_key: None,
            login_tagline: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Fully resolved branding for one request, as returned by
/// `GET /branding`, the public branding route, and the
/// `whitelabel.branding.resolve` hook. `allowed` reports whether
/// white labelling is permitted for this tenant;
/// `enforced_platform_default` is true when the tenant is not
/// allowed, meaning every value below came from the global defaults
/// record and all tenant/company overrides were ignored.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedBranding {
    pub company_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub allowed: bool,
    pub enforced_platform_default: bool,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo_storage_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo_storage_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon_storage_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon_storage_key: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_domain: Option<String>,
    pub domain_status: String,
    pub hide_powered_by: bool,
    pub sender_name: String,
    pub sender_email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_logo_storage_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_logo_storage_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_background_storage_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_background_storage_key: Option<String>,
    pub login_tagline: String,
    pub powered_by_text: String,
}

/// One branding history entry: a change or a disallowed attempt.
/// `allowed` is false for denied attempts; `field` names the changed
/// field (or `"*"` for a whole-profile write); `old_value` /
/// `new_value` are plain strings, never secrets (this module stores
/// no secrets).
#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub company_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<String>,
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
}

// Request inputs

#[derive(Debug, Deserialize)]
pub struct PutGlobalAllowanceInput {
    pub allow_whitelabel: bool,
}

#[derive(Debug, Deserialize)]
pub struct PutCompanyAllowanceInput {
    pub mode: String,
}

/// Partial update of the admin-editable global defaults record.
/// Storage references use the double-Option shape from the portals
/// module: outer `None` = leave unchanged, inner `None` = clear.
#[derive(Debug, Deserialize, Default)]
pub struct PutDefaultsInput {
    pub display_name: Option<String>,
    pub logo_storage_target: Option<Option<String>>,
    pub logo_storage_key: Option<Option<String>>,
    pub favicon_storage_target: Option<Option<String>>,
    pub favicon_storage_key: Option<Option<String>>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub login_tagline: Option<String>,
    pub login_background_storage_target: Option<Option<String>>,
    pub login_background_storage_key: Option<Option<String>>,
    pub powered_by_text: Option<String>,
}

/// Tenant branding write. Every field is optional; omitted fields
/// keep their current value. Storage references use the double-Option
/// shape: outer `None` = leave unchanged, inner `None` = clear.
/// `hide_powered_by` is a plain bool: present means set, absent
/// means leave unchanged.
#[derive(Debug, Deserialize, Default)]
pub struct PutBrandingInput {
    pub display_name: Option<String>,
    pub logo_storage_target: Option<Option<String>>,
    pub logo_storage_key: Option<Option<String>>,
    pub favicon_storage_target: Option<Option<String>>,
    pub favicon_storage_key: Option<Option<String>>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub custom_domain: Option<Option<String>>,
    pub hide_powered_by: Option<bool>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub login_logo_storage_target: Option<Option<String>>,
    pub login_logo_storage_key: Option<Option<String>>,
    pub login_background_storage_target: Option<Option<String>>,
    pub login_background_storage_key: Option<Option<String>>,
    pub login_tagline: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowance_mode_round_trip() {
        assert_eq!(AllowanceMode::Allowed.as_str(), "allowed");
        assert_eq!(AllowanceMode::Denied.as_str(), "denied");
        assert_eq!(AllowanceMode::Inherit.as_str(), "inherit");
        assert_eq!(AllowanceMode::parse("denied").unwrap(), AllowanceMode::Denied);
        assert_eq!(
            AllowanceMode::parse(AllowanceMode::Allowed.as_str()).unwrap(),
            AllowanceMode::Allowed
        );
        assert!(AllowanceMode::parse("bogus").is_err());
    }

    #[test]
    fn global_defaults_seed_values() {
        let defaults = GlobalDefaults::seed();
        assert_eq!(defaults.display_name, "Unified");
        assert_eq!(defaults.primary_color, "#2563EB");
        assert_eq!(defaults.background_color, "#FFFFFF");
        assert_eq!(defaults.powered_by_text, "Powered by Unified");
    }
}
