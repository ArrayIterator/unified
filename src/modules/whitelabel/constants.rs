// Whitelabel module - event names, permissions, and limits.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

// Constants

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
