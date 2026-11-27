// Api Manager - Public API access middleware and rate-limit errors.

use unified_core::error::UnifiedError;
use unified_core::hardening::client_ip;
use unified_core::json as api_json;
use unified_core::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::dev::ServiceRequest;
use actix_web::HttpResponse;
use chrono::Utc;

use super::{API_KEY_HEADER, ApiManagerState};

/// Build the standard-envelope error response for a 429, carrying
/// the same `Retry-After` and `X-RateLimit-*` headers the hardening
/// rate limiter sets (see
/// `unified_core::hardening::rate_limited_response`). Non-429 errors
/// pass through unchanged. Header values come from the error
/// metadata (`retry_after_secs`, `limit`, `remaining`,
/// `reset_in_secs`); a missing value leaves that header unset.
pub(crate) fn rate_limited_error_response(error: &UnifiedError, debug: bool) -> HttpResponse {
    let mut response = api_json::error_response(error, debug);
    if error.code != 429 {
        return response;
    }
    let headers = response.headers_mut();
    if let Some(retry_after) = error
        .metadata
        .get("retry_after_secs")
        .and_then(|value| value.as_u64())
    {
        if let Ok(value) =
            actix_web::http::header::HeaderValue::from_str(&retry_after.to_string())
        {
            headers.insert(actix_web::http::header::RETRY_AFTER, value);
        }
    }
    if let Some(limit) = error.metadata.get("limit").and_then(|value| value.as_u64()) {
        if let Ok(value) = actix_web::http::header::HeaderValue::from_str(&limit.to_string()) {
            headers.insert(
                actix_web::http::header::HeaderName::from_static("x-ratelimit-limit"),
                value,
            );
        }
    }
    if let Some(remaining) = error
        .metadata
        .get("remaining")
        .and_then(|value| value.as_u64())
    {
        if let Ok(value) =
            actix_web::http::header::HeaderValue::from_str(&remaining.to_string())
        {
            headers.insert(
                actix_web::http::header::HeaderName::from_static("x-ratelimit-remaining"),
                value,
            );
        }
    }
    if let Some(reset_in_secs) = error
        .metadata
        .get("reset_in_secs")
        .and_then(|value| value.as_u64())
    {
        let reset_at = Utc::now().timestamp() + reset_in_secs as i64;
        if let Ok(value) =
            actix_web::http::header::HeaderValue::from_str(&reset_at.to_string())
        {
            headers.insert(
                actix_web::http::header::HeaderName::from_static("x-ratelimit-reset"),
                value,
            );
        }
    }
    response
}

/// Request-path guard enforcing the access rules.
///
/// Runs at priority 850: after the IP filter (900, so blocked IPs
/// never reach the key check) and before the generic rate limiter
/// (800, so denied requests do not consume quota). It applies to
/// the public versioned surface (`/{prefix}/{version}/...`) and to
/// internal `/api/v1/...` routes called with an `X-Api-Key` header.
/// Requests without a key to internal routes pass through
/// untouched, so authenticated admin traffic is unaffected, and the
/// API Manager's own `/api/v1/api-manager/` prefix is exempt: its
/// public gateway enforces inside `resolve_public`, and its admin
/// routes enforce per handler (`caller` + `require_permission`
/// with `api-manager:read` / `api-manager:manage` -- there is no
/// session or JWT middleware on this server, so no admin route may
/// rely on one). The guard and the gateway check never overlap, so
/// a request is checked exactly once.
#[derive(Clone)]
pub struct PublicApiAccessMiddleware {
    pub(crate) state: ApiManagerState,
}

impl std::fmt::Debug for PublicApiAccessMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicApiAccessMiddleware")
    }
}

impl PublicApiAccessMiddleware {
    pub fn new(state: ApiManagerState) -> Self {
        Self { state }
    }
}

impl Middleware for PublicApiAccessMiddleware {
    fn priority(&self) -> isize {
        850
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let path = req.path().to_string();
        let method = req.method().to_string();
        let presented_key = req
            .headers()
            .get(API_KEY_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let prefix = state
            .prefix
            .read()
            .map(|settings| settings.prefix.clone())
            .unwrap_or_else(|_| "v".to_string());
        let public_root = format!("/{prefix}/");
        let is_public_surface = path.starts_with(&public_root);
        let is_keyed_internal = path.starts_with("/api/v1/")
            && presented_key.is_some()
            && !path.starts_with("/api/v1/api-manager/");
        if !is_public_surface && !is_keyed_internal {
            return Box::pin(async move { next(req).await });
        }
        let ip = client_ip(&req).map(|ip| ip.to_string());
        match state.check_public_access(&method, &path, presented_key.as_deref(), ip) {
            Ok(_) => Box::pin(async move { next(req).await }),
            Err(error) => {
                // `debug: false` on purpose: this middleware only
                // holds the `ApiManagerState` - no `AppContext`, so
                // the development mode is unknowable here and the
                // trace must never render. (The access errors it
                // renders carry no trace anyway.)
                let response = rate_limited_error_response(&error, false);
                Box::pin(async move { Ok(req.into_response(response)) })
            }
        }
    }
}
