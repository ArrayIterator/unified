// Domain Verification module - ownership verification for custom domain
// aliases through admin-determined DNS records.
//
// A tenant adds a custom domain alias on the user dashboard (main site);
// this module issues a verification token and tells the dashboard the
// EXACT DNS record to create. The tenant creates the record, then the
// dashboard calls verify (or a scheduler does): the server performs
// exactly one DNS check per verify call with the Network module's
// `DnsVerifier` and, on success, emits
// `domain.verification.completed` so the SaaS Domains module can flip
// the alias status. A failed check returns a pending outcome and the
// token stays pending, so the next verify call can retry within the
// rate-limit budget; only the attempt that exhausts the budget is
// recorded as failed and flips the token to failed. Unverified
// domains never serve: resolvers and middleware ask this module through the
// `domain.verification.status_requested` hook event instead of trusting
// any stored flag.
//
// This module is API-only. It serves no tenant-facing page: the
// tenant subdomain serves services only, while the settings and the
// verify action live on the user dashboard (main site).
//
// Fully config-driven (no hard-coded verification behaviour):
// the record format is DETERMINED BY ADMIN, not by this code. The
// admin settings choose the record type (TXT or CNAME), the host and
// value templates, the token length and alphabet, the DNS timeout,
// the retry policy, the token lifetime, and the rate-limit window.
// The `DEFAULT_SETTINGS` below are clearly-marked CONFIGURABLE
// DEFAULTS only: they seed the settings store at startup and are
// replaced wholesale by whatever the admin saves. Changing the
// settings does NOT invalidate already-verified domains; the new
// format applies only to tokens issued after the change (tokens
// already issued keep the format they were issued with, because the
// rendered host/value are stored on the token at issuance time).
// The status vocabulary (`verified` / `pending` / `failed` /
// `unknown`) is the one exception: those four strings are part of
// the inter-module event contract fixed by the platform spec, so
// they are literal constants, not settings. Everything about WHEN a
// domain moves between those statuses (timeouts, attempts, windows)
// comes from settings.
//
// Security:
//   - Tokens are cryptographically random (OS RNG via `rand`) and
//     stored only as SHA-256 hashes. The raw token is returned
//     exactly once, in the issuance response; it is never stored,
//     never logged, and never included in any hook event.
//   - Every endpoint requires caller identity (`X-User-Id` and
//     `X-Role` headers) and a capability checked through the
//     `role.check` hook, failing closed (no answer means not
//     allowed), exactly like the properties module.
//   - Verify attempts are rate-limited per domain: once
//     `max_attempts` attempts have been recorded inside
//     `rate_limit_window_seconds`, further verify calls fail closed
//     with HTTP 429.
//   - `verified` is never reported unless a successful DNS check is
//     recorded against the CURRENT token for the domain.
//
// Persistence: the live data lives in the in-memory DashMap stores
// below (authoritative, following the network_tools precedent). The
// migration `2026-11-22-000072_domain_verification` creates the
// matching PostgreSQL tables as persistence targets; the module does
// not read them yet (the schema registry is owned by the integrator).
//
// Events emitted on the hook bus:
//   - `domain.verification.token_issued` - a token was issued
//     (never carries the raw token).
//   - `domain.verification.attempted` - one DNS check attempt.
//   - `domain.verification.completed` - verification succeeded;
//     payload `{"alias_id", "domain", "tenant_id", "company_id",
//     "status": "verified", "record_found", "verified_at"}`.
//   - `domain.verification.failed` - attempts exhausted; payload
//     `{"alias_id", "domain", "tenant_id", "company_id",
//     "status": "failed", "record_found", "error", "attempts",
//     "verified_at": null}`.
//   - `domain.verification.settings_updated` - admin settings
//     changed (audit event).
// Events this module listens to:
//   - `domain.verification.status_requested` - payload
//     `{"domain"}`; answers with `{"domain", "status",
//     "verified_at"}` (no result when the domain is unknown).

// Split into one file per concern under `src/modules/domain_verification/`.
// This root only declares the submodules and re-exports the
// public API at its original paths.

pub mod events;
pub mod settings;
pub mod tokens;
pub mod helpers;
pub mod requests;
pub mod handlers;
pub mod hooks;
pub mod module;

pub use self::events::{EVENT_TOKEN_ISSUED, EVENT_ATTEMPTED, EVENT_COMPLETED, EVENT_FAILED, EVENT_SETTINGS_UPDATED, EVENT_STATUS_REQUESTED, EVENT_VERIFY_RETRY_REQUESTED, EVENT_VERIFY_RETRY_COMPLETED, STATUS_VERIFIED, STATUS_PENDING, STATUS_FAILED, STATUS_UNKNOWN, ALL_STATUSES};
pub(crate) use self::events::{PERMISSION_MANAGE, PERMISSION_ISSUE, PERMISSION_VERIFY, PERMISSION_READ};
pub use self::settings::{VerificationSettings, default_settings};
pub(crate) use self::settings::{SETTINGS, settings_store, current_settings};
pub use self::tokens::{VerificationToken, VerificationTokenView, VerificationAttempt};
pub(crate) use self::tokens::{TOKENS, ATTEMPTS, tokens_store, attempts_store};
pub(crate) use self::helpers::{
    Caller, attempts_in_window, caller, caller_company, check_capability, current_token_for,
    domain_status, generate_token, hash_token, in_company_scope, normalize_domain, paginate,
    render_host, render_value, require_body_tenant_match, require_company_for_write,
    require_company_match_uuid, require_permission, require_record_in_company_scope,
    require_record_writable_in_company_scope, require_tenant, value_prefix,
};
pub(crate) use self::requests::{UpdateSettingsRequest, IssueTokenRequest, VerifyRequest, StatusQuery, TokenIssuance, VerifyOutcome};
pub(crate) use self::handlers::{get_settings, update_settings, issue_token, list_tokens, revoke_token, check_once, verify_domain_alias, list_attempts, get_status};
pub(crate) use self::hooks::{on_status_requested, on_verify_retry_requested, retry_pending_verifications, stash_ctx, hook_ctx, DOMAIN_VERIFICATION_CTX};
pub use self::module::{DomainVerificationModule, register};
