// Auditors & Public Link Sharing module - temporary sandbox
// auditor accounts with scoped read-only access, and public links
// that expose exactly one resource in one view.
//
// Routes live under `/api/v1/auditors` and `/api/v1/sharing`:
//
//   Auditors:
//     POST / - create a temp auditor account (expiry required).
//     GET  / - list auditor accounts (paginated, filter by status,
//          auditor_of, company_id, search on label/email).
//     GET  /{id} - one auditor account with its scope.
//     POST /{id}/revoke - revoke immediately (kills live sessions).
//     POST /{id}/extend - extend the expiry (capped at the maximum).
//     GET  /{id}/sessions - sessions of one auditor.
//     POST /{id}/sessions/{session_id}/revoke - kill one session.
//     GET  /{id}/activity - auditor action log (paginated, filter
//          by action, allowed).
//     GET  /sessions/active - active auditor sessions (banner data
//          for the UI: who is watching, since when, what scope).
//     GET  /me/scope - the calling auditor's own scope (self only).
//     POST /expire-sweep - run the expiry sweep now (admin).
//   Public links:
//     POST /api/v1/sharing/links - create a link (the target's
//          `allow_public_link` flag is enforced server-side through
//          the `sharing.allow_check` hook; a false answer is a 403).
//     GET  /api/v1/sharing/links - list links (paginated, filter by
//          scope_kind, created_by, revoked, expired).
//     GET  /api/v1/sharing/links/{id} - one link (never the token).
//     POST /api/v1/sharing/links/{id}/revoke - revoke a link.
//     GET  /api/v1/sharing/links/{id}/access-log - access log of one
//          link (paginated, filter by allowed).
//     GET  /api/v1/sharing/access-log - access log of all links
//          (paginated, filter by allowed, link_id, reason).
//     GET  /api/v1/sharing/kinds - registered shareable kinds.
//   Public (no auth; see the public-access note below):
//     GET  /api/v1/sharing/public/{token}?scope_kind=&scope_id=
//          &scope_view= - validate a link token and return its scope
//          descriptor. Serves NO resource content: the owning module
//          serves content after calling
//          `unified_core::sharing::validate_public_link` itself.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory auditor accounts, scopes, sessions, action log, public
// links, and access log; Argon2id hashing of the auditor one-time
// password (shown exactly once at creation); SHA-256 token hashes
// (raw tokens are never stored); constant-time token comparison;
// HMAC-signed link payloads; server-side enforcement of the
// `allow_public_link` flag through the `sharing.allow_check` hook
// (fail closed when no module answers); failed link validations
// feeding the hardening engine's progressive lockout; auto-expire
// of auditor accounts (checked on every use, plus a scheduler job
// kind `auditors.expire`); one-click revoke that kills live
// sessions immediately; scope evaluation through
// `unified_core::sharing::AuditorScope` (allow/deny for one
// (module, kind, record, company) tuple, export only when the
// scope entry grants it). What is NOT built here: the actual user
// row in the users module (the auditor account references a user
// by opaque id string, following the codebase convention; the
// users module stays the source of truth for identity, and no
// user row is created or modified here); login / session issuing
// for auditors (the auth flow owns sessions - this module only
// records a session row when `auth.login` fires for an auditor
// user id, and revokes sessions by marking them revoked); the
// resource content behind a public link (owning modules serve it
// after validating the link through `core::sharing`); and the
// durable read path (see the persistence note below).
//
// Persistence note: the in-memory DashMap store is authoritative,
// exactly like the events module. Durable rows for these types are
// defined by migration `2026-11-22-000067_sharing_auditor`; until
// that migration is executed, auditor accounts and links do not
// survive a restart. The migration has never been executed against
// a live PostgreSQL database.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). The public route below relies on that
// gate: an administrator must create the matching `ApiAccessRule`
// row for `GET /api/v1/sharing/public/{token}`. The route is
// registered in `route_info()` with `route_version_support()`
// defaulting to `All`, so the API Manager's per-version registry
// picks it up. Do NOT treat the absence of a capability check on
// the public route as an open door: without the rule the API
// Manager denies the call, and without a valid token the handler
// itself denies it.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai and events modules. Capability
// checks use the `role.check` hook and fail closed (no answer
// means not allowed). The public route takes no identity headers:
// the link token IS the credential, and it grants read-only
// access to exactly one scoped resource.
//
// Auditor escalation guards: an auditor account can never gain a
// role (no endpoint assigns one), can never change a password to a
// permanent account (the one-time password is set once at creation
// and there is no password-change endpoint for auditors), and can
// never create an API key (API-key creation requires a capability
// auditors never hold, and the scope check rejects the
// `apikey.create` action before any scope lookup). Auditor callers
// are recognised by `X-User-Id` matching an active auditor account;
// when a request carries an auditor identity, the module's own
// handlers reject every mutation with 403 - auditors are
// read-only always, including on this module's own routes (an
// auditor may read its own scope and nothing else here).
//
// Events emitted on the hook bus:
//   - `sharing.link.created`, `sharing.link.revoked`,
//     `sharing.link.accessed`, `sharing.link.access_denied`
//   - `auditors.created`, `auditors.revoked`, `auditors.expired`,
//     `auditors.action`, `auditors.session.started`,
//     `auditors.session.revoked`
//   - `scheduler.enqueue` with `{"kind": "auditors.expire",
//     "payload": {"auditor_id", "run_at"}}` when an auditor is
//     created (the sweep job for its expiry).
//   - `notification.requested` when an auditor account is created
//     or revoked, addressed to the creating admin (`auditor_of`).
// Events this module listens to:
//   - `auditors.expire_requested` - the event the scheduler emits
//     when an `auditors.expire` job fires, following its naming
//     rule (`backup.run` -> `backup.run_requested`); expires the
//     named account (when due) or sweeps all due accounts.
//   - `auditors.scope.check` - answers `allowed` from the stored
//     auditor scope for one (module, kind, record, company,
//     action) tuple; `action` is `"read"` or `"export"`, anything
//     else is denied.
//   - `auth.login` - when the logging-in user id belongs to an
//     active auditor account, records a session row so the banner
//     endpoint can show it. The payload shape is read defensively
//     (`user_id`, then `actor`, then `id`); an unrecognised payload
//     is ignored, never an error.
//
// Scheduler note: `auditors.expire` IS in the scheduler's
// `KNOWN_KINDS` list (`scheduler/types.rs`) and its dispatch arm
// in `scheduler/service.rs` emits `auditors.expire_requested`
// with the auditor payload, so the expiry job queued at creation
// is accepted (`scheduler_status: "queued"`). The account is
// stored either way and stays usable; expiry is ALSO checked on
// every use (see `AuditorIdentity::is_active`), so a missed sweep
// can never leave an expired auditor able to read.

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// Module

