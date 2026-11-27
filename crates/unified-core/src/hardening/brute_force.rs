// hardening - brute_force.
// Login-failure tracking, lockouts, and failed-auth clusters.
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

// State records

#[derive(Debug, Clone)]
pub(crate) struct AttemptState {
    failures: u32,
    window_start: Instant,
    locked_until: Option<Instant>,
    lock_count: u32,
    last_failure: DateTime<Utc>,
}

impl AttemptState {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            window_start: now,
            locked_until: None,
            lock_count: 0,
            last_failure: Utc::now(),
        }
    }

    pub(crate) fn is_locked(&self, now: Instant) -> bool {
        self.locked_until.is_some_and(|until| until > now)
    }

    fn locked_until_wall(&self) -> Option<DateTime<Utc>> {
        self.locked_until.map(|until| {
            let remaining = until.saturating_duration_since(Instant::now());
            Utc::now() + chrono::Duration::from_std(remaining).unwrap_or(chrono::Duration::zero())
        })
    }
}

/// One lockout / attempt status, as returned by the status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct LockoutStatus {
    pub kind: String,
    pub key: String,
    pub failures: u32,
    pub locked: bool,
    pub locked_until: Option<DateTime<Utc>>,
    pub lock_count: u32,
    pub last_failure: DateTime<Utc>,
}

/// One failed-auth cluster: repeated failures grouped by IP + account.
#[derive(Debug, Clone, Serialize)]
pub struct FailedAuthCluster {
    pub key: String,
    pub ip: Option<String>,
    pub account: Option<String>,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub locked: bool,
}

impl HardeningState {
    // -- Brute force ---------------------------------------------------

    /// Record one failed login for an IP and/or an account. Applies
    /// progressive lockout and emits `security.bruteforce.locked` when
    /// a lockout starts. Also feeds the failed-auth cluster tracker so
    /// repeated failures raise `security.auth.failed` alerts.
    pub fn record_login_failure(&self, ip: Option<&str>, account: Option<&str>) {
        let settings = self.settings();
        if settings.brute_force_enabled {
            if let Some(ip) = ip.filter(|value| !value.is_empty()) {
                self.record_failure(
                    &self.brute_ip,
                    ip,
                    "ip",
                    settings.brute_force_max_failures,
                    &settings,
                );
            }
            if let Some(account) = account.filter(|value| !value.is_empty()) {
                self.record_failure(
                    &self.brute_account,
                    account,
                    "account",
                    settings.brute_force_account_max_failures,
                    &settings,
                );
            }
        }
        self.record_failed_auth_cluster(ip, account, &settings);
    }

    fn record_failure(
        &self,
        map: &DashMap<String, AttemptState>,
        key: &str,
        kind: &str,
        max_failures: u32,
        settings: &SecuritySettings,
    ) {
        let now = Instant::now();
        // Decide the lockout while the entry guard is held, then drop
        // the guard before emitting: a hook listener may query this
        // same map, and emitting under the write guard could deadlock.
        let locked_event: Option<(u32, u64, Option<DateTime<Utc>>)> = {
            let mut entry = map
                .entry(key.to_string())
                .or_insert_with(|| AttemptState::new(now));
            if entry.is_locked(now) {
                return;
            }
            if entry.window_start.elapsed().as_secs() >= settings.brute_force_window_secs {
                entry.failures = 0;
                entry.window_start = now;
                entry.locked_until = None;
            }
            entry.failures += 1;
            entry.last_failure = Utc::now();
            if entry.failures >= max_failures {
                entry.lock_count += 1;
                let multiplier = settings
                    .brute_force_lockout_multiplier
                    .checked_pow(entry.lock_count.saturating_sub(1))
                    .unwrap_or(u32::MAX);
                let duration_secs = settings
                    .brute_force_base_lockout_secs
                    .saturating_mul(u64::from(multiplier))
                    .min(settings.brute_force_max_lockout_secs);
                entry.locked_until = Some(now + Duration::from_secs(duration_secs));
                entry.failures = 0;
                let locked_until = entry.locked_until_wall();
                Some((entry.lock_count, duration_secs, locked_until))
            } else {
                None
            }
        };
        if let Some((lock_count, duration_secs, locked_until)) = locked_event {
            self.hooks.emit(
                security_events::BRUTEFORCE_LOCKED,
                serde_json::json!({
                    "kind": kind,
                    "key": key,
                    "lock_count": lock_count,
                    "lockout_secs": duration_secs,
                    "locked_until": locked_until,
                }),
            );
        }
    }

