//! Licensing - Expiry scan: expiring-license alerts and the expiry-check hook.

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
use super::{keys::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// Expiry scan: `license.expiring`.
// ---------------------------------------------------------------------------

/// Event emitted when a license enters an expiry warning window (or
/// has already expired). Payload: `{"license_id", "license_key",
/// "days_remaining", "threshold_days", "expired", "expires_at"}`.
/// There is no `license.expiring` constant in `unified_core::hooks`
/// (it only defines `license.issued` / `license.revoked`), so the
/// name lives here next to the code that emits it. The notifications
/// module does NOT subscribe to this event yet (verified by reading
/// its listener list: it subscribes to `license.issued` /
/// `activated` / `deactivated` / `suspended` / `revoked` only), so
/// the scan also requests a notification directly
/// (`notification.requested`, user `system`), the same pattern the
/// certificates module uses for `ssl.expiring` (which the
/// notifications module likewise does not subscribe to). If the
/// notifications module later subscribes to `license.expiring`
/// directly, drop the direct request to avoid duplicate alerts.
pub const LICENSE_EXPIRING: &str = "license.expiring";

/// Hook an operator or the scheduler can emit to run the expiry scan
/// now. The scheduler has no license job kind yet (its known kinds
/// are mail, geo, ssl, cleanup, backup, cluster, subscription
/// renewal, report, newsletter, digest); until one is added there,
/// the scan is reachable through this hook and through
/// `POST /api/v1/licensing/licenses/expiry-check`.
pub const LICENSE_EXPIRY_CHECK_REQUESTED: &str = "license.expiry.check_requested";

/// Warning windows in days before expiry. There is no expiry-window
/// setting in `AppConfig` (the `licenses` section only carries the
/// MaxMind key), so the windows are these documented defaults: a
/// license is alerted once per window it newly enters, and once when
/// it expires.
pub const EXPIRY_ALERT_WINDOW_DAYS: &[i64] = &[30, 14, 7];

/// Thresholds already alerted for, per license id, so the scan
/// alerts once per window instead of on every run. Mirrors the
/// certificates module's `alerted_thresholds` (`-1` = expired).
pub(crate) static EXPIRY_ALERTED: OnceLock<DashMap<Uuid, Vec<i64>>> = OnceLock::new();

pub(crate) fn expiry_alerted_store() -> &'static DashMap<Uuid, Vec<i64>> {
    EXPIRY_ALERTED.get_or_init(DashMap::new)
}

/// Renewal-reminder windows already sent, per license id (SaaS).
/// Kept apart from `EXPIRY_ALERTED` so a renewal reminder and an
/// expiry alert for the same window are tracked independently.
pub(crate) static REMINDER_ALERTED: OnceLock<DashMap<Uuid, Vec<i64>>> = OnceLock::new();

pub(crate) fn reminder_alerted_store() -> &'static DashMap<Uuid, Vec<i64>> {
    REMINDER_ALERTED.get_or_init(DashMap::new)
}

/// Licenses for which `license.grace.started` was already emitted
/// (SaaS): the grace-started event fires once per grace period.
pub(crate) static GRACE_ALERTED: OnceLock<DashMap<Uuid, bool>> = OnceLock::new();

pub(crate) fn grace_alerted_store() -> &'static DashMap<Uuid, bool> {
    GRACE_ALERTED.get_or_init(DashMap::new)
}

/// Whole days from `now` until `expires_at`, floored toward
/// negative infinity: a license expired less than 24 hours ago is
/// -1 days (already expired), not 0. `chrono::Duration::num_days`
/// truncates toward zero, which made a just-expired license look
/// "expiring in 0 days" and cross all three warning windows
/// (30/14/7) in a single scan run, emitting three alerts and three
/// notifications for one expiry.
pub(crate) fn days_until_expiry(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (expires_at - now).num_seconds().div_euclid(86_400)
}

/// Warning windows (`30`/`14`/`7`, or `-1` for "already expired")
/// a license at `days` remaining newly crosses, given the windows
/// already alerted for it. A license expiring today crosses all
/// three windows at once on its first scan; an already-expired
/// license crosses `-1` once and never re-alerts.
pub(crate) fn newly_crossed_windows(days: i64, alerted: &[i64]) -> Vec<i64> {
    if days < 0 {
        if alerted.contains(&-1) {
            Vec::new()
        } else {
            vec![-1]
        }
    } else {
        EXPIRY_ALERT_WINDOW_DAYS
            .iter()
            .filter(|threshold| days <= **threshold && !alerted.contains(threshold))
            .copied()
            .collect()
    }
}

