// hardening — state.
// Shared hardening state: construction, shared instance, overview.
//
// Split from `crates/unified-core/src/hardening.rs`; moved verbatim, no behaviour changes.

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

impl std::fmt::Debug for HardeningState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HardeningState")
    }
}

impl HardeningState {
    pub fn new(settings: SecuritySettings, hooks: Arc<HookBus>) -> Self {
        let state = Self {
            settings: RwLock::new(settings.clone()),
            hooks,
            brute_ip: DashMap::new(),
            brute_account: DashMap::new(),
            rate_ip: DashMap::new(),
            rate_user: DashMap::new(),
            rate_endpoint: DashMap::new(),
            burst: DashMap::new(),
            concurrent: DashMap::new(),
            allowlist: DashMap::new(),
            blocklist: DashMap::new(),
            reputation: DashMap::new(),
            failed_auth: DashMap::new(),
        };
        for entry in &settings.allowlist {
            if let Ok(range) = parse_cidr(entry) {
                state.allowlist.insert(
                    range.cidr.clone(),
                    IpListEntry {
                        list: "allow".to_string(),
                        cidr: range.cidr.clone(),
                        network: range.network.to_string(),
                        prefix: range.prefix,
                        added_at: Utc::now(),
                    },
                );
            } else {
                log::warn!("Ignoring invalid security allowlist entry \"{entry}\"");
            }
        }
        for entry in &settings.blocklist {
            if let Ok(range) = parse_cidr(entry) {
                state.blocklist.insert(
                    range.cidr.clone(),
                    IpListEntry {
                        list: "block".to_string(),
                        cidr: range.cidr.clone(),
                        network: range.network.to_string(),
                        prefix: range.prefix,
                        added_at: Utc::now(),
                    },
                );
            } else {
                log::warn!("Ignoring invalid security blocklist entry \"{entry}\"");
            }
        }
        state
    }

    /// Counts used by the settings overview endpoint.
    pub fn overview_counts(&self) -> HashMap<String, u64> {
        let now = Instant::now();
        let mut counts = HashMap::new();
        counts.insert("allowlist_entries".to_string(), self.allowlist.len() as u64);
        counts.insert("blocklist_entries".to_string(), self.blocklist.len() as u64);
        counts.insert(
            "locked_ips".to_string(),
            self.brute_ip
                .iter()
                .filter(|entry| entry.value().is_locked(now))
                .count() as u64,
        );
        counts.insert(
            "locked_accounts".to_string(),
            self.brute_account
                .iter()
                .filter(|entry| entry.value().is_locked(now))
                .count() as u64,
        );
        counts.insert(
            "flagged_ips".to_string(),
            self.reputation
                .iter()
                .filter(|entry| entry.value().flagged)
                .count() as u64,
        );
        counts.insert(
            "failed_auth_clusters".to_string(),
            self.failed_auth.len() as u64,
        );
        counts
    }
}

impl Default for HardeningState {
    fn default() -> Self {
        Self::new(SecuritySettings::default(), Arc::new(HookBus::new()))
    }
}

// ---------------------------------------------------------------------------
// Shared instance
// ---------------------------------------------------------------------------

static SHARED_STATE: RwLock<Option<Arc<HardeningState>>> = RwLock::new(None);

/// Install the shared hardening state, normally the config-derived one
/// built by the server at startup. Replaces any state installed before,
/// so a reload can swap in fresh settings.
pub fn install_shared_state(state: Arc<HardeningState>) {
    *SHARED_STATE
        .write()
        .expect("hardening shared state lock poisoned") = Some(state);
}

/// The shared hardening state: the installed one, or a lazily created
/// default when the server has not installed one yet.
pub fn shared_state() -> Arc<HardeningState> {
    if let Some(state) = SHARED_STATE
        .read()
        .expect("hardening shared state lock poisoned")
        .clone()
    {
        return state;
    }
    let state = Arc::new(HardeningState::default());
    *SHARED_STATE
        .write()
        .expect("hardening shared state lock poisoned") = Some(state.clone());
    state
}