    /// A successful login resets the brute-force counters for the IP
    /// and the account, so a legitimate user is never locked out by
    /// earlier failures.
    pub fn record_login_success(&self, ip: Option<&str>, account: Option<&str>) {
        if let Some(ip) = ip.filter(|value| !value.is_empty()) {
            self.brute_ip.remove(ip);
        }
        if let Some(account) = account.filter(|value| !value.is_empty()) {
            self.brute_account.remove(account);
        }
    }

    /// True when the IP or the account is currently locked out.
    pub fn is_locked(&self, kind: &str, key: &str) -> bool {
        let now = Instant::now();
        let map = if kind == "account" {
            &self.brute_account
        } else {
            &self.brute_ip
        };
        map.get(key).is_some_and(|entry| entry.is_locked(now))
    }

    /// Every lockout / attempt status, most recently failed first.
    pub fn lockout_statuses(&self) -> Vec<LockoutStatus> {
        let now = Instant::now();
        let mut out: Vec<LockoutStatus> = Vec::new();
        for entry in self.brute_ip.iter() {
            out.push(LockoutStatus {
                kind: "ip".to_string(),
                key: entry.key().clone(),
                failures: entry.value().failures,
                locked: entry.value().is_locked(now),
                locked_until: entry.value().locked_until_wall(),
                lock_count: entry.value().lock_count,
                last_failure: entry.value().last_failure,
            });
        }
        for entry in self.brute_account.iter() {
            out.push(LockoutStatus {
                kind: "account".to_string(),
                key: entry.key().clone(),
                failures: entry.value().failures,
                locked: entry.value().is_locked(now),
                locked_until: entry.value().locked_until_wall(),
                lock_count: entry.value().lock_count,
                last_failure: entry.value().last_failure,
            });
        }
        out.sort_by(|a, b| b.last_failure.cmp(&a.last_failure));
        out
    }

    /// Manually clear a lockout / attempt record. Returns true when a
    /// record existed. Emits `security.bruteforce.unlocked`.
    pub fn unlock(&self, kind: &str, key: &str) -> bool {
        let removed = if kind == "account" {
            self.brute_account.remove(key).is_some()
        } else {
            self.brute_ip.remove(key).is_some()
        };
        if removed {
            self.hooks.emit(
                security_events::BRUTEFORCE_UNLOCKED,
                serde_json::json!({ "kind": kind, "key": key }),
            );
        }
        removed
    }

    // -- Failed-auth clusters ------------------------------------------

    fn record_failed_auth_cluster(
        &self,
        ip: Option<&str>,
        account: Option<&str>,
        settings: &SecuritySettings,
    ) {
        let ip = ip.filter(|value| !value.is_empty());
        let account = account.filter(|value| !value.is_empty());
        if ip.is_none() && account.is_none() {
            return;
        }
        let key = format!(
            "{}|{}",
            ip.unwrap_or("-"),
            account.unwrap_or("-")
        );
        let now = Utc::now();
        // Update the cluster under the entry guard, then drop the guard
        // before emitting: a hook listener may query the clusters.
        let (count, threshold) = {
            let mut cluster = self.failed_auth.entry(key.clone()).or_insert_with(|| {
                FailedAuthCluster {
                    key: key.clone(),
                    ip: ip.map(str::to_string),
                    account: account.map(str::to_string),
                    count: 0,
                    first_seen: now,
                    last_seen: now,
                    locked: false,
                }
            });
            if now
                .signed_duration_since(cluster.first_seen)
                .num_seconds()
                .unsigned_abs()
                >= settings.failed_auth_alert_window_secs
            {
                cluster.count = 0;
                cluster.first_seen = now;
            }
            cluster.count += 1;
            cluster.last_seen = now;
            cluster.locked = ip.is_some_and(|value| self.is_locked("ip", value))
                || account.is_some_and(|value| self.is_locked("account", value));
            (
                cluster.count,
                u64::from(settings.failed_auth_alert_threshold),
            )
        };
        if threshold > 0 && count % threshold == 0 {
            self.hooks.emit(
                security_events::AUTH_FAILED,
                serde_json::json!({
                    "key": key,
                    "ip": ip,
                    "account": account,
                    "count": count,
                    "threshold": threshold,
                }),
            );
        }
    }

    /// Failed-auth clusters, most recently active first.
    pub fn failed_auth_clusters(&self) -> Vec<FailedAuthCluster> {
        let mut clusters: Vec<FailedAuthCluster> = self
            .failed_auth
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        clusters.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        clusters
    }
}
