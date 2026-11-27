// hardening - reputation.
// IP reputation scores and flagging.
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

/// One IP reputation record, as returned by the reputation endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ReputationEntry {
    pub ip: String,
    pub score: i32,
    pub flagged: bool,
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl HardeningState {
    // -- IP reputation ---------------------------------------------------

    /// Add reputation points to an IP. At the configured threshold the
    /// IP is auto-flagged and `security.ip.flagged` is emitted (once,
    /// on the transition to flagged).
    pub fn add_reputation(
        &self,
        ip: &str,
        points: i32,
        reason: Option<String>,
        settings: &SecuritySettings,
    ) {
        // Update under the entry guard, then drop it before emitting:
        // a hook listener may query the reputation map.
        let flag_event: Option<(i32, Option<String>)> = {
            let mut entry = self.reputation.entry(ip.to_string()).or_insert_with(|| {
                ReputationEntry {
                    ip: ip.to_string(),
                    score: 0,
                    flagged: false,
                    reason: None,
                    updated_at: Utc::now(),
                }
            });
            entry.score = entry.score.saturating_add(points);
            entry.updated_at = Utc::now();
            if let Some(reason) = reason {
                entry.reason = Some(reason.clone());
            }
            if !entry.flagged && entry.score >= settings.ip_reputation_flag_threshold {
                entry.flagged = true;
                Some((entry.score, entry.reason.clone()))
            } else {
                None
            }
        };
        if let Some((score, reason)) = flag_event {
            self.hooks.emit(
                security_events::IP_FLAGGED,
                serde_json::json!({
                    "ip": ip,
                    "score": score,
                    "reason": reason,
                    "automatic": true,
                }),
            );
        }
    }

    /// Manually flag an IP with a reason. Emits `security.ip.flagged`.
    /// The score is raised to at least the flag threshold so the flag
    /// survives later score changes.
    pub fn flag_ip(&self, ip: &str, reason: Option<String>) -> Result<ReputationEntry, String> {
        let parsed: IpAddr = ip
            .trim()
            .parse()
            .map_err(|_| format!("\"{ip}\" is not a valid IP address"))?;
        let settings = self.settings();
        let snapshot = {
            let mut entry = self
                .reputation
                .entry(parsed.to_string())
                .or_insert_with(|| ReputationEntry {
                    ip: parsed.to_string(),
                    score: 0,
                    flagged: false,
                    reason: None,
                    updated_at: Utc::now(),
                });
            entry.score = entry.score.max(settings.ip_reputation_flag_threshold);
            entry.flagged = true;
            entry.reason = reason.clone();
            entry.updated_at = Utc::now();
            entry.clone()
        };
        self.hooks.emit(
            security_events::IP_FLAGGED,
            serde_json::json!({
                "ip": parsed.to_string(),
                "score": snapshot.score,
                "reason": reason,
                "automatic": false,
            }),
        );
        Ok(snapshot)
    }

    /// Manually unflag an IP and reset its score. Emits
    /// `security.ip.unflagged`.
    pub fn unflag_ip(&self, ip: &str) -> Result<ReputationEntry, String> {
        let parsed: IpAddr = ip
            .trim()
            .parse()
            .map_err(|_| format!("\"{ip}\" is not a valid IP address"))?;
        let snapshot = {
            let mut entry = self
                .reputation
                .entry(parsed.to_string())
                .or_insert_with(|| ReputationEntry {
                    ip: parsed.to_string(),
                    score: 0,
                    flagged: false,
                    reason: None,
                    updated_at: Utc::now(),
                });
            entry.flagged = false;
            entry.score = 0;
            entry.reason = None;
            entry.updated_at = Utc::now();
            entry.clone()
        };
        self.hooks.emit(
            security_events::IP_UNFLAGGED,
            serde_json::json!({ "ip": parsed.to_string() }),
        );
        Ok(snapshot)
    }

    /// Reputation records, highest score first.
    pub fn reputation_entries(&self) -> Vec<ReputationEntry> {
        let mut entries: Vec<ReputationEntry> = self
            .reputation
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        entries.sort_by(|a, b| b.score.cmp(&a.score));
        entries
    }
}
