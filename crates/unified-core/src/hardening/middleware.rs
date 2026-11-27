// hardening - middleware.
// Actix middlewares: headers, IP filter, rate limit, body size.
//

use super::helpers::rate_limited_response;
use super::helpers::set_header;
use super::*;
use crate::error::UnifiedError;
use crate::hooks::HookBus;
use crate::json;
use crate::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::HttpResponse;
use actix_web::dev::ServiceRequest;
use actix_web::http::header::{self, HeaderValue};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// Middleware

/// Adds the configured security headers to every response. Runs first
/// (priority 1000) so error responses short-circuited by the later
/// hardening middleware still carry the headers.
#[derive(Clone)]
pub struct SecurityHeadersMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for SecurityHeadersMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecurityHeadersMiddleware")
    }
}

impl SecurityHeadersMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for SecurityHeadersMiddleware {
    fn priority(&self) -> isize {
        1000
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        Box::pin(async move {
            let mut res = next(req).await?;
            let settings = state.settings();
            if settings.enabled {
                let headers = res.headers_mut();
                set_header(
                    headers,
                    header::STRICT_TRANSPORT_SECURITY,
                    &settings.headers_hsts,
                );
                set_header(
                    headers,
                    header::X_CONTENT_TYPE_OPTIONS,
                    &settings.headers_content_type_options,
                );
                set_header(
                    headers,
                    header::X_FRAME_OPTIONS,
                    &settings.headers_frame_options,
                );
                set_header(
                    headers,
                    header::REFERRER_POLICY,
                    &settings.headers_referrer_policy,
                );
                set_header(
                    headers,
                    header::CONTENT_SECURITY_POLICY,
                    &settings.headers_content_security_policy,
                );
                set_header(
                    headers,
                    header::PERMISSIONS_POLICY,
                    &settings.headers_permissions_policy,
                );
            }
            Ok(res)
        })
    }
}

/// Rejects blocklisted IPs with 403. Allowlisted IPs are left alone
/// here (the rate limiter gives them their bypass). The ACME challenge
/// route is exempt: a blocked challenge would break certificate
/// issuance and renewal.
#[derive(Clone)]
pub struct IpFilterMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for IpFilterMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IpFilterMiddleware")
    }
}

impl IpFilterMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for IpFilterMiddleware {
    fn priority(&self) -> isize {
        900
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let settings = state.settings();
        if !settings.enabled {
            return Box::pin(async move { next(req).await });
        }
        let path = req.path().to_string();
        if is_acme_challenge_path(&path) || state.is_exempt_path(&path, &settings) {
            return Box::pin(async move { next(req).await });
        }
        if let Some(ip) = trusted_client_ip_for_request(&req, &settings) {
            if state.is_blocklisted(ip) {
                let error = UnifiedError::forbidden("Request rejected: IP address is blocked")
                    .with_metadata("ip", serde_json::json!(ip.to_string()))
                    .with_metadata("list", serde_json::json!("block"));
                let response = HttpResponse::Forbidden().json(json::error_json(&error, false));
                return Box::pin(async move { Ok(req.into_response(response)) });
            }
        }
        Box::pin(async move { next(req).await })
    }
}

/// Fixed-window rate limiting per IP, per user, and per endpoint, plus
/// flood-burst detection and per-IP concurrent-connection limits.
/// Exceeding any limit returns 429 in the standard error envelope
/// with `Retry-After` and `X-RateLimit-*` headers. Allowlisted IPs
/// bypass this middleware entirely. The ACME challenge route and the
/// other exempt paths bypass it as well.
#[derive(Clone)]
pub struct RateLimitMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for RateLimitMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RateLimitMiddleware")
    }
}

