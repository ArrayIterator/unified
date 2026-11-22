// White Labelling module.
//
// Server-side white labelling for tenants (companies), gated by an
// admin setting and enforced on the server for every request. When
// white labelling is NOT allowed for a tenant, Unified branding stays
// everywhere: logo, "Powered by Unified", default colours, default
// email sender, and default login page. The client is never trusted
// to decide branding: every render path (theme, portals, mail, login)
// must resolve branding through this module, on the server, per
// request and per tenant.
//
// # Configurable defaults (no hard-coded branding)
//
// This module follows the standing modularity rule: nothing about
// branding is hard-coded as the only source of truth.
//
// - The platform defaults (default logo, default colours, default
//   sender name/address, default product name, default login
//   tagline, "Powered by Unified" text) live in the admin-editable
//   `whitelabel_global_defaults` record, managed through
//   `GET/PUT /api/v1/whitelabel/admin/defaults` (capability
//   `whitelabel.manage`). The `GlobalDefaults::seed()` values below
//   are ONLY the initial contents of that record on a fresh install,
//   so the server has something to render before an admin saves
//   defaults; once saved, the stored record is the source of truth
//   and the seed values are never consulted again.
// - There is no hard-coded list of allowed colours, brand fields, or
//   tenants. Colours are validated as a format only (`#RGB` /
//   `#RRGGBB` hex, same rule as the portals module); brand fields
//   are the profile schema, not a list; tenants are whatever
//   `company_id` values arrive in requests.
// - The closed value sets in this file are state machines, not
//   branding lists: allowance mode (`allowed` / `denied` /
//   `inherit`) and domain status (`none` / `requested` /
//   `verified` / `rejected`, the same set the portals module uses
//   for its custom domains).
//
// # Allowance (global + per-company)
//
// - Global default: `allow_whitelabel`, admin-managed, default
//   `false` (deny). Fail closed, matching the API Manager's
//   default-deny posture for public access.
// - Per-company override: `allowed` / `denied` / `inherit`
//   (`inherit` follows the global default). Admin-managed.
// - Effective allowance: per-company override wins unless it is
//   `inherit`, in which case the global default applies. Pure
//   function `effective_allowance` below; tenants can read their own
//   effective allowance at `GET /api/v1/whitelabel/allowance`.
//
// # Branding resolution order
//
// Tenant override -> company default -> global default. A branding
// profile is stored per `(company_id, tenant_id)`; a profile with no
// `tenant_id` is the company default, a profile with a `tenant_id`
// is that tenant's override. Every field resolves independently:
// the tenant value wins when set, else the company default, else the
// admin-editable global defaults record. Resolution is the pure,
// testable function `resolve_branding` over those three inputs plus
// the effective allowance. When the tenant is NOT allowed, the
// resolved branding is the global defaults record in full — tenant
// and company overrides are ignored entirely.
//
// # How other modules consume this module (hook bus)
//
// - `whitelabel.branding.resolve` (request): payload
//   `{"company_id", "tenant_id"?}`. Answered with the resolved
//   branding object (same shape as `GET /branding`). Theme, portals,
//   mail, and login rendering call this through the hook bus instead
//   of reading branding themselves or trusting client input.
// - `whitelabel.allowance.check` (request): payload
//   `{"company_id"}`. Answered with `{"company_id", "allowed"}`.
// - `whitelabel.domain.requested` (emitted): payload
//   `{"company_id", "tenant_id"?, "domain"}`. The domains module
//   verifies and provisions the domain; this module never verifies
//   a domain itself.
// - `whitelabel.sender.requested` (emitted): payload
//   `{"company_id", "tenant_id"?, "sender_name", "sender_email"}`.
//   The mail module applies the tenant sender; this module never
//   sends mail itself.
// - `domain.verify` (listened to, emitted by the domains module):
//   payload `{"domain", "dns_ok", "mx_ok", "ssl_ok",
//   "ssl_status"}`. A matching profile's `domain_status` moves to
//   `verified` (dns_ok true) or `rejected` (dns_ok false).
//
// # Events emitted (audit trail)
//
// - `whitelabel.allowance.changed` — global or per-company allowance
//   changed: `{"scope", "company_id"?, "mode"/"allow_whitelabel",
//   "actor"}`.
// - `whitelabel.defaults.changed` — global defaults record changed:
//   `{"actor", "fields"}`.
// - `whitelabel.branding.changed` — tenant branding saved:
//   `{"company_id", "tenant_id"?, "actor", "fields"}`.
// - `whitelabel.branding.denied` — a tenant without permission tried
//   to set custom branding (rejected with 403):
//   `{"company_id", "tenant_id"?, "actor", "fields"}`.
// - `whitelabel.branding.reset` — tenant branding reset to defaults.
// - `whitelabel.domain.verified` / `whitelabel.domain.rejected` —
//   domain verification outcome applied to a profile.
// Every change AND every disallowed attempt is also recorded in the
// module's own history store (table `whitelabel_history`), which the
// audit module can subscribe to through the events above. There is
// no direct call into the audit module: interconnection happens
// through the hook/event bus only.
//
// # Storage references, not bytes
//
// Logos and favicons are storage references only (storage target +
// key, exactly like the portals module logo): the bytes live in the
// storage module. Target and key must always be given together.
// No secret is ever stored here: sender credentials stay in the
// mail module; only the display sender name/address are kept.
//
// # Actor identity and capability checks
//
// Until the auth module provides an extractor, the caller is
// identified by the `X-User-Id` header (required) and `X-Role`
// carries the role slug the capability check runs against, exactly
// like the portals and ai modules. Tenant endpoints also require
// `X-Company-Id`; `X-Tenant-Id` is optional and selects the tenant
// override inside the company. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Capabilities: `whitelabel.manage` (admin), `whitelabel.view`
// (tenant read), `whitelabel.branding.manage` (tenant write).
// These three permissions must exist in the role definitions or
// every check fails closed.
//
// # Public-access gate
//
// `GET /api/v1/whitelabel/public/branding/{company_id}` returns the
// server-resolved branding for a login/portal page render. Like the
// portals public route it carries no capability check on purpose:
// it is gated by the API Manager's admin-managed `ApiAccessRule`
// records (default deny — an administrator must create the matching
// rule for `GET /api/v1/whitelabel/public/*`). Without the rule the
// API Manager denies the call. It only ever returns resolved
// branding (defaults when the tenant is not allowed); it never
// exposes allowance settings, history, or other tenants' data.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use lettre::Address;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Capability: manage the global allowance, per-company overrides,
/// and the global defaults record (admin only).
pub const PERM_MANAGE: &str = "whitelabel.manage";
/// Capability: read the caller's own allowance and resolved branding.
pub const PERM_VIEW: &str = "whitelabel.view";
/// Capability: write the caller's own branding profile.
pub const PERM_BRANDING_MANAGE: &str = "whitelabel.branding.manage";

