//! Auth Manager - Module definition: routes, hooks, and registration.

use crate::auth::{password as auth_password, totp as auth_totp};
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hardening::{shared_state, trusted_client_ip_for_http_request};
use unified_core::hooks::events;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::db::models::User;
use unified_core::db::DbPool;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use uuid::Uuid;
use super::{providers::*, jwt::*, state::*, handlers_providers::*, login::*, tokens::*, recovery::*, totp::*, google::*, reset::*};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AuthManagerModule {
    pub(crate) state: AuthManagerState,
}

impl AuthManagerModule {
    pub fn new() -> Self {
        Self {
            state: AuthManagerState::default(),
        }
    }
}

impl Default for AuthManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AuthManagerModule {
    fn id(&self) -> &'static str {
        "auth-manager"
    }

    fn name(&self) -> &'static str {
        "Auth Manager"
    }

    fn description(&self) -> &'static str {
        "Authentication providers, per-provider settings, JWT issue, verify, and revoke, and the unified Google connector (OAuth 2.0 with PKCE, SSO status, token refresh and revocation, Google Drive linking)."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/providers", owner: "auth-manager", description: "List providers", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/providers/{id}/{action}", owner: "auth-manager", description: "Set provider enabled", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/providers/{id}/settings", owner: "auth-manager", description: "Get provider settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/auth-manager/providers/{id}/settings", owner: "auth-manager", description: "Update provider settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/issue", owner: "auth-manager", description: "Login with email and password (plus TOTP when enabled) and issue a token; scopes come only from the user's database roles", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/verify", owner: "auth-manager", description: "Verify token handler", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/revoke", owner: "auth-manager", description: "Revoke token handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/totp/setup", owner: "auth-manager", description: "Start TOTP enrolment: generate a pending secret and provisioning URI", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/totp/confirm", owner: "auth-manager", description: "Confirm TOTP enrolment with a valid code; activates TOTP on the account", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/totp/disable", owner: "auth-manager", description: "Disable TOTP after a password check plus a valid code or recovery code", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/generate", owner: "auth-manager", description: "Generate recovery codes", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/verify", owner: "auth-manager", description: "Verify recovery code", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/regenerate", owner: "auth-manager", description: "Regenerate recovery codes", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/recovery-codes/remaining", owner: "auth-manager", description: "Remaining recovery codes handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/revoke", owner: "auth-manager", description: "Revoke recovery codes", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/password/forgot", owner: "auth-manager", description: "Request a password-reset email (generic response either way; no account enumeration)", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/password/reset", owner: "auth-manager", description: "Redeem a password-reset token and set a new password", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/status", owner: "auth-manager", description: "Google status", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/authorize", owner: "auth-manager", description: "Google authorize", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/callback", owner: "auth-manager", description: "Google callback", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/refresh", owner: "auth-manager", description: "Google refresh", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/revoke", owner: "auth-manager", description: "Google revoke", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/accounts", owner: "auth-manager", description: "Google accounts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/sso/status", owner: "auth-manager", description: "SSO flow status: whether a Google sign-in can start right now.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/drive/links", owner: "auth-manager", description: "Google drive links", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/drive/links", owner: "auth-manager", description: "Google drive link", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/auth-manager/google/drive/links/{id}", owner: "auth-manager", description: "Google drive unlink", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/auth-manager")
                .app_data(state)
                .route("/providers", web::get().to(list_providers))
                .route("/providers/{id}/{action}", web::post().to(set_provider_enabled))
                .route(
                    "/providers/{id}/settings",
                    web::get().to(get_provider_settings),
                )
                .route(
                    "/providers/{id}/settings",
                    web::put().to(update_provider_settings),
                )
                .route("/jwt/issue", web::post().to(issue_token))
                .route("/jwt/verify", web::post().to(verify_token_handler))
                .route("/jwt/revoke", web::post().to(revoke_token_handler))
                .route("/jwt/totp/setup", web::post().to(totp_setup))
                .route("/jwt/totp/confirm", web::post().to(totp_confirm))
                .route("/jwt/totp/disable", web::post().to(totp_disable))
                .route(
                    "/recovery-codes/generate",
                    web::post().to(generate_recovery_codes),
                )
                .route(
                    "/recovery-codes/verify",
                    web::post().to(verify_recovery_code),
                )
                .route(
                    "/recovery-codes/regenerate",
                    web::post().to(regenerate_recovery_codes),
                )
                .route(
                    "/recovery-codes/remaining",
                    web::get().to(remaining_recovery_codes_handler),
                )
                .route(
                    "/recovery-codes/revoke",
                    web::post().to(revoke_recovery_codes),
                )
                .route("/password/forgot", web::post().to(request_password_reset))
                .route("/password/reset", web::post().to(reset_password))
                .route("/google/status", web::get().to(google_status))
                .route("/google/authorize", web::get().to(google_authorize))
                .route("/google/callback", web::post().to(google_callback))
                .route("/google/refresh", web::post().to(google_refresh))
                .route("/google/revoke", web::post().to(google_revoke))
                .route("/google/accounts", web::get().to(google_accounts))
                .route("/google/sso/status", web::get().to(google_sso_status))
                .route("/google/drive/links", web::get().to(google_drive_links))
                .route("/google/drive/links", web::post().to(google_drive_link))
                .route(
                    "/google/drive/links/{id}",
                    web::delete().to(google_drive_unlink),
                ),
        );
    }

    /// Load the persisted JWT revocations into memory before the
    /// first request is served, so a token revoked before a restart
    /// is denied from the start instead of only after the first
    /// verify/revoke call hydrates lazily. Best-effort: when no
    /// database pool is attached, or the load fails (table not
    /// migrated yet, database unreachable), this skips silently and
    /// the lazy hydration in `ensure_revoked_tokens_hydrated`
    /// still applies — startup must never fail because the
    /// revocation table is unavailable.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
            if let Ok(rows) = AuthManagerState::load_revoked_tokens(&pool) {
                let now_unix = Utc::now().timestamp();
                for row in rows {
                    self.state.revoked_tokens.insert(
                        row.jti,
                        RevokedToken {
                            revoked_at: row.revoked_at,
                            expires_at: row.expires_at.timestamp(),
                        },
                    );
                }
                self.state.prune_revoked_tokens(now_unix);
                self.state
                    .revocations_hydrated
                    .store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuthManagerModule::new()));
}