/// Run the expiry scan over every `active` license: for each warning
/// window (30/14/7 days) a license has newly entered, emit
/// `license.expiring` and request a notification; an already-expired
/// license alerts once the same way (`threshold_days: -1`,
/// `days_remaining` negative). Licenses with no expiry date and
/// non-`active` licenses are skipped. Returns a summary of what was
/// checked. The database rows are authoritative when a pool is
/// attached; otherwise the in-memory store is scanned.
pub fn check_license_expiry(ctx: &AppContext) -> UnifiedResult<Value> {
    let licenses: Vec<License> = match db_pool(ctx) {
        Some(pool) => db_load_licenses(&pool)?,
        None => licenses_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect(),
    };
    let now = Utc::now();
    let mut checked = 0u64;
    let mut expiring: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for license in &licenses {
        if license.status != "active" {
            continue;
        }
        let Some(expires_at) = license.expires_at else {
            continue;
        };
        checked += 1;
        let days = days_until_expiry(expires_at, now);
        let mut alerted = expiry_alerted_store()
            .get(&license.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut crossed: Vec<i64> = Vec::new();
        // Grace (SaaS): a license past expiry but inside its product
        // grace period is not expired yet -- emit `license.grace.started`
        // once and skip the expired alert until grace ends.
        let (in_grace, days_in_grace) = grace_state(license);
        if days < 0 {
            if in_grace {
                if !grace_alerted_store().contains_key(&license.id) {
                    grace_alerted_store().insert(license.id, true);
                    ctx.hooks.emit(
                        LICENSE_GRACE_STARTED,
                        serde_json::json!({
                            "license_id": license.id,
                            "license_key": license.license_key,
                            "days_in_grace": days_in_grace,
                            "grace_period_days": grace_days_for(&license.product),
                            "expires_at": expires_at,
                        }),
                    );
                    ctx.hooks.emit(
                        "notification.requested",
                        serde_json::json!({
                            "user_id": "system",
                            "title": format!(
                                "License {} is in its grace period",
                                license.license_key
                            ),
                            "body": format!(
                                "License {} expired on {} and is now in its grace period (day {days_in_grace}); it still validates until grace ends.",
                                license.license_key,
                                expires_at.to_rfc3339()
                            ),
                            "kind": "license.grace.started",
                        }),
                    );
                }
            } else {
                crossed.extend(newly_crossed_windows(days, &alerted));
                expired.push(serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "days_remaining": days,
                    "expires_at": expires_at,
                }));
            }
        } else {
            crossed.extend(newly_crossed_windows(days, &alerted));
            if !crossed.is_empty() {
                expiring.push(serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "days_remaining": days,
                    "expires_at": expires_at,
                    "threshold_days": &crossed,
                }));
            }
            // Renewal reminders (SaaS): per-product windows (default
            // 30/14/7/1 days), one `license.renewal.reminder` per
            // newly-entered window plus a direct notification.
            let reminder_windows = renewal_reminder_days_for(&license.product);
            let mut reminded = reminder_alerted_store()
                .get(&license.id)
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            for window in &reminder_windows {
                if days <= *window && !reminded.contains(window) {
                    reminded.push(*window);
                    ctx.hooks.emit(
                        LICENSE_RENEWAL_REMINDER,
                        serde_json::json!({
                            "license_id": license.id,
                            "license_key": license.license_key,
                            "product": license.product,
                            "days_remaining": days,
                            "threshold_days": window,
                            "expires_at": expires_at,
                        }),
                    );
                    ctx.hooks.emit(
                        "notification.requested",
                        serde_json::json!({
                            "user_id": "system",
                            "title": format!(
                                "Renewal reminder: license {} expires in {days} day(s)",
                                license.license_key
                            ),
                            "body": format!(
                                "License {} for product {} expires on {} (in {days} day(s)); renewal is due.",
                                license.license_key,
                                license.product,
                                expires_at.to_rfc3339()
                            ),
                            "kind": "license.renewal.reminder",
                        }),
                    );
                }
            }
            if !reminded.is_empty() {
                reminder_alerted_store().insert(license.id, reminded);
            }
        }
        for threshold in &crossed {
            alerted.push(*threshold);
            let expired_flag = *threshold == -1;
            ctx.hooks.emit(
                LICENSE_EXPIRING,
                serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "days_remaining": days,
                    "threshold_days": threshold,
                    "expired": expired_flag,
                    "expires_at": expires_at,
                }),
            );
            // Direct notification, same pattern as the certificates
            // module's `ssl.expiring` alerts: the notifications
            // module does not subscribe to this event, so without
            // this request the alert would land nowhere.
            let (title, body) = if expired_flag {
                (
                    format!("License {} has expired", license.license_key),
                    format!(
                        "License {} expired {} day(s) ago; it can no longer be validated until it is renewed or replaced.",
                        license.license_key,
                        -days
                    ),
                )
            } else {
                (
                    format!(
                        "License {} expires in {days} day(s)",
                        license.license_key
                    ),
                    format!(
                        "License {} expires on {} (in {days} day(s)); renewal is due within the warning window.",
                        license.license_key,
                        expires_at.to_rfc3339()
                    ),
                )
            };
            ctx.hooks.emit(
                "notification.requested",
                serde_json::json!({
                    "user_id": "system",
                    "title": title,
                    "body": body,
                    "kind": "license.expiring",
                }),
            );
        }
        if !crossed.is_empty() {
            expiry_alerted_store().insert(license.id, alerted);
        }
    }
    Ok(serde_json::json!({
        "checked": checked,
        "expiring": expiring,
        "expired": expired,
    }))
}

