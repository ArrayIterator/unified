// Dashboards - Dashboard authentication middleware.

use unified_core::error::UnifiedError;
use unified_core::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::dev::ServiceRequest;
use actix_web::HttpMessage;
use actix_web::HttpResponse;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::{CallerIdentity, DashboardsState, EVENT_VIEW_AS_EXPIRED, extract_token, verify_jwt};

// ---------------------------------------------------------------------------
// Layer 1 — middleware (authentication + view-as expiry on dashboard paths)
// ---------------------------------------------------------------------------

/// Authenticates every request under a dashboard prefix and stores
/// the caller in the request extensions. Unauthenticated requests get
/// 401 in the standard error envelope before any handler runs.
/// Non-dashboard paths pass through untouched. Capability checks
/// stay in the handlers (layer 2); this middleware deliberately does
/// not decide which sections a caller may see.
pub struct DashboardsAuthMiddleware {
    pub(crate) state: Arc<DashboardsState>,
}

impl std::fmt::Debug for DashboardsAuthMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DashboardsAuthMiddleware")
    }
}

impl Clone for DashboardsAuthMiddleware {
    fn clone(&self) -> Self {
        Self { state: self.state.clone() }
    }
}

impl DashboardsAuthMiddleware {
    pub(crate) fn new(state: Arc<DashboardsState>) -> Self {
        Self { state }
    }
}

impl Middleware for DashboardsAuthMiddleware {
    /// After the security-hardening middleware (priorities 700-1000),
    /// before default-priority module middleware.
    fn priority(&self) -> isize {
        600
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let path = req.path().to_string();
        if !state.is_dashboard_path(&path) {
            return Box::pin(async move { next(req).await });
        }
        // The middleware only has the state, not the AppContext, so it
        // rebuilds the minimal authentication inputs from the stashed
        // config and hook handles filled in by `init`. When `init`
        // has not run, fail closed: dashboard paths are not served
        // without authentication.
        let Some(config) = state.config.get() else {
            let error = UnifiedError::unauthorized(
                "Dashboard authentication is not configured",
            );
            let response = HttpResponse::Unauthorized().json(error.to_json(false));
            return Box::pin(async move { Ok(req.into_response(response)) });
        };
        let (secret_key, salt_key, session_name) = {
            let config = config.read().expect("config lock poisoned");
            (
                config.app.secret_key.clone(),
                config.app.salt_key.clone(),
                config.app.session_name.clone(),
            )
        };
        let token = extract_token(req.request(), &session_name);
        let identity = match token {
            Some(token) => match verify_jwt(&token, &secret_key, &salt_key) {
                Ok(claims) => match Uuid::parse_str(claims.sub.trim()) {
                    Ok(user_id) => {
                        let mut identity =
                            CallerIdentity::direct(user_id, claims.scopes);
                        // Apply (and expire) a view-as session. The
                        // audit event for expiry goes through the
                        // stashed hook bus when available.
                        if let Some(session) = state.view_as.get(&user_id) {
                            let session = session.value().clone();
                            if session.expires_at <= Utc::now() {
                                state.view_as.remove(&user_id);
                                state.emit_audit(
                                    EVENT_VIEW_AS_EXPIRED,
                                    json!({
                                        "actor_id": user_id.to_string(),
                                        "target_user_id": session.target_user_id.to_string(),
                                        "reason": session.reason,
                                        "resource": format!("user:{}", session.target_user_id),
                                        "expired_at": session.expires_at.to_rfc3339(),
                                    }),
                                );
                            } else {
                                identity.user_id = session.target_user_id;
                                identity.impersonating = true;
                                identity.impersonated_by = Some(user_id);
                                identity.view_as_expires_at = Some(session.expires_at);
                            }
                        }
                        Some(identity)
                    }
                    Err(_) => None,
                },
                Err(_) => None,
            },
            None => None,
        };
        let Some(identity) = identity else {
            let error = UnifiedError::unauthorized(
                "Authentication required: provide a Bearer token",
            );
            // The middleware has the stashed config (unlike the
            // not-configured branch above, where no config exists),
            // so the trace renders in development mode like every
            // other error path.
            let debug = config
                .read()
                .expect("config lock poisoned")
                .is_debug();
            let response = HttpResponse::Unauthorized().json(error.to_json(debug));
            return Box::pin(async move { Ok(req.into_response(response)) });
        };
        let mut req = req;
        req.extensions_mut().insert(identity);
        Box::pin(async move { next(req).await })
    }
}