/// Event: allowance (global or per-company) changed.
pub const EVENT_ALLOWANCE_CHANGED: &str = "whitelabel.allowance.changed";
/// Event: global defaults record changed.
pub const EVENT_DEFAULTS_CHANGED: &str = "whitelabel.defaults.changed";
/// Event: tenant branding saved.
pub const EVENT_BRANDING_CHANGED: &str = "whitelabel.branding.changed";
/// Event: tenant without permission attempted to set custom branding.
pub const EVENT_BRANDING_DENIED: &str = "whitelabel.branding.denied";
/// Event: tenant branding reset to defaults.
pub const EVENT_BRANDING_RESET: &str = "whitelabel.branding.reset";
/// Event: a custom domain was set or changed; the domains module
/// verifies and provisions it.
pub const EVENT_DOMAIN_REQUESTED: &str = "whitelabel.domain.requested";
/// Event: domain verification outcome applied to a profile.
pub const EVENT_DOMAIN_VERIFIED: &str = "whitelabel.domain.verified";
/// Event: domain verification failed for a profile.
pub const EVENT_DOMAIN_REJECTED: &str = "whitelabel.domain.rejected";
/// Event: a custom email sender was set; the mail module applies it.
pub const EVENT_SENDER_REQUESTED: &str = "whitelabel.sender.requested";

/// Hook request: resolve branding for a company/tenant.
/// Payload `{"company_id", "tenant_id"?}`; answered with the
/// resolved branding object.
pub const HOOK_BRANDING_RESOLVE: &str = "whitelabel.branding.resolve";
/// Hook request: check effective allowance for a company.
/// Payload `{"company_id"}`; answered with
/// `{"company_id", "allowed"}`.
pub const HOOK_ALLOWANCE_CHECK: &str = "whitelabel.allowance.check";

/// Maximum lengths enforced on user supplied text.
pub const MAX_NAME_LEN: usize = 200;
pub const MAX_TAGLINE_LEN: usize = 300;
pub const MAX_DOMAIN_LEN: usize = 253;
pub const MAX_STORAGE_REF_LEN: usize = 512;
pub const MAX_COMPANY_ID_LEN: usize = 128;
pub const MAX_TENANT_ID_LEN: usize = 128;

/// Allowance mode values. Closed set on purpose: this is a state
/// machine for the override, not a branding list.
pub const MODE_ALLOWED: &str = "allowed";
pub const MODE_DENIED: &str = "denied";
pub const MODE_INHERIT: &str = "inherit";