/// Answer `license.expiry.check_requested` by running the expiry
/// scan and returning its summary as the hook result.
pub(crate) fn on_license_expiry_check_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "checked": 0,
            "error": "licensing module is not initialised",
        }));
        return;
    };
    match check_license_expiry(&app_ctx) {
        Ok(summary) => ctx.add_result(summary),
        Err(error) => ctx.add_result(serde_json::json!({
            "checked": 0,
            "error": error.message,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_until_expiry_floors_instead_of_truncating() {
        let now = Utc::now();
        // A license expired less than 24 hours ago is -1 days, not 0:
        // the old `num_days` truncation made it look "expiring in 0
        // days" and cross all three warning windows in one run.
        assert_eq!(
            days_until_expiry(now - chrono::Duration::seconds(3_600), now),
            -1
        );
        assert_eq!(
            days_until_expiry(now - chrono::Duration::seconds(86_400), now),
            -1
        );
        assert_eq!(
            days_until_expiry(now - chrono::Duration::seconds(86_401), now),
            -2
        );
        assert_eq!(days_until_expiry(now, now), 0);
        // Expiring within the next 24 hours is 0 days, not negative.
        assert_eq!(
            days_until_expiry(now + chrono::Duration::seconds(3_600), now),
            0
        );
        assert_eq!(
            days_until_expiry(now + chrono::Duration::seconds(30 * 86_400 + 82_800), now),
            30
        );
        assert_eq!(
            days_until_expiry(now + chrono::Duration::seconds(30 * 86_400), now),
            30
        );
    }

    #[test]
    fn newly_crossed_windows_cases() {
        // Far out: nothing crosses.
        assert!(newly_crossed_windows(31, &[]).is_empty());
        // Entering the 30-day window.
        assert_eq!(newly_crossed_windows(30, &[]), vec![30]);
        assert_eq!(newly_crossed_windows(20, &[]), vec![30]);
        // Entering the 14-day window (30 already alerted).
        assert_eq!(newly_crossed_windows(14, &[30]), vec![14]);
        assert_eq!(newly_crossed_windows(10, &[30]), vec![14]);
        // Expiring today, first scan: all three windows at once.
        assert_eq!(newly_crossed_windows(0, &[]), vec![30, 14, 7]);
        // Already alerted windows never re-fire.
        assert!(newly_crossed_windows(5, &[30, 14, 7]).is_empty());
        // Already expired: the -1 window, once.
        assert_eq!(newly_crossed_windows(-1, &[]), vec![-1]);
        assert_eq!(newly_crossed_windows(-5, &[]), vec![-1]);
        assert!(newly_crossed_windows(-1, &[-1]).is_empty());
    }
}