pub struct AuditorsModule {
    pub(crate) state: AuditorsState,
}

impl AuditorsModule {
    pub fn new() -> Self {
        Self {
            state: AuditorsState::default(),
        }
    }
}

impl Default for AuditorsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AuditorsModule {
    fn id(&self) -> &'static str {
        "auditors"
    }

    fn name(&self) -> &'static str {
        "Auditors & Public Link Sharing"
    }

    fn description(&self) -> &'static str {
        "Temporary sandbox auditor accounts with scoped read-only access (time-limited, auto-expiring, one-click revoke, full action log) and public links that expose exactly one resource in one view (token hashed, HMAC-signed, server-side allow_public_link enforcement, full access log)."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "audit", "scheduler", "security"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "audit", "scheduler", "security"]
    }

    /// Auditors & sharing is core security infrastructure, not a
    /// sellable product: it is not declared available as a product.
    /// (The integrator may flip this if the product team decides
    /// auditor seats are sold separately; the category would then
    /// be "infrastructure".)
    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("auditors", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        // Register the shareable kinds before any link can be
        // minted: an unregistered kind fails closed by design.
        // A failure here fails module init, never a half-registered
        // registry.
        self.state.register_builtin_shareable_kinds()?;
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            EXPIRE_REQUESTED,
            "auditors",
            Arc::new(move |ctx: &mut HookContext| {
                on_expire_requested(Arc::as_ref(&state).clone(), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            SCOPE_CHECK,
            "auditors",
            Arc::new(move |ctx: &mut HookContext| {
                on_scope_check(Arc::as_ref(&state).clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::AUTH_LOGIN,
            "auditors",
            Arc::new(move |ctx: &mut HookContext| {
                on_auth_login(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/auditors")
                .app_data(state.clone())
                .route("", web::post().to(create_auditor))
                .route("", web::get().to(list_auditors))
                .route("/sessions/active", web::get().to(list_active_auditor_sessions))
                .route("/me/scope", web::get().to(get_my_scope))
                .route("/expire-sweep", web::post().to(expire_sweep))
                .route("/{id}", web::get().to(get_auditor))
                .route("/{id}/revoke", web::post().to(revoke_auditor))
                .route("/{id}/extend", web::post().to(extend_auditor))
                .route("/{id}/sessions", web::get().to(list_auditor_sessions))
                .route(
                    "/{id}/sessions/{session_id}/revoke",
                    web::post().to(revoke_auditor_session),
                )
                .route("/{id}/activity", web::get().to(list_auditor_activity)),
        );
        cfg.service(
            web::scope("/api/v1/sharing")
                .app_data(state)
                .route("/links", web::post().to(create_link))
                .route("/links", web::get().to(list_links))
                .route("/kinds", web::get().to(list_shareable_kinds))
                .route("/access-log", web::get().to(list_access_log))
                .route("/public/{token}", web::get().to(get_public_by_token))
                .route("/links/{id}", web::get().to(get_link))
                .route("/links/{id}/revoke", web::post().to(revoke_link))
                .route("/links/{id}/access-log", web::get().to(list_link_access_log)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/auditors", owner: "auditors", description: "Create a temp auditor account (expiry in hours required; one-time password shown once; expiry job queued as auditors.expire).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auditors", owner: "auditors", description: "List auditor accounts (paginated, filter by status, auditor_of, company_id, search on label/email).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auditors/{id}", owner: "auditors", description: "Get one auditor account with its scope.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auditors/{id}/revoke", owner: "auditors", description: "Revoke an auditor account immediately (kills live sessions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auditors/{id}/extend", owner: "auditors", description: "Extend an auditor account expiry (capped at now + max auditor lifetime).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auditors/{id}/sessions", owner: "auditors", description: "List sessions of one auditor (paginated, filter by live).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auditors/{id}/sessions/{session_id}/revoke", owner: "auditors", description: "Kill one auditor session.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auditors/{id}/activity", owner: "auditors", description: "Auditor action log (paginated, filter by action, allowed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auditors/sessions/active", owner: "auditors", description: "Active auditor sessions for the UI banner (who is watching, since when, what scope).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auditors/me/scope", owner: "auditors", description: "The calling auditor's own scope (self only; no role capability - the caller must be an active auditor account).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auditors/expire-sweep", owner: "auditors", description: "Run the auditor expiry sweep now (admin; same sweep the auditors.expire job runs).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/sharing/links", owner: "auditors", description: "Create a public link (target allow_public_link enforced server-side via sharing.allow_check; 403 when false).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/sharing/links", owner: "auditors", description: "List public links (paginated, filter by scope_kind, created_by, revoked, expired).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/sharing/links/{id}", owner: "auditors", description: "Get one public link (never the raw token).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/sharing/links/{id}/revoke", owner: "auditors", description: "Revoke a public link.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/sharing/links/{id}/access-log", owner: "auditors", description: "Access log of one public link (paginated, filter by allowed, reason).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/sharing/access-log", owner: "auditors", description: "Access log of all public links (paginated, filter by allowed, link_id, reason).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/sharing/kinds", owner: "auditors", description: "List registered shareable resource kinds and their views.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/sharing/public/{token}", owner: "auditors", description: "PUBLIC: validate a link token and return its scope descriptor (no auth; gated by the API Manager ApiAccessRule; serves no resource content).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuditorsModule::new()));
}
