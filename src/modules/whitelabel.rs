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
// resolved branding is the global defaults record in full - tenant
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
// - `whitelabel.allowance.changed` - global or per-company allowance
//   changed: `{"scope", "company_id"?, "mode"/"allow_whitelabel",
//   "actor"}`.
// - `whitelabel.defaults.changed` - global defaults record changed:
//   `{"actor", "fields"}`.
// - `whitelabel.branding.changed` - tenant branding saved:
//   `{"company_id", "tenant_id"?, "actor", "fields"}`.
// - `whitelabel.branding.denied` - a tenant without permission tried
//   to set custom branding (rejected with 403):
//   `{"company_id", "tenant_id"?, "actor", "fields"}`.
// - `whitelabel.branding.reset` - tenant branding reset to defaults.
// - `whitelabel.domain.verified` / `whitelabel.domain.rejected` -
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
// like the portals and ai modules. Tenant endpoints are scoped to
// the verified caller's company (the `X-Company-Id` header is
// never trusted); `X-Tenant-Id` is optional and selects the tenant
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
// records (default deny - an administrator must create the matching
// rule for `GET /api/v1/whitelabel/public/*`). Without the rule the
// API Manager denies the call. It only ever returns resolved
// branding (defaults when the tenant is not allowed); it never
// exposes allowance settings, history, or other tenants' data.

pub mod constants;
pub mod types;
pub mod state;
pub mod service;
mod validation;
mod helpers;
mod handlers_admin;
mod handlers_branding;
mod events;
pub mod routes;

pub use self::constants::PERM_MANAGE;
pub use self::constants::PERM_VIEW;
pub use self::constants::PERM_BRANDING_MANAGE;
pub use self::constants::EVENT_ALLOWANCE_CHANGED;
pub use self::constants::EVENT_DEFAULTS_CHANGED;
pub use self::constants::EVENT_BRANDING_CHANGED;
pub use self::constants::EVENT_BRANDING_DENIED;
pub use self::constants::EVENT_BRANDING_RESET;
pub use self::constants::EVENT_DOMAIN_REQUESTED;
pub use self::constants::EVENT_DOMAIN_VERIFIED;
pub use self::constants::EVENT_DOMAIN_REJECTED;
pub use self::constants::EVENT_SENDER_REQUESTED;
pub use self::constants::HOOK_BRANDING_RESOLVE;
pub use self::constants::HOOK_ALLOWANCE_CHECK;
pub use self::constants::MAX_NAME_LEN;
pub use self::constants::MAX_TAGLINE_LEN;
pub use self::constants::MAX_DOMAIN_LEN;
pub use self::constants::MAX_STORAGE_REF_LEN;
pub use self::constants::MAX_COMPANY_ID_LEN;
pub use self::constants::MAX_TENANT_ID_LEN;
pub use self::constants::MODE_ALLOWED;
pub use self::constants::MODE_DENIED;
pub use self::constants::MODE_INHERIT;
pub use self::constants::DOMAIN_NONE;
pub use self::constants::DOMAIN_REQUESTED;
pub use self::constants::DOMAIN_VERIFIED;
pub use self::constants::DOMAIN_REJECTED;
pub use self::types::AllowanceMode;
pub use self::types::GlobalAllowance;
pub use self::types::CompanyAllowance;
pub use self::types::GlobalDefaults;
pub use self::types::BrandingProfile;
pub use self::types::ResolvedBranding;
pub use self::types::HistoryEntry;
pub use self::types::PutGlobalAllowanceInput;
pub use self::types::PutCompanyAllowanceInput;
pub use self::types::PutDefaultsInput;
pub use self::types::PutBrandingInput;
pub use self::state::WhitelabelState;
pub use self::service::effective_allowance;
pub use self::service::resolve_branding;
pub use self::routes::WhitelabelModule;
pub use self::routes::register;
