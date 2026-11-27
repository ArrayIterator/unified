// hardening - rate_limit.
// Per-IP / per-user / per-endpoint rate limiting and bursts.
//

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

#[derive(Debug, Clone)]
pub(crate) struct WindowBucket {
    count: u32,
    window_start: Instant,
    started_at: DateTime<Utc>,
}

/// One tracked rate-limit window, as returned by the status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct RateLimitEntry {
    pub scope: String,
    pub key: String,
    pub limit: u32,
    pub count: u32,
    pub window_secs: u64,
    pub window_started_at: DateTime<Utc>,
    pub reset_in_secs: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct BurstBucket {
    count: u32,
    window_start: Instant,
}

/// Outcome of one fixed-window rate-limit check.
#[derive(Debug, Clone)]
pub enum RateDecision {
    Allowed { remaining: u32, reset_in_secs: u64 },
    Limited { retry_after_secs: u64 },
}

impl HardeningState {
    // -- Rate limiting ---------------------------------------------------

    fn check_window(
        map: &DashMap<String, WindowBucket>,
        key: &str,
        limit: u32,
        window_secs: u64,
    ) -> RateDecision {
        let now = Instant::now();
        let mut bucket = map.entry(key.to_string()).or_insert_with(|| WindowBucket {
            count: 0,
            window_start: now,
            started_at: Utc::now(),
        });
        if bucket.window_start.elapsed().as_secs() >= window_secs {
            bucket.count = 0;
            bucket.window_start = now;
            bucket.started_at = Utc::now();
        }
        bucket.count += 1;
        if bucket.count > limit {
            let elapsed = bucket.window_start.elapsed().as_secs();
            RateDecision::Limited {
                retry_after_secs: window_secs.saturating_sub(elapsed).max(1),
            }
        } else {
            let elapsed = bucket.window_start.elapsed().as_secs();
            RateDecision::Allowed {
                remaining: limit.saturating_sub(bucket.count),
                reset_in_secs: window_secs.saturating_sub(elapsed).max(1),
            }
        }
    }

    /// Run the per-IP, per-user, and per-endpoint fixed-window checks.
    /// Returns the first limiting decision, or the per-IP allowance
    /// (used for the response headers) when all checks pass.
    pub fn check_rate_limits(
        &self,
        ip_key: &str,
        user_key: Option<&str>,
        endpoint_key: &str,
        settings: &SecuritySettings,
    ) -> RateDecision {
        match Self::check_window(
            &self.rate_ip,
            ip_key,
            settings.rate_limit_per_ip_requests,
            settings.rate_limit_per_ip_window_secs,
        ) {
            RateDecision::Limited { retry_after_secs } => {
                return RateDecision::Limited { retry_after_secs };
            }
            allowed @ RateDecision::Allowed { .. } => {
                if let Some(user_key) = user_key {
                    if let RateDecision::Limited { retry_after_secs } = Self::check_window(
                        &self.rate_user,
                        user_key,
                        settings.rate_limit_per_user_requests,
                        settings.rate_limit_per_user_window_secs,
                    ) {
                        return RateDecision::Limited { retry_after_secs };
                    }
                }
                if let RateDecision::Limited { retry_after_secs } = Self::check_window(
                    &self.rate_endpoint,
                    endpoint_key,
                    settings.rate_limit_per_endpoint_requests,
                    settings.rate_limit_per_endpoint_window_secs,
                ) {
                    return RateDecision::Limited { retry_after_secs };
                }
                allowed
            }
        }
    }