impl RateLimitMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for RateLimitMiddleware {
    fn priority(&self) -> isize {
        800
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let settings = state.settings();
        if !settings.enabled || !settings.rate_limit_enabled {
            return Box::pin(async move { next(req).await });
        }
        let path = req.path().to_string();
        if is_acme_challenge_path(&path) || state.is_exempt_path(&path, &settings) {
            return Box::pin(async move { next(req).await });
        }
        let ip = match trusted_client_ip_for_request(&req, &settings) {
            Some(ip) => ip,
            None => return Box::pin(async move { next(req).await }),
        };
        if state.is_allowlisted(ip) {
            return Box::pin(async move { next(req).await });
        }
        let ip_text = ip.to_string();

        // Flood burst: reject immediately with a challenge flag (see
        // `check_burst` for why the request is not held instead).
        if let Some(retry_after) = state.check_burst(&ip_text, &settings) {
            state.hooks.emit(
                security_events::RATE_LIMITED,
                serde_json::json!({
                    "ip": ip_text,
                    "path": path,
                    "scope": "flood",
                    "retry_after_secs": retry_after,
                }),
            );
            let response = rate_limited_response(
                settings.flood_burst_threshold,
                0,
                retry_after,
                retry_after,
                true,
            );
            return Box::pin(async move { Ok(req.into_response(response)) });
        }

        let user = user_key(&req);
        let endpoint_key = format!(
            "{}:{}",
            req.method(),
            normalize_endpoint_path(&path)
        );
        let decision = state.check_rate_limits(
            &format!("ip:{ip_text}"),
            user.as_deref(),
            &endpoint_key,
            &settings,
        );
        let (limit, remaining, reset_in_secs) = match decision {
            RateDecision::Allowed {
                remaining,
                reset_in_secs,
            } => (
                settings.rate_limit_per_ip_requests,
                remaining,
                reset_in_secs,
            ),
            RateDecision::Limited { retry_after_secs } => {
                state.hooks.emit(
                    security_events::RATE_LIMITED,
                    serde_json::json!({
                        "ip": ip_text,
                        "path": path,
                        "scope": "rate_limit",
                        "retry_after_secs": retry_after_secs,
                    }),
                );
                let response = rate_limited_response(
                    settings.rate_limit_per_ip_requests,
                    0,
                    retry_after_secs,
                    retry_after_secs,
                    false,
                );
                return Box::pin(async move { Ok(req.into_response(response)) });
            }
        };

        // Concurrent-connection limit: counted only around the inner
        // call so a rejected request never occupies a slot. The slot
        // is a drop guard, so a request dropped mid-flight (client
        // disconnect) releases it instead of leaking the count.
        let (in_flight, slot) = ConcurrentSlot::acquire(&state, &ip_text);
        if in_flight > u64::from(settings.max_concurrent_per_ip) {
            drop(slot);
            let retry_after = 1u64;
            state.hooks.emit(
                security_events::RATE_LIMITED,
                serde_json::json!({
                    "ip": ip_text,
                    "path": path,
                    "scope": "concurrent",
                    "retry_after_secs": retry_after,
                }),
            );
            let response = rate_limited_response(
                settings.max_concurrent_per_ip,
                0,
                retry_after,
                retry_after,
                false,
            );
            return Box::pin(async move { Ok(req.into_response(response)) });
        }

        Box::pin(async move {
            let _slot = slot;
            let mut res = next(req).await;
            if let Ok(res) = &mut res {
                let headers = res.headers_mut();
                headers.insert(
                    header::HeaderName::from_static("X-RateLimit-Limit"),
                    HeaderValue::from_str(&limit.to_string())
                        .unwrap_or(HeaderValue::from_static("0")),
                );
                headers.insert(
                    header::HeaderName::from_static("X-RateLimit-Remaining"),
                    HeaderValue::from_str(&remaining.to_string())
                        .unwrap_or(HeaderValue::from_static("0")),
                );
                headers.insert(
                    header::HeaderName::from_static("X-RateLimit-Reset"),
                    HeaderValue::from_str(&(Utc::now().timestamp() + reset_in_secs as i64).to_string())
                        .unwrap_or(HeaderValue::from_static("0")),
                );
            }
            res
        })
    }
}

/// Enforces the configured maximum request body size per route group
/// from the `Content-Length` header. Requests without a
/// `Content-Length` (chunked bodies) pass this check; the Actix
/// payload limits bound them instead. Oversize requests get 413 in
/// the standard error envelope.
#[derive(Clone)]
pub struct RequestSizeMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for RequestSizeMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RequestSizeMiddleware")
    }
}

impl RequestSizeMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for RequestSizeMiddleware {
    fn priority(&self) -> isize {
        700
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let settings = state.settings();
        if !settings.enabled {
            return Box::pin(async move { next(req).await });
        }
        let path = req.path().to_string();
        let content_length = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if let Some(content_length) = content_length {
            let (limit, group) = state.body_limit_for_path(&path, &settings);
            if content_length > limit {
                let error = UnifiedError::new("Request body is too large", 413)
                    .with_metadata("content_length", serde_json::json!(content_length))
                    .with_metadata("max_bytes", serde_json::json!(limit))
                    .with_metadata("limit_group", serde_json::json!(group));
                let response =
                    HttpResponse::PayloadTooLarge().json(json::error_json(&error, false));
                return Box::pin(async move { Ok(req.into_response(response)) });
            }
        }
        Box::pin(async move { next(req).await })
    }
}
