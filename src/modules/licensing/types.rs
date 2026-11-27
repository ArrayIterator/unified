//! Licensing - Request and response types for the core license endpoints.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, store::*, hooks::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// Request / response types.

#[derive(Debug, Deserialize)]
pub(crate) struct CreateLicenseRequest {
    pub(crate) product: String,
    #[serde(default)]
    pub(crate) license_key: Option<String>,
    #[serde(default)]
    pub(crate) user_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
    #[serde(default = "default_max_activations")]
    pub(crate) max_activations: i32,
    #[serde(default)]
    pub(crate) expires_at: Option<DateTime<Utc>>,
}

pub(crate) fn default_max_activations() -> i32 {
    1
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateLicenseRequest {
    #[serde(default)]
    pub(crate) product: Option<String>,
    #[serde(default)]
    pub(crate) max_activations: Option<i32>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) expires_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ValidateRequest {
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) instance_id: Option<String>,
    #[serde(default)]
    pub(crate) hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ActivationRequest {
    pub(crate) key: String,
    pub(crate) instance_id: String,
    #[serde(default)]
    pub(crate) hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ValidationResult {
    pub(crate) valid: bool,
    pub(crate) reason: Option<String>,
    pub(crate) license: Option<License>,
    pub(crate) activations_used: usize,
    pub(crate) max_activations: i32,
    /// True when the license is past `expires_at` but still inside
    /// its product grace period (see `grace_state`).
    pub(crate) grace: bool,
    /// Whole days elapsed since `expires_at` while in grace (0 when
    /// not in grace).
    pub(crate) days_in_grace: i64,
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn validate_status(status: &str) -> UnifiedResult<()> {
    match status {
        "active" | "suspended" | "revoked" | "expired" => Ok(()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown license status \"{other}\" (expected active, suspended, revoked, or expired)"
        ))),
    }
}

/// Terminal-state guard for license status changes (suspend,
/// revoke, and the license-update endpoint).
///
/// - `revoked` is terminal: no status change out of it is ever
///   allowed, so a revoked license cannot be resurrected.
/// - `expired` may move to `suspended` (the suspend endpoint
///   allows it) or to `revoked` (terminal to terminal), and back to
///   `active` only as a renewal: the effective expiry (the request's
///   new `expires_at`, or the license's own when the request does
///   not change it) must be in the future. That is the upgrade path
///   through this endpoint; the hook upgrade path
///   (`store.order.upgraded` / `billing.plan.changed`) applies the
///   same rule in `on_order_upgraded`.
/// - `active` and `suspended` may move to any known status.
/// - A request that keeps the current status is a no-op and always
///   allowed.
///
/// `new_expires_at` is the effective expiry after the requested
/// update (`None` when the license has, or is left with, no expiry).
/// Rejected transitions are 409 conflicts, not validation errors:
/// the request is well-formed, the state forbids it.
pub(crate) fn check_status_transition(
    current: &str,
    requested: &str,
    new_expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> UnifiedResult<()> {
    if requested == current {
        return Ok(());
    }
    match current {
        "revoked" => Err(UnifiedError::conflict(
            "License is revoked; its status is terminal and cannot be changed",
        )),
        "expired" => match requested {
            "revoked" | "suspended" => Ok(()),
            "active" => match new_expires_at {
                Some(expires_at) if expires_at > now => Ok(()),
                _ => Err(UnifiedError::conflict(
                    "An expired license can only return to active through a renewal: set expires_at to a future date, or use the upgrade path (store.order.upgraded / billing.plan.changed)",
                )),
            },
            _ => Err(UnifiedError::conflict(format!(
                "Unknown target license status \"{requested}\""
            ))),
        },
        "active" | "suspended" => Ok(()),
        other => Err(UnifiedError::conflict(format!(
            "License status \"{other}\" cannot be changed"
        ))),
    }
}

/// Suspend guard: only an `active` or `expired` license can be
/// suspended. A `suspended` license is already suspended and a
/// `revoked` license is terminal (suspending it would move it out
/// of the terminal state).
pub(crate) fn check_suspend_allowed(current: &str) -> UnifiedResult<()> {
    match current {
        "active" | "expired" => Ok(()),
        "suspended" => Err(UnifiedError::conflict(
            "License is already suspended",
        )),
        "revoked" => Err(UnifiedError::conflict(
            "License is revoked; a revoked license cannot be suspended",
        )),
        other => Err(UnifiedError::conflict(format!(
            "License status \"{other}\" cannot be suspended"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_status_accepts_known_statuses() {
        for status in ["active", "suspended", "revoked", "expired"] {
            assert!(validate_status(status).is_ok(), "{status} should be valid");
        }
    }

    #[test]
    fn validate_status_rejects_unknown_statuses() {
        assert!(validate_status("pending").is_err());
        assert!(validate_status("").is_err());
        assert!(validate_status("ACTIVE").is_err());
    }

    #[test]
    fn check_status_transition_revoked_is_terminal() {
        let now = chrono::Utc::now();
        let future = now + chrono::Duration::days(30);
        // Even a renewal cannot resurrect a revoked license.
        for requested in ["active", "suspended", "expired"] {
            let error = check_status_transition("revoked", requested, Some(future), now)
                .expect_err("revoked must reject every transition");
            assert_eq!(error.code, 409u16);
        }
        // Keeping the status is a no-op, not a transition.
        assert!(check_status_transition("revoked", "revoked", None, now).is_ok());
    }

    #[test]
    fn check_status_transition_expired_needs_a_renewal_to_go_active() {
        let now = chrono::Utc::now();
        let future = now + chrono::Duration::days(30);
        let past = now - chrono::Duration::days(1);
        // No new expiry (or a past one): resurrection is rejected.
        for expiry in [None, Some(past), Some(now)] {
            let error = check_status_transition("expired", "active", expiry, now)
                .expect_err("expired must not return to active without a future expiry");
            assert_eq!(error.code, 409u16);
        }
        // A renewal (future expiry) is the upgrade path: allowed.
        assert!(check_status_transition("expired", "active", Some(future), now).is_ok());
        // Terminal-to-terminal and suspend are allowed.
        assert!(check_status_transition("expired", "revoked", None, now).is_ok());
        assert!(check_status_transition("expired", "suspended", None, now).is_ok());
        assert!(check_status_transition("expired", "expired", None, now).is_ok());
    }

    #[test]
    fn check_status_transition_active_and_suspended_are_free() {
        let now = chrono::Utc::now();
        for current in ["active", "suspended"] {
            for requested in ["active", "suspended", "revoked", "expired"] {
                assert!(
                    check_status_transition(current, requested, None, now).is_ok(),
                    "{current} -> {requested} should be allowed"
                );
            }
        }
    }

    #[test]
    fn check_suspend_allowed_cases() {
        assert!(check_suspend_allowed("active").is_ok());
        assert!(check_suspend_allowed("expired").is_ok());
        for status in ["suspended", "revoked", "pending"] {
            let error =
                check_suspend_allowed(status).expect_err("suspend must be rejected");
            assert_eq!(error.code, 409u16);
        }
    }
}