    /// Every tracked rate-limit window, for the status endpoint.
    pub fn rate_limit_entries(&self) -> Vec<RateLimitEntry> {
        let settings = self.settings();
        let mut out: Vec<RateLimitEntry> = Vec::new();
        let mut collect = |scope: &str,
                           map: &DashMap<String, WindowBucket>,
                           limit: u32,
                           window_secs: u64| {
            for entry in map.iter() {
                let bucket = entry.value();
                let elapsed = bucket.window_start.elapsed().as_secs();
                out.push(RateLimitEntry {
                    scope: scope.to_string(),
                    key: entry.key().clone(),
                    limit,
                    count: bucket.count,
                    window_secs,
                    window_started_at: bucket.started_at,
                    reset_in_secs: window_secs.saturating_sub(elapsed).max(1),
                });
            }
        };
        collect(
            "ip",
            &self.rate_ip,
            settings.rate_limit_per_ip_requests,
            settings.rate_limit_per_ip_window_secs,
        );
        collect(
            "user",
            &self.rate_user,
            settings.rate_limit_per_user_requests,
            settings.rate_limit_per_user_window_secs,
        );
        collect(
            "endpoint",
            &self.rate_endpoint,
            settings.rate_limit_per_endpoint_requests,
            settings.rate_limit_per_endpoint_window_secs,
        );
        out.sort_by(|a, b| b.count.cmp(&a.count));
        out
    }

    // -- Flood detection and concurrent connections ----------------------

    /// Flood check: returns the seconds to wait when the IP sent more
    /// than `flood_burst_threshold` requests inside
    /// `flood_burst_window_secs`. On a burst the IP also gains
    /// reputation points and is auto-flagged at the configured
    /// threshold. The chosen behaviour is an immediate 429 with a
    /// `challenge_required` flag in the error metadata (implemented by
    /// the rate-limit middleware), not a held request: holding would
    /// occupy a worker for the whole hold and amplify the flood.
    pub fn check_burst(&self, ip: &str, settings: &SecuritySettings) -> Option<u64> {
        let now = Instant::now();
        let mut bucket = self.burst.entry(ip.to_string()).or_insert_with(|| BurstBucket {
            count: 0,
            window_start: now,
        });
        if bucket.window_start.elapsed().as_secs() >= settings.flood_burst_window_secs {
            bucket.count = 0;
            bucket.window_start = now;
        }
        bucket.count += 1;
        if bucket.count > settings.flood_burst_threshold {
            let elapsed = bucket.window_start.elapsed().as_secs();
            let retry_after = settings.flood_burst_window_secs.saturating_sub(elapsed).max(1);
            drop(bucket);
            self.add_reputation(
                ip,
                settings.ip_reputation_burst_penalty,
                Some("request flood burst".to_string()),
                settings,
            );
            return Some(retry_after);
        }
        None
    }

    /// Current in-flight request count for one IP, after incrementing.
    pub fn inc_concurrent(&self, ip: &str) -> u64 {
        let counter = self
            .concurrent
            .entry(ip.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .value()
            .clone();
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Decrement the in-flight request count for one IP. Saturating:
    /// a decrement without a matching increment (for example a slot
    /// released twice, or a counter left over from an older build)
    /// never wraps the counter to `u64::MAX`.
    pub fn dec_concurrent(&self, ip: &str) {
        if let Some(counter) = self.concurrent.get(ip) {
            let _ = counter.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                if value == 0 {
                    None
                } else {
                    Some(value - 1)
                }
            });
        }
    }
}

/// One held concurrent-connection slot. Dropping the slot releases
/// it, so a request that is dropped mid-flight (client disconnect)
/// can never leak a slot and permanently throttle its IP.
pub struct ConcurrentSlot {
    state: Arc<HardeningState>,
    ip: String,
}

impl ConcurrentSlot {
    /// Take one slot for `ip` and return the in-flight count after
    /// incrementing, with the guard that releases it on drop.
    pub fn acquire(state: &Arc<HardeningState>, ip: &str) -> (u64, Self) {
        let count = state.inc_concurrent(ip);
        (
            count,
            Self {
                state: Arc::clone(state),
                ip: ip.to_string(),
            },
        )
    }
}

impl Drop for ConcurrentSlot {
    fn drop(&mut self) {
        self.state.dec_concurrent(&self.ip);
    }
}
