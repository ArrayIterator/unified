// Privacy module.
//
// PII redaction helpers, data-minimization field declarations,
// one-way anonymization, private-by-default visibility, consent
// records, right-to-erasure requests, and versioned privacy
// notices. The in-memory DashMap store is authoritative; the
// migration 2026-11-22-000065_privacy tables are persistence
// targets only (see the migration header). Interconnection with
// the Compliance, Vault, and Files modules happens only through
// the hook bus: this module emits events, it never calls into
// those modules directly.
//
// Security notes:
//   - Every endpoint requires the `X-User-Id` and `X-Role`
//     headers and a capability check (`privacy:read`,
//     `privacy:write`, or `privacy:admin`), failing closed.
//   - Anonymization tokens are HMAC-SHA-256 values keyed by a
//     server-side key that is write-only: it is read from the
//     `UNIFIED_PRIVACY_ANONYMIZATION_KEY` environment variable
//     at startup and derived with Argon2id (fixed domain salt,
//     OWASP parameters; see `state.rs`), and is never stored in
//     a record, logged, or returned. The mapping is not
//     reversible. Tokens are stable across restarts only when
//     the environment key is configured: with no key, startup
//     logs an error and falls back to an ephemeral in-memory key
//     whose tokens change on every restart (dev/test only).
//     Production startup MUST fail fast instead: use
//     `PrivacyState::try_new` / `PrivacyModule::try_new` and
//     refuse to boot when the key is unset or shorter than 16
//     characters. Wiring that into `register_all` is the one
//     remaining startup decision; it is intentionally not done
//     here because the registration chain (`src/modules/mod.rs`)
//     is infallible today and changing it is outside this module.
//   - Consent uniqueness: exactly one record exists per
//     (user_id, consent_type) pair; concurrent grants are
//     serialized by a write lock (see `PrivacyState`). A repeated
//     withdraw is an idempotent no-op returning the record
//     unchanged. Erasure fulfill runs under a per-request guard,
//     so a concurrent second fulfill gets a 409 conflict instead
//     of duplicate anonymization noise.
//   - No original personal data is kept in anonymization,
//     erasure, or audit records: only derived tokens, scopes,
//     and actor ids.

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PrivacyModule {
    pub(crate) state: Arc<PrivacyState>,
}

impl PrivacyModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PrivacyState::new()),
        }
    }

    /// Fallible constructor for production startup: fails when no
    /// usable anonymization key is configured. See
    /// `PrivacyState::try_new`: this is not wired into
    /// `register_all` yet because that chain is infallible.
    #[allow(dead_code)]
    pub fn try_new() -> UnifiedResult<Self> {
        Ok(Self {
            state: Arc::new(PrivacyState::try_new()?),
        })
    }
}