/// Domain status values, the same set the portals module uses for
/// its custom domains.
pub const DOMAIN_NONE: &str = "none";
pub const DOMAIN_REQUESTED: &str = "requested";
pub const DOMAIN_VERIFIED: &str = "verified";
pub const DOMAIN_REJECTED: &str = "rejected";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
/// default. Default `false` (deny) — fail closed.
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
    fn new(company_id: &str, tenant_id: Option<String>) -> Self {
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

// ---------------------------------------------------------------------------
// Request inputs
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct WhitelabelState {
    pub global_allowance: Arc<RwLock<GlobalAllowance>>,
    pub company_allowances: Arc<DashMap<String, CompanyAllowance>>,
    /// Admin-editable platform defaults record, seeded once at
    /// construction (see `GlobalDefaults::seed`). The stored record
    /// is the source of truth; the seed is never consulted after
    /// an admin saves defaults.
    pub defaults: Arc<RwLock<GlobalDefaults>>,
    /// Profiles keyed by `(company_id, tenant_id)` with `""` for the
    /// company default (no tenant).
    pub profiles: Arc<DashMap<(String, String), BrandingProfile>>,
    pub history: Arc<DashMap<Uuid, HistoryEntry>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl WhitelabelState {
    pub fn new() -> Self {
        Self {
            global_allowance: Arc::new(RwLock::new(GlobalAllowance {
                allow_whitelabel: false,
                updated_by: None,
                updated_at: Utc::now(),
            })),
            company_allowances: Arc::new(DashMap::new()),
            defaults: Arc::new(RwLock::new(GlobalDefaults::seed())),
            profiles: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn profile_key(company_id: &str, tenant_id: &Option<String>) -> (String, String) {
        (
            company_id.to_string(),
            tenant_id.clone().unwrap_or_default(),
        )
    }

    fn get_profile(&self, company_id: &str, tenant_id: &Option<String>) -> Option<BrandingProfile> {
        self.profiles
            .get(&Self::profile_key(company_id, tenant_id))
            .map(|entry| entry.value().clone())
    }

    fn company_mode(&self, company_id: &str) -> Option<AllowanceMode> {
        self.company_allowances
            .get(company_id)
            .map(|entry| entry.value().mode)
    }

    fn global_allow(&self) -> bool {
        self.global_allowance
            .read()
            .expect("allowance lock poisoned")
            .allow_whitelabel
    }

    fn global_defaults(&self) -> GlobalDefaults {
        self.defaults.read().expect("defaults lock poisoned").clone()
    }

    /// Effective allowance for a company: the per-company override
    /// wins unless it is `inherit`, in which case the global default
    /// applies.
    fn effective_allowance(&self, company_id: &str) -> bool {
        effective_allowance(self.global_allow(), self.company_mode(company_id))
    }

    /// Server-side branding resolution for one request: tenant
    /// override -> company default -> global defaults record, with
    /// the allowance enforced (not allowed means the global defaults
    /// record in full).
    fn resolve_for(&self, company_id: &str, tenant_id: &Option<String>) -> ResolvedBranding {
        let tenant = self.get_profile(company_id, tenant_id);
        let company = if tenant_id.is_some() {
            self.get_profile(company_id, &None)
        } else {
            None
        };
        let allowed = self.effective_allowance(company_id);
        resolve_branding(
            company_id,
            tenant_id,
            tenant.as_ref(),
            company.as_ref(),
            &self.global_defaults(),
            allowed,
        )
    }

    fn record_history(
        &self,
        company_id: &str,
        tenant_id: &Option<String>,
        actor: &str,
        action: &str,
        field: &str,
        old_value: Option<String>,
        new_value: Option<String>,
        allowed: bool,
    ) {
        let entry_id = Uuid::new_v4();
        self.history.insert(
            entry_id,
            HistoryEntry {
                id: entry_id,
                company_id: company_id.to_string(),
                tenant_id: tenant_id.clone(),
                actor: actor.to_string(),
                action: action.to_string(),
                field: field.to_string(),
                old_value,
                new_value,
                allowed,
                created_at: Utc::now(),
            },
        );
    }
}

impl Default for WhitelabelState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pure resolution functions (no I/O, no locks: unit-testable)
// ---------------------------------------------------------------------------

/// Effective allowance from the global default and the per-company
/// override mode. `inherit` (or no override) follows the global
/// default; `allowed` / `denied` override it.
pub fn effective_allowance(global_allow: bool, company_mode: Option<AllowanceMode>) -> bool {
    match company_mode {
        Some(AllowanceMode::Allowed) => true,
        Some(AllowanceMode::Denied) => false,
        Some(AllowanceMode::Inherit) | None => global_allow,
    }
}

/// Resolve branding from the three levels, field by field: tenant
/// override, then company default, then the global defaults record.
/// When `allowed` is false the resolved branding is the global
/// defaults record in full — tenant and company overrides are
/// ignored entirely, so Unified branding stays everywhere.
pub fn resolve_branding(
    company_id: &str,
    tenant_id: &Option<String>,
    tenant: Option<&BrandingProfile>,
    company: Option<&BrandingProfile>,
    defaults: &GlobalDefaults,
    allowed: bool,
) -> ResolvedBranding {
    // Pick one field from the first level that sets it.
    let pick = |field: fn(&BrandingProfile) -> &Option<String>| -> Option<String> {
        tenant
            .and_then(field)
            .or_else(|| company.and_then(field))
            .cloned()
    };
    let pick_pair = |field: fn(&BrandingProfile) -> &(Option<String>, Option<String>)| -> (Option<String>, Option<String>) {
        let tenant_pair = tenant.map(field);
        let company_pair = company.map(field);
        let target = tenant_pair
            .and_then(|(target, _)| target.clone())
            .or_else(|| company_pair.and_then(|(target, _)| target.clone()));
        let key = tenant_pair
            .and_then(|(_, key)| key.clone())
            .or_else(|| company_pair.and_then(|(_, key)| key.clone()));
        (target, key)
    };
    let (logo_target, logo_key) = pick_pair(|profile| {
        (&profile.logo_storage_target, &profile.logo_storage_key)
    });
    let (favicon_target, favicon_key) = pick_pair(|profile| {
        (&profile.favicon_storage_target, &profile.favicon_storage_key)
    });
    let (login_logo_target, login_logo_key) = pick_pair(|profile| {
        (
            &profile.login_logo_storage_target,
            &profile.login_logo_storage_key,
        )
    });
    let (login_bg_target, login_bg_key) = pick_pair(|profile| {
        (
            &profile.login_background_storage_target,
            &profile.login_background_storage_key,
        )
    });
    if !allowed {
        return ResolvedBranding {
            company_id: company_id.to_string(),
            tenant_id: tenant_id.clone(),
            allowed: false,
            enforced_platform_default: true,
            display_name: defaults.display_name.clone(),
            logo_storage_target: defaults.logo_storage_target.clone(),
            logo_storage_key: defaults.logo_storage_key.clone(),
            favicon_storage_target: defaults.favicon_storage_target.clone(),
            favicon_storage_key: defaults.favicon_storage_key.clone(),
            primary_color: defaults.primary_color.clone(),
            accent_color: defaults.accent_color.clone(),
            background_color: defaults.background_color.clone(),
            text_color: defaults.text_color.clone(),
            custom_domain: None,
            domain_status: DOMAIN_NONE.to_string(),
            hide_powered_by: false,
            sender_name: defaults.sender_name.clone(),
            sender_email: defaults.sender_email.clone(),
            login_logo_storage_target: defaults.logo_storage_target.clone(),
            login_logo_storage_key: defaults.logo_storage_key.clone(),
            login_background_storage_target: defaults.login_background_storage_target.clone(),
            login_background_storage_key: defaults.login_background_storage_key.clone(),
            login_tagline: defaults.login_tagline.clone(),
            powered_by_text: defaults.powered_by_text.clone(),
        };
    }
    let hide_powered_by = tenant
        .map(|profile| profile.hide_powered_by)
        .or_else(|| company.map(|profile| profile.hide_powered_by))
        .unwrap_or(false);
    let domain_status = tenant
        .and_then(|profile| profile.custom_domain.as_ref().map(|_| profile.domain_status.clone()))
        .or_else(|| {
            company.and_then(|profile| {
                profile
                    .custom_domain
                    .as_ref()
                    .map(|_| profile.domain_status.clone())
            })
        })
        .unwrap_or_else(|| DOMAIN_NONE.to_string());
    ResolvedBranding {
        company_id: company_id.to_string(),
        tenant_id: tenant_id.clone(),
        allowed: true,
        enforced_platform_default: false,
        display_name: pick(|profile| &profile.display_name)
            .unwrap_or_else(|| defaults.display_name.clone()),
        logo_storage_target: logo_target.or_else(|| defaults.logo_storage_target.clone()),
        logo_storage_key: logo_key.or_else(|| defaults.logo_storage_key.clone()),
        favicon_storage_target: favicon_target.or_else(|| defaults.favicon_storage_target.clone()),
        favicon_storage_key: favicon_key.or_else(|| defaults.favicon_storage_key.clone()),
        primary_color: pick(|profile| &profile.primary_color)
            .unwrap_or_else(|| defaults.primary_color.clone()),
        accent_color: pick(|profile| &profile.accent_color)
            .unwrap_or_else(|| defaults.accent_color.clone()),
        background_color: pick(|profile| &profile.background_color)
            .unwrap_or_else(|| defaults.background_color.clone()),
        text_color: pick(|profile| &profile.text_color)
            .unwrap_or_else(|| defaults.text_color.clone()),
        custom_domain: pick(|profile| &profile.custom_domain),
        domain_status,
        hide_powered_by,
        sender_name: pick(|profile| &profile.sender_name)
            .unwrap_or_else(|| defaults.sender_name.clone()),
        sender_email: pick(|profile| &profile.sender_email)
            .unwrap_or_else(|| defaults.sender_email.clone()),
        login_logo_storage_target: login_logo_target
            .or_else(|| defaults.logo_storage_target.clone()),
        login_logo_storage_key: login_logo_key.or_else(|| defaults.logo_storage_key.clone()),
        login_background_storage_target: login_bg_target
            .or_else(|| defaults.login_background_storage_target.clone()),
        login_background_storage_key: login_bg_key
            .or_else(|| defaults.login_background_storage_key.clone()),
        login_tagline: pick(|profile| &profile.login_tagline)
            .unwrap_or_else(|| defaults.login_tagline.clone()),
        powered_by_text: defaults.powered_by_text.clone(),
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn valid_hex_color(color: &str) -> bool {
    let digits = color.strip_prefix('#').unwrap_or("");
    (digits.len() == 3 || digits.len() == 6)
        && digits.chars().all(|c| c.is_ascii_hexdigit())
}

fn require_hex_color(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if !valid_hex_color(trimmed) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a hex colour such as \"#2563EB\" or \"#FFF\""
        )));
    }
    Ok(trimmed.to_uppercase())
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn require_optional_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    require_text(value, field, max)
}

/// A custom domain is a hostname: dot-separated labels of letters,
/// digits, and hyphens, never leading or trailing with a hyphen or
/// a dot, at least one dot, at most 253 chars. No scheme, port, or
/// path is allowed. Same rule as the portals module.
fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > MAX_DOMAIN_LEN {
        return false;
    }
    if domain.starts_with('.')
        || domain.ends_with('.')
        || domain.starts_with('-')
        || domain.ends_with('-')
        || !domain.contains('.')
    {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

fn require_domain(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_lowercase();
    if !valid_domain(&trimmed) {
        return Err(UnifiedError::bad_request(format!(
            "custom_domain \"{value}\" is not a valid hostname"
        )));
    }
    Ok(trimmed)
}

fn require_email(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_string();
    trimmed.parse::<Address>().map_err(|_| {
        UnifiedError::bad_request(format!("{field} is not a valid email address"))
    })?;
    Ok(trimmed)
}

/// A storage reference (target + key) must be given together, like
/// the portals module logo: the bytes live in the storage module
/// and this module only stores the reference.
fn require_storage_pair(
    target: &Option<Option<String>>,
    key: &Option<Option<String>>,
    field: &str,
) -> UnifiedResult<(Option<Option<String>>, Option<Option<String>>)> {
    let clean = |value: &Option<Option<String>>| -> UnifiedResult<Option<Option<String>>> {
        match value {
            None => Ok(None),
            Some(None) => Ok(Some(None)),
            Some(Some(raw)) => Ok(Some(Some(require_text(
                raw,
                field,
                MAX_STORAGE_REF_LEN,
            )?))),
        }
    };
    let target = clean(target)?;
    let key = clean(key)?;
    // A storage pair is set or cleared as a unit: touching one side
    // without the other (including clearing only one side, which
    // would leave a dangling reference) is rejected. Callers that
    // change one side must resend the other side in the same
    // request, even when it is already stored.
    let target_touched = target.is_some();
    let key_touched = key.is_some();
    if target_touched != key_touched {
        return Err(UnifiedError::bad_request(format!(
            "{field} storage target and key must be given together"
        )));
    }
    Ok((target, key))
}

fn apply_storage_pair(
    current_target: &mut Option<String>,
    current_key: &mut Option<String>,
    target: Option<Option<String>>,
    key: Option<Option<String>>,
) {
    if let Some(target) = target {
        *current_target = target;
    }
    if let Some(key) = key {
        *current_key = key;
    }
}

// ---------------------------------------------------------------------------
// Caller identity + capability checks (portals / ai module shape)
// ---------------------------------------------------------------------------

struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

fn caller_company(req: &HttpRequest) -> UnifiedResult<String> {
    let company = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Company-Id header"))?;
    if company.chars().count() > MAX_COMPANY_ID_LEN {
        return Err(UnifiedError::bad_request(
            "X-Company-Id must be at most 128 characters",
        ));
    }
    Ok(company)
}

/// Optional tenant inside the company, from `X-Tenant-Id`. Absent
/// means the company default profile (no tenant override).
fn caller_tenant(req: &HttpRequest) -> UnifiedResult<Option<String>> {
    let tenant = req
        .headers()
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(tenant) = &tenant {
        if tenant.chars().count() > MAX_TENANT_ID_LEN {
            return Err(UnifiedError::bad_request(
                "X-Tenant-Id must be at most 128 characters",
            ));
        }
    }
    Ok(tenant)
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_company_id(raw: &str) -> UnifiedResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_COMPANY_ID_LEN {
        return Err(UnifiedError::bad_request(
            "company_id must not be empty and must be at most 128 characters",
        ));
    }
    Ok(trimmed.to_string())
}

fn page_of<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> Value {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items: Vec<T> = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    PageResult::new(page_items, total, &pagination).to_json()
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    success_response(data, 200)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    error_response(&error, debug)
}

// ---------------------------------------------------------------------------
// Admin handlers: allowance
// ---------------------------------------------------------------------------

/// List the global allowance plus every per-company override
/// (paginated, filter by `company_id` or `mode`, search by
/// company id). Admin only.
async fn get_admin_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let mode_filter = params.filters.get("mode").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut overrides: Vec<CompanyAllowance> = state
            .company_allowances
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                if let Some(company_id) = &company_filter {
                    if record.company_id != *company_id {
                        return false;
                    }
                }
                if let Some(mode) = &mode_filter {
                    if record.mode.as_str() != mode.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() && !record.company_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        overrides.sort_by(|a, b| a.company_id.cmp(&b.company_id));
        let page = page_of(overrides, &params);
        let global = state.global_allowance.read().expect("allowance lock poisoned");
        Ok(json!({
            "global": {
                "allow_whitelabel": global.allow_whitelabel,
                "updated_by": global.updated_by,
                "updated_at": global.updated_at,
            },
            "overrides": page,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Set the global default allowance. Admin only. Emits
/// `whitelabel.allowance.changed` and records history.
async fn put_global_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    body: web::Json<PutGlobalAllowanceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let input = body.into_inner();
        let mut global = state
            .global_allowance
            .write()
            .expect("allowance lock poisoned");
        let old = global.allow_whitelabel;
        global.allow_whitelabel = input.allow_whitelabel;
        global.updated_by = Some(caller.actor.clone());
        global.updated_at = Utc::now();
        let updated = global.clone();
        drop(global);
        state.record_history(
            "global",
            &None,
            &caller.actor,
            "allowance.global_changed",
            "allow_whitelabel",
            Some(old.to_string()),
            Some(input.allow_whitelabel.to_string()),
            true,
        );
        ctx.hooks.emit(
            EVENT_ALLOWANCE_CHANGED,
            json!({
                "scope": "global",
                "allow_whitelabel": input.allow_whitelabel,
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "allow_whitelabel": updated.allow_whitelabel,
            "updated_by": updated.updated_by,
            "updated_at": updated.updated_at,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Effective allowance for one company (global default resolved
/// through the per-company override). Admin only.
async fn get_company_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    company_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = parse_company_id(&company_id.into_inner())?;
        let mode = state.company_mode(&company_id);
        Ok(json!({
            "company_id": company_id,
            "mode": mode.map(|mode| mode.as_str()),
            "allowed": state.effective_allowance(&company_id),
            "global_allow_whitelabel": state.global_allow(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Set the per-company allowance override (`allowed` / `denied` /
/// `inherit`). Admin only. Emits `whitelabel.allowance.changed`
/// and records history.
async fn put_company_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    company_id: web::Path<String>,
    body: web::Json<PutCompanyAllowanceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = parse_company_id(&company_id.into_inner())?;
        let mode = AllowanceMode::parse(body.into_inner().mode.trim())?;
        let old = state.company_mode(&company_id).map(|mode| mode.as_str().to_string());
        state.company_allowances.insert(
            company_id.clone(),
            CompanyAllowance {
                company_id: company_id.clone(),
                mode,
                updated_by: Some(caller.actor.clone()),
                updated_at: Utc::now(),
            },
        );
        state.record_history(
            &company_id,
            &None,
            &caller.actor,
            "allowance.company_changed",
            "mode",
            old,
            Some(mode.as_str().to_string()),
            true,
        );
        ctx.hooks.emit(
            EVENT_ALLOWANCE_CHANGED,
            json!({
                "scope": "company",
                "company_id": company_id,
                "mode": mode.as_str(),
                "allowed": state.effective_allowance(&company_id),
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "company_id": company_id,
            "mode": mode.as_str(),
            "allowed": state.effective_allowance(&company_id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Admin handlers: global defaults record
// ---------------------------------------------------------------------------

/// Read the admin-editable global defaults record. Admin only.
async fn get_admin_defaults(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GlobalDefaults> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        Ok(state.global_defaults())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Partial update of the admin-editable global defaults record.
/// Admin only. Every field is validated the same way tenant fields
/// are (hex colours, email address, storage pairs together). Emits
/// `whitelabel.defaults.changed` and records history.
async fn put_admin_defaults(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    body: web::Json<PutDefaultsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let input = body.into_inner();
        // Validate everything before touching the stored record.
        let display_name = match &input.display_name {
            Some(raw) => Some(require_text(raw, "display_name", MAX_NAME_LEN)?),
            None => None,
        };
        let primary_color = match &input.primary_color {
            Some(raw) => Some(require_hex_color(raw, "primary_color")?),
            None => None,
        };
        let accent_color = match &input.accent_color {
            Some(raw) => Some(require_hex_color(raw, "accent_color")?),
            None => None,
        };
        let background_color = match &input.background_color {
            Some(raw) => Some(require_hex_color(raw, "background_color")?),
            None => None,
        };
        let text_color = match &input.text_color {
            Some(raw) => Some(require_hex_color(raw, "text_color")?),
            None => None,
        };
        let sender_name = match &input.sender_name {
            Some(raw) => Some(require_text(raw, "sender_name", MAX_NAME_LEN)?),
            None => None,
        };
        let sender_email = match &input.sender_email {
            Some(raw) if raw.trim().is_empty() => Some(String::new()),
            Some(raw) => Some(require_email(raw, "sender_email")?),
            None => None,
        };
        let login_tagline = match &input.login_tagline {
            Some(raw) => Some(require_text(raw, "login_tagline", MAX_TAGLINE_LEN)?),
            None => None,
        };
        let powered_by_text = match &input.powered_by_text {
            Some(raw) => Some(require_text(raw, "powered_by_text", MAX_NAME_LEN)?),
            None => None,
        };
        let (logo_target, logo_key) = require_storage_pair(
            &input.logo_storage_target,
            &input.logo_storage_key,
            "logo",
        )?;
        let (favicon_target, favicon_key) = require_storage_pair(
            &input.favicon_storage_target,
            &input.favicon_storage_key,
            "favicon",
        )?;
        let (login_bg_target, login_bg_key) = require_storage_pair(
            &input.login_background_storage_target,
            &input.login_background_storage_key,
            "login_background",
        )?;
        let mut defaults = state.defaults.write().expect("defaults lock poisoned");
        let mut changed: Vec<String> = Vec::new();
        if let Some(value) = display_name {
            defaults.display_name = value;
            changed.push("display_name".to_string());
        }
        if let Some(value) = primary_color {
            defaults.primary_color = value;
            changed.push("primary_color".to_string());
        }
        if let Some(value) = accent_color {
            defaults.accent_color = value;
            changed.push("accent_color".to_string());
        }
        if let Some(value) = background_color {
            defaults.background_color = value;
            changed.push("background_color".to_string());
        }
        if let Some(value) = text_color {
            defaults.text_color = value;
            changed.push("text_color".to_string());
        }
        if let Some(value) = sender_name {
            defaults.sender_name = value;
            changed.push("sender_name".to_string());
        }
        if let Some(value) = sender_email {
            defaults.sender_email = value;
            changed.push("sender_email".to_string());
        }
        if let Some(value) = login_tagline {
            defaults.login_tagline = value;
            changed.push("login_tagline".to_string());
        }
        if let Some(value) = powered_by_text {
            defaults.powered_by_text = value;
            changed.push("powered_by_text".to_string());
        }
        apply_storage_pair(
            &mut defaults.logo_storage_target,
            &mut defaults.logo_storage_key,
            logo_target,
            logo_key,
        );
        if input.logo_storage_target.is_some() || input.logo_storage_key.is_some() {
            changed.push("logo".to_string());
        }
        apply_storage_pair(
            &mut defaults.favicon_storage_target,
            &mut defaults.favicon_storage_key,
            favicon_target,
            favicon_key,
        );
        if input.favicon_storage_target.is_some() || input.favicon_storage_key.is_some() {
            changed.push("favicon".to_string());
        }
        apply_storage_pair(
            &mut defaults.login_background_storage_target,
            &mut defaults.login_background_storage_key,
            login_bg_target,
            login_bg_key,
        );
        if input.login_background_storage_target.is_some()
            || input.login_background_storage_key.is_some()
        {
            changed.push("login_background".to_string());
        }
        let updated = defaults.clone();
        drop(defaults);
        state.record_history(
            "global",
            &None,
            &caller.actor,
            "defaults.changed",
            "*",
            None,
            Some(changed.join(",")),
            true,
        );
        ctx.hooks.emit(
            EVENT_DEFAULTS_CHANGED,
            json!({ "actor": caller.actor, "fields": changed }),
        );
        Ok(json!(updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Admin handlers: profiles + history
// ---------------------------------------------------------------------------

/// List all branding profiles (admin). Paginated, filter by
/// `company_id`, `tenant_id`, or `domain_status`, search by company
/// id, tenant id, display name, or custom domain.
async fn list_admin_profiles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let tenant_filter = params.filters.get("tenant_id").cloned();
        let status_filter = params.filters.get("domain_status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut profiles: Vec<BrandingProfile> = state
            .profiles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|profile| {
                if let Some(company_id) = &company_filter {
                    if profile.company_id != *company_id {
                        return false;
                    }
                }
                if let Some(tenant_id) = &tenant_filter {
                    if profile.tenant_id.as_deref() != Some(tenant_id.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if profile.domain_status != *status {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        profile.company_id,
                        profile.tenant_id.clone().unwrap_or_default(),
                        profile.display_name.clone().unwrap_or_default(),
                        profile.custom_domain.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        profiles.sort_by(|a, b| {
            a.company_id
                .cmp(&b.company_id)
                .then_with(|| a.tenant_id.cmp(&b.tenant_id))
        });
        Ok(page_of(profiles, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Get one company's branding profile (admin). `?tenant_id=` selects
/// the tenant override; without it the company default is returned.
async fn get_admin_profile(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    company_id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BrandingProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = parse_company_id(&company_id.into_inner())?;
        let tenant_id = query.filters.get("tenant_id").cloned();
        state.get_profile(&company_id, &tenant_id).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No branding profile for company \"{company_id}\""
            ))
        })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the full branding history (admin). Paginated, filter by
/// `company_id`, `action`, or `allowed`, search by actor or field.
async fn list_admin_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let params = query.into_inner();
        Ok(page_of(filter_history(&state, &params, None), &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared history filtering for the admin and tenant history lists.
/// `company_scope` restricts to one company (tenant list); `None`
/// means all companies (admin list).
fn filter_history(
    state: &WhitelabelState,
    params: &PaginationParams,
    company_scope: Option<&str>,
) -> Vec<HistoryEntry> {
    let company_filter = params.filters.get("company_id").cloned();
    let action_filter = params.filters.get("action").cloned();
    let allowed_filter = params.filters.get("allowed").cloned();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut entries: Vec<HistoryEntry> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            if let Some(scope) = company_scope {
                if entry.company_id != scope {
                    return false;
                }
            }
            if let Some(company_id) = &company_filter {
                if entry.company_id != *company_id {
                    return false;
                }
            }
            if let Some(action) = &action_filter {
                if entry.action != *action {
                    return false;
                }
            }
            if let Some(allowed) = &allowed_filter {
                let wanted = allowed == "true";
                if entry.allowed != wanted {
                    return false;
                }
            }
            if !search.is_empty() {
                let haystack = format!("{} {} {}", entry.actor, entry.field, entry.action)
                    .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            true
        })
        .collect();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    entries
}

// ---------------------------------------------------------------------------
// Tenant handlers
// ---------------------------------------------------------------------------

/// The caller's own effective allowance. Tenant read capability.
async fn get_my_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = caller_company(&req)?;
        let mode = state.company_mode(&company_id);
        Ok(json!({
            "company_id": company_id,
            "mode": mode.map(|mode| mode.as_str()),
            "allowed": state.effective_allowance(&company_id),
            "global_allow_whitelabel": state.global_allow(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The caller's own server-resolved branding. Enforcement happens
/// here, on the server: when the tenant is not allowed, the resolved
/// branding is the global defaults record in full. Tenant read
/// capability.
async fn get_my_branding(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ResolvedBranding> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = caller_company(&req)?;
        let tenant_id = caller_tenant(&req)?;
        Ok(state.resolve_for(&company_id, &tenant_id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Save the caller's own branding profile. When white labelling is
/// NOT allowed for the tenant, the write is rejected with 403 and a
/// `whitelabel.branding.denied` audit event is emitted plus a history
/// entry — the attempt itself is part of the audit trail. A new
/// custom domain is stored as `requested` and a
/// `whitelabel.domain.requested` event is emitted for the domains
/// module; only a VERIFIED domain is ever attached as the resolved
/// custom domain (the resolver reports the stored status, and
/// renderers must only use a domain whose status is `verified`).
async fn put_my_branding(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    body: web::Json<PutBrandingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BrandingProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BRANDING_MANAGE)?;
        let company_id = caller_company(&req)?;
        let tenant_id = caller_tenant(&req)?;
        let input = body.into_inner();
        // Enforcement first: no custom branding without permission.
        if !state.effective_allowance(&company_id) {
            let fields = branding_input_fields(&input);
            state.record_history(
                &company_id,
                &tenant_id,
                &caller.actor,
                "branding.denied",
                "*",
                None,
                Some(fields.join(",")),
                false,
            );
            ctx.hooks.emit(
                EVENT_BRANDING_DENIED,
                json!({
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                    "actor": caller.actor,
                    "fields": fields,
                }),
            );
            return Err(UnifiedError::forbidden(
                "White labelling is not allowed for this company: Unified branding applies",
            ));
        }
        // Validate everything before writing anything.
        let display_name = match &input.display_name {
            Some(raw) => Some(require_text(raw, "display_name", MAX_NAME_LEN)?),
            None => None,
        };
        let primary_color = match &input.primary_color {
            Some(raw) => Some(require_hex_color(raw, "primary_color")?),
            None => None,
        };
        let accent_color = match &input.accent_color {
            Some(raw) => Some(require_hex_color(raw, "accent_color")?),
            None => None,
        };
        let background_color = match &input.background_color {
            Some(raw) => Some(require_hex_color(raw, "background_color")?),
            None => None,
        };
        let text_color = match &input.text_color {
            Some(raw) => Some(require_hex_color(raw, "text_color")?),
            None => None,
        };
        let custom_domain = match &input.custom_domain {
            Some(Some(raw)) => Some(Some(require_domain(raw)?)),
            Some(None) => Some(None),
            None => None,
        };
        let sender_name = match &input.sender_name {
            Some(raw) => Some(require_text(raw, "sender_name", MAX_NAME_LEN)?),
            None => None,
        };
        let sender_email = match &input.sender_email {
            Some(raw) => Some(require_email(raw, "sender_email")?),
            None => None,
        };
        let login_tagline = match &input.login_tagline {
            Some(raw) => Some(require_text(raw, "login_tagline", MAX_TAGLINE_LEN)?),
            None => None,
        };
        let (logo_target, logo_key) =
            require_storage_pair(&input.logo_storage_target, &input.logo_storage_key, "logo")?;
        let (favicon_target, favicon_key) = require_storage_pair(
            &input.favicon_storage_target,
            &input.favicon_storage_key,
            "favicon",
        )?;
        let (login_logo_target, login_logo_key) = require_storage_pair(
            &input.login_logo_storage_target,
            &input.login_logo_storage_key,
            "login_logo",
        )?;
        let (login_bg_target, login_bg_key) = require_storage_pair(
            &input.login_background_storage_target,
            &input.login_background_storage_key,
            "login_background",
        )?;
        let key = WhitelabelState::profile_key(&company_id, &tenant_id);
        let mut profile = state
            .profiles
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| BrandingProfile::new(&company_id, tenant_id.clone()));
        let mut changed: Vec<String> = Vec::new();
        if let Some(value) = display_name {
            profile.display_name = Some(value);
            changed.push("display_name".to_string());
        }
        if let Some(value) = primary_color {
            profile.primary_color = Some(value);
            changed.push("primary_color".to_string());
        }
        if let Some(value) = accent_color {
            profile.accent_color = Some(value);
            changed.push("accent_color".to_string());
        }
        if let Some(value) = background_color {
            profile.background_color = Some(value);
            changed.push("background_color".to_string());
        }
        if let Some(value) = text_color {
            profile.text_color = Some(value);
            changed.push("text_color".to_string());
        }
        if let Some(value) = sender_name {
            profile.sender_name = Some(value);
            changed.push("sender_name".to_string());
        }
        if let Some(value) = sender_email {
            profile.sender_email = Some(value);
            changed.push("sender_email".to_string());
        }
        if let Some(value) = login_tagline {
            profile.login_tagline = Some(value);
            changed.push("login_tagline".to_string());
        }
        if let Some(value) = input.hide_powered_by {
            profile.hide_powered_by = value;
            changed.push("hide_powered_by".to_string());
        }
        apply_storage_pair(
            &mut profile.logo_storage_target,
            &mut profile.logo_storage_key,
            logo_target,
            logo_key,
        );
        if input.logo_storage_target.is_some() || input.logo_storage_key.is_some() {
            changed.push("logo".to_string());
        }
        apply_storage_pair(
            &mut profile.favicon_storage_target,
            &mut profile.favicon_storage_key,
            favicon_target,
            favicon_key,
        );
        if input.favicon_storage_target.is_some() || input.favicon_storage_key.is_some() {
            changed.push("favicon".to_string());
        }
        apply_storage_pair(
            &mut profile.login_logo_storage_target,
            &mut profile.login_logo_storage_key,
            login_logo_target,
            login_logo_key,
        );
        if input.login_logo_storage_target.is_some() || input.login_logo_storage_key.is_some() {
            changed.push("login_logo".to_string());
        }
        apply_storage_pair(
            &mut profile.login_background_storage_target,
            &mut profile.login_background_storage_key,
            login_bg_target,
            login_bg_key,
        );
        if input.login_background_storage_target.is_some()
            || input.login_background_storage_key.is_some()
        {
            changed.push("login_background".to_string());
        }
        // Custom domain: a changed domain restarts verification.
        // Only a VERIFIED domain may be attached; until the domains
        // module verifies it, the status stays `requested`.
        let mut domain_changed = false;
        if let Some(value) = custom_domain {
            if profile.custom_domain != value {
                domain_changed = value.is_some();
                profile.custom_domain = value;
                profile.domain_status = if domain_changed {
                    DOMAIN_REQUESTED.to_string()
                } else {
                    DOMAIN_NONE.to_string()
                };
                changed.push("custom_domain".to_string());
            }
        }
        profile.updated_at = Utc::now();
        state.profiles.insert(key, profile.clone());
        state.record_history(
            &company_id,
            &tenant_id,
            &caller.actor,
            "branding.updated",
            "*",
            None,
            Some(changed.join(",")),
            true,
        );
        ctx.hooks.emit(
            EVENT_BRANDING_CHANGED,
            json!({
                "company_id": company_id,
                "tenant_id": tenant_id,
                "actor": caller.actor,
                "fields": changed,
            }),
        );
        if domain_changed {
            let domain = profile.custom_domain.clone().unwrap_or_default();
            ctx.hooks.emit(
                EVENT_DOMAIN_REQUESTED,
                json!({
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                    "domain": domain,
                }),
            );
            state.record_history(
                &company_id,
                &tenant_id,
                &caller.actor,
                "domain.requested",
                "custom_domain",
                None,
                Some(domain),
                true,
            );
        }
        if profile.sender_email.is_some() || profile.sender_name.is_some() {
            ctx.hooks.emit(
                EVENT_SENDER_REQUESTED,
                json!({
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                    "sender_name": profile.sender_name,
                    "sender_email": profile.sender_email,
                }),
            );
        }
        Ok(profile)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Field names present in a branding write, for the denied-attempt
/// audit record (no values: the attempt is logged, not the content).
fn branding_input_fields(input: &PutBrandingInput) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    if input.display_name.is_some() {
        fields.push("display_name".to_string());
    }
    if input.logo_storage_target.is_some() || input.logo_storage_key.is_some() {
        fields.push("logo".to_string());
    }
    if input.favicon_storage_target.is_some() || input.favicon_storage_key.is_some() {
        fields.push("favicon".to_string());
    }
    if input.primary_color.is_some() {
        fields.push("primary_color".to_string());
    }
    if input.accent_color.is_some() {
        fields.push("accent_color".to_string());
    }
    if input.background_color.is_some() {
        fields.push("background_color".to_string());
    }
    if input.text_color.is_some() {
        fields.push("text_color".to_string());
    }
    if input.custom_domain.is_some() {
        fields.push("custom_domain".to_string());
    }
    if input.hide_powered_by.is_some() {
        fields.push("hide_powered_by".to_string());
    }
    if input.sender_name.is_some() {
        fields.push("sender_name".to_string());
    }
    if input.sender_email.is_some() {
        fields.push("sender_email".to_string());
    }
    if input.login_logo_storage_target.is_some() || input.login_logo_storage_key.is_some() {
        fields.push("login_logo".to_string());
    }
    if input.login_background_storage_target.is_some()
        || input.login_background_storage_key.is_some()
    {
        fields.push("login_background".to_string());
    }
    if input.login_tagline.is_some() {
        fields.push("login_tagline".to_string());
    }
    fields
}

/// Reset the caller's own branding profile to the platform defaults
/// (deletes the profile). Removing custom branding is always safe,
/// so this is allowed even when white labelling is not permitted:
/// it can only move the tenant towards the default branding.
/// Audit-logged and emits `whitelabel.branding.reset`.
async fn delete_my_branding(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BRANDING_MANAGE)?;
        let company_id = caller_company(&req)?;
        let tenant_id = caller_tenant(&req)?;
        let key = WhitelabelState::profile_key(&company_id, &tenant_id);
        match state.profiles.remove(&key) {
            Some(_) => {
                state.record_history(
                    &company_id,
                    &tenant_id,
                    &caller.actor,
                    "branding.reset",
                    "*",
                    None,
                    Some("reset to platform defaults".to_string()),
                    true,
                );
                ctx.hooks.emit(
                    EVENT_BRANDING_RESET,
                    json!({
                        "company_id": company_id,
                        "tenant_id": tenant_id,
                        "actor": caller.actor,
                    }),
                );
                Ok(json!({
                    "deleted": true,
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                }))
            }
            None => Err(UnifiedError::not_found(format!(
                "No branding profile for company \"{company_id}\""
            ))),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The caller's own branding history (paginated, filter by `action`
/// or `allowed`, search by actor or field). Scoped to the caller's
/// company: a tenant never sees another company's history.
async fn list_my_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = caller_company(&req)?;
        let params = query.into_inner();
        Ok(page_of(
            filter_history(&state, &params, Some(&company_id)),
            &params,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public handler (API Manager ApiAccessRule gated, like portals)
// ---------------------------------------------------------------------------

/// Server-resolved branding for one company's login / portal page
/// render. No capability check on purpose: the API Manager denies
/// the call unless an administrator created the matching
/// `ApiAccessRule` for `GET /api/v1/whitelabel/public/*` (default
/// deny). Only resolved branding is returned — never allowance
/// settings, history, or another tenant's data — and a tenant that
/// is not allowed resolves to the platform defaults.
async fn get_public_branding(
    state: web::Data<WhitelabelState>,
    company_id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(company_id) => company_id,
        Err(error) => return error_response(&error, false),
    };
    let tenant_id = query.filters.get("tenant_id").cloned();
    ok(state.resolve_for(&company_id, &tenant_id))
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Answer `whitelabel.branding.resolve` with the server-resolved
/// branding for `{"company_id", "tenant_id"?}`. This is the hook
/// theme, portals, mail, and login rendering must call instead of
/// reading branding themselves or trusting client input.
fn on_branding_resolve(state: &WhitelabelState, ctx: &mut HookContext) {
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if company_id.trim().is_empty() {
        ctx.add_result(json!({
            "error": "whitelabel.branding.resolve requires 'company_id'",
        }));
        return;
    }
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let resolved = state.resolve_for(&company_id, &tenant_id);
    match serde_json::to_value(&resolved) {
        Ok(value) => ctx.add_result(value),
        Err(error) => ctx.add_result(json!({ "error": error.to_string() })),
    }
}

/// Answer `whitelabel.allowance.check` with
/// `{"company_id", "allowed"}` for `{"company_id"}`.
fn on_allowance_check(state: &WhitelabelState, ctx: &mut HookContext) {
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if company_id.trim().is_empty() {
        ctx.add_result(json!({
            "error": "whitelabel.allowance.check requires 'company_id'",
        }));
        return;
    }
    ctx.add_result(json!({
        "company_id": company_id,
        "allowed": state.effective_allowance(&company_id),
    }));
}

/// Apply a `domain.verify` outcome (emitted by the domains module)
/// to the matching branding profile: `verified` when `dns_ok` is
/// true, `rejected` when it is false. Only a verified domain is
/// ever reported as attached by the resolver. The domains module
/// owns verification; this module only records the outcome.
fn on_domain_verify(state: &WhitelabelState, ctx: &mut HookContext) {
    let domain = ctx
        .get("domain")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_lowercase();
    if domain.is_empty() {
        return;
    }
    let dns_ok = ctx
        .get("dns_ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let new_status = if dns_ok { DOMAIN_VERIFIED } else { DOMAIN_REJECTED };
    let mut matched: Vec<(String, Option<String>)> = Vec::new();
    for mut entry in state.profiles.iter_mut() {
        let profile = entry.value_mut();
        if profile
            .custom_domain
            .as_deref()
            .map(|custom| custom.to_lowercase() == domain)
            .unwrap_or(false)
            && profile.domain_status != new_status
        {
            profile.domain_status = new_status.to_string();
            profile.updated_at = Utc::now();
            matched.push((profile.company_id.clone(), profile.tenant_id.clone()));
        }
    }
    for (company_id, tenant_id) in matched {
        let event = if dns_ok {
            EVENT_DOMAIN_VERIFIED
        } else {
            EVENT_DOMAIN_REJECTED
        };
        let action = if dns_ok {
            "domain.verified"
        } else {
            "domain.rejected"
        };
        state.record_history(
            &company_id,
            &tenant_id,
            "system:domains",
            action,
            "domain_status",
            Some(DOMAIN_REQUESTED.to_string()),
            Some(new_status.to_string()),
            true,
        );
        state.emit(
            event,
            json!({
                "company_id": company_id,
                "tenant_id": tenant_id,
                "domain": domain,
                "domain_status": new_status,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct WhitelabelModule {
    state: WhitelabelState,
}

impl WhitelabelModule {
    pub fn new() -> Self {
        Self {
            state: WhitelabelState::new(),
        }
    }
}

impl Default for WhitelabelModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WhitelabelModule {
    fn id(&self) -> &'static str {
        "whitelabel"
    }

    fn name(&self) -> &'static str {
        "White Labelling"
    }

    fn description(&self) -> &'static str {
        "Admin-gated white labelling: global and per-company allowance, server-side branding resolution (tenant override, company default, global defaults), custom domains, email sender and login branding, with a full audit history."
    }

    fn requires(&self) -> &[&'static str] {
        &["theme", "portals", "mail", "domains"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            HOOK_BRANDING_RESOLVE,
            "whitelabel",
            Arc::new(move |ctx: &mut HookContext| on_branding_resolve(&state, ctx)),
        );
        let state = self.state.clone();
        bus.register(
            HOOK_ALLOWANCE_CHECK,
            "whitelabel",
            Arc::new(move |ctx: &mut HookContext| on_allowance_check(&state, ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "domain.verify",
            "whitelabel",
            Arc::new(move |ctx: &mut HookContext| on_domain_verify(&state, ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/whitelabel")
                .app_data(state)
                .route("/admin/allowance", web::get().to(get_admin_allowance))
                .route("/admin/allowance", web::put().to(put_global_allowance))
                .route(
                    "/admin/allowance/{company_id}",
                    web::get().to(get_company_allowance),
                )
                .route(
                    "/admin/allowance/{company_id}",
                    web::put().to(put_company_allowance),
                )
                .route("/admin/defaults", web::get().to(get_admin_defaults))
                .route("/admin/defaults", web::put().to(put_admin_defaults))
                .route("/admin/profiles", web::get().to(list_admin_profiles))
                .route(
                    "/admin/profiles/{company_id}",
                    web::get().to(get_admin_profile),
                )
                .route("/admin/history", web::get().to(list_admin_history))
                .route("/allowance", web::get().to(get_my_allowance))
                .route("/branding", web::get().to(get_my_branding))
                .route("/branding", web::put().to(put_my_branding))
                .route("/branding", web::delete().to(delete_my_branding))
                .route("/branding/history", web::get().to(list_my_history))
                .route(
                    "/public/branding/{company_id}",
                    web::get().to(get_public_branding),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/allowance", owner: "whitelabel", description: "List the global allowance plus per-company overrides (admin; paginated, filter by company_id, mode)." },
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/admin/allowance", owner: "whitelabel", description: "Set the global default allowance (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/allowance/{company_id}", owner: "whitelabel", description: "Effective allowance for one company (admin)." },
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/admin/allowance/{company_id}", owner: "whitelabel", description: "Set the per-company allowance override: allowed, denied, or inherit (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/defaults", owner: "whitelabel", description: "Read the admin-editable global defaults record (admin)." },
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/admin/defaults", owner: "whitelabel", description: "Update the admin-editable global defaults record (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/profiles", owner: "whitelabel", description: "List all branding profiles (admin; paginated, filter by company_id, tenant_id, domain_status)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/profiles/{company_id}", owner: "whitelabel", description: "Get one company's branding profile (admin; ?tenant_id= selects the tenant override)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/history", owner: "whitelabel", description: "List the full branding history (admin; paginated, filter by company_id, action, allowed)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/allowance", owner: "whitelabel", description: "The caller's own effective allowance." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/branding", owner: "whitelabel", description: "The caller's own server-resolved branding (platform defaults when not allowed)." },
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/branding", owner: "whitelabel", description: "Save the caller's own branding profile (rejected with 403 + audit event when not allowed)." },
            RouteInfo { method: "DELETE", path: "/api/v1/whitelabel/branding", owner: "whitelabel", description: "Reset the caller's own branding to the platform defaults." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/branding/history", owner: "whitelabel", description: "The caller's own branding history, scoped to the caller's company (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/public/branding/{company_id}", owner: "whitelabel", description: "PUBLIC: server-resolved branding for a login/portal render; gated by the API Manager ApiAccessRule (default deny)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WhitelabelModule::new()));
}