impl Default for PrivacyModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PrivacyModule {
    fn id(&self) -> &'static str {
        "privacy"
    }

    fn name(&self) -> &'static str {
        "Privacy"
    }

    fn description(&self) -> &'static str {
        "Privacy: PII redaction, data minimization, anonymization, private-by-default visibility, consent, erasure, and privacy notices."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "audit"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Production must never boot on an ephemeral anonymization
        // key: tokens issued under a per-process key change on
        // every restart. The constructor already fell back to one
        // (register_all is infallible, so try_new is not wired
        // there); this init-time check is the fail-fast guard —
        // refuse to start in production when no usable key is
        // configured, instead of serving with unstable tokens.
        // Development keeps the loud fallback (with its error log)
        // so local boots do not need the environment variable.
        if ctx.config.read().expect("config lock poisoned").app.mode == "production" {
            super::state::try_load_anonymization_key()?;
        }

        ai::register_ai_capabilities("privacy", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/privacy/redact", owner: "privacy", description: "Redact PII in text or a JSON value", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/field-declarations", owner: "privacy", description: "List declared personal-data fields", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/field-declarations", owner: "privacy", description: "Declare a personal-data field for a module", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/field-declarations/{id}", owner: "privacy", description: "Get a field declaration", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/privacy/field-declarations/{id}", owner: "privacy", description: "Update a field declaration", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/privacy/field-declarations/{id}", owner: "privacy", description: "Delete a field declaration", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/validate-payload", owner: "privacy", description: "Validate a payload against declared fields", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/anonymize", owner: "privacy", description: "Anonymize a user (one-way, idempotent)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/anonymizations", owner: "privacy", description: "List anonymization records", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/anonymizations/{user_id}", owner: "privacy", description: "Get an anonymization record by user", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/visibility/defaults", owner: "privacy", description: "List per-module visibility defaults", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/visibility/defaults", owner: "privacy", description: "Set a per-module visibility default", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/visibility/{module_id}", owner: "privacy", description: "Get the effective visibility default for a module", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/consent-types", owner: "privacy", description: "List consent types", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/consent-types", owner: "privacy", description: "Define a consent type", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents", owner: "privacy", description: "List consent records", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/consents", owner: "privacy", description: "Grant or withdraw consent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents/current", owner: "privacy", description: "Get current consent for a user and type", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents/{id}", owner: "privacy", description: "Get a consent record", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/consents/{id}/withdraw", owner: "privacy", description: "Withdraw a consent record", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents/{id}/history", owner: "privacy", description: "List consent change history", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/erasure-requests", owner: "privacy", description: "List erasure requests", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests", owner: "privacy", description: "Create an erasure request", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/erasure-requests/{id}", owner: "privacy", description: "Get an erasure request", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests/{id}/approve", owner: "privacy", description: "Approve an erasure request", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests/{id}/fulfill", owner: "privacy", description: "Fulfill an erasure request via anonymization", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests/{id}/reject", owner: "privacy", description: "Reject an erasure request with a reason", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/notices", owner: "privacy", description: "List privacy notices", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/privacy/notices", owner: "privacy", description: "Publish a versioned privacy notice", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/notices/current", owner: "privacy", description: "Get the current notice for a module", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/notices/{id}", owner: "privacy", description: "Get a privacy notice", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/privacy/audit", owner: "privacy", description: "List privacy audit entries", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/privacy")
                .route("/redact", web::post().to(redact))
                .route("/field-declarations", web::get().to(list_field_declarations))
                .route("/field-declarations", web::post().to(create_field_declaration))
                .route("/field-declarations/{id}", web::get().to(get_field_declaration))
                .route("/field-declarations/{id}", web::put().to(update_field_declaration))
                .route("/field-declarations/{id}", web::delete().to(delete_field_declaration))
                .route("/validate-payload", web::post().to(validate_payload))
                .route("/anonymize", web::post().to(anonymize_user))
                .route("/anonymizations", web::get().to(list_anonymizations))
                .route("/anonymizations/{user_id}", web::get().to(get_anonymization))
                .route("/visibility/defaults", web::get().to(list_visibility_defaults))
                .route("/visibility/defaults", web::post().to(set_visibility_default))
                .route("/visibility/{module_id}", web::get().to(get_effective_visibility))
                .route("/consent-types", web::get().to(list_consent_types))
                .route("/consent-types", web::post().to(create_consent_type))
                .route("/consents", web::get().to(list_consents))
                .route("/consents", web::post().to(grant_consent))
                .route("/consents/current", web::get().to(get_current_consent))
                .route("/consents/{id}", web::get().to(get_consent))
                .route("/consents/{id}/withdraw", web::post().to(withdraw_consent))
                .route("/consents/{id}/history", web::get().to(list_consent_history))
                .route("/erasure-requests", web::get().to(list_erasure_requests))
                .route("/erasure-requests", web::post().to(create_erasure_request))
                .route("/erasure-requests/{id}", web::get().to(get_erasure_request))
                .route("/erasure-requests/{id}/approve", web::post().to(approve_erasure_request))
                .route("/erasure-requests/{id}/fulfill", web::post().to(fulfill_erasure_request))
                .route("/erasure-requests/{id}/reject", web::post().to(reject_erasure_request))
                .route("/notices", web::get().to(list_notices))
                .route("/notices", web::post().to(create_notice))
                .route("/notices/current", web::get().to(get_current_notice))
                .route("/notices/{id}", web::get().to(get_notice))
                .route("/audit", web::get().to(list_audit_entries)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PrivacyModule::new()));
}
