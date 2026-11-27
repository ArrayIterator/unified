//! Licensing - Hook listeners: license issue, validation, order created/refunded/upgraded.

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
use super::{keys::*, expiry::*, store::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

/// Create and store one license with an already-chosen key, and emit
/// `license.issued`. Shared by the HTTP create handler and the
/// `order.created` hook handler.
pub(crate) fn issue_license(
    ctx: &AppContext,
    key: String,
    product: String,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    max_activations: i32,
    expires_at: Option<DateTime<Utc>>,
) -> UnifiedResult<License> {
    let duplicate = match db_pool(ctx) {
        Some(pool) => db_find_license_by_key(&pool, &key)?.is_some(),
        None => find_license_by_key(&key).is_some(),
    };
    if duplicate {
        return Err(UnifiedError::conflict(
            "A license with this key already exists",
        ));
    }
    let now = Utc::now();
    let license = License {
        id: Uuid::now_v7(),
        license_key: key,
        product,
        user_id,
        company_id,
        max_activations,
        status: "active".to_string(),
        expires_at,
        created_at: now,
        updated_at: now,
    };
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::licenses::dsl as lic_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::insert_into(lic_dsl::licenses)
            .values(&NewLicense {
                id: license.id,
                license_key: license.license_key.clone(),
                product: license.product.clone(),
                user_id: license.user_id,
                company_id: license.company_id,
                max_activations: license.max_activations,
                status: license.status.clone(),
                expires_at: license.expires_at,
            })
            .execute(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot create license: {error}")))?;
    }
    licenses_store().insert(license.id, license.clone());
    ctx.hooks.emit(
        events::LICENSE_ISSUED,
        serde_json::json!({
            "license_id": license.id,
            "license_key": license.license_key,
            "product": license.product,
        }),
    );
    // Hash-chained event log (SaaS): issuing is the first entry of
    // the license chain.
    append_license_event(
        ctx,
        license.id,
        "issued",
        None,
        serde_json::json!({
            "license_key": license.license_key,
            "product": license.product,
            "max_activations": license.max_activations,
            "expires_at": license.expires_at,
        }),
    );
    Ok(license)
}

/// True when a license already exists for this product and owner
/// (in-memory store, plus the database when a pool is attached).
pub(crate) fn license_exists_for_product(
    ctx: &AppContext,
    product: &str,
    user_id: Option<Uuid>,
) -> bool {
    if licenses_store()
        .iter()
        .any(|entry| entry.value().product == product && entry.value().user_id == user_id)
    {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::licenses::dsl as lic_dsl;
        if let Ok(mut conn) = pool.get() {
            let mut query = lic_dsl::licenses
                .filter(lic_dsl::product.eq(product))
                .into_boxed();
            query = match user_id {
                Some(id) => query.filter(lic_dsl::user_id.eq(id)),
                None => query.filter(lic_dsl::user_id.is_null()),
            };
            if let Ok(count) = query
                .count()
                .get_result::<i64>(&mut conn)
            {
                return count > 0;
            }
        }
    }
    false
}

/// Answer `license.validate_requested` (from the Software module) with
/// the same validation the HTTP `/validate` endpoint performs: the key
/// must exist, pass its checksum, be `active`, not be expired, have a
/// free activation seat, and (when the payload names a product) belong
/// to that product.
pub(crate) fn on_license_validate_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "licensing module is not initialised",
        }));
        return;
    };
    let key = ctx
        .get("license_key")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if key.is_empty() {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "license_key is required",
        }));
        return;
    }
    let instance_id = ctx
        .get("instance_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let hostname = ctx
        .get("hostname")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let outcome = match validate_and_maybe_activate(
        &app_ctx,
        &key,
        instance_id.as_deref(),
        hostname.as_deref(),
        true,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            ctx.add_result(serde_json::json!({
                "valid": false,
                "message": error.message,
            }));
            return;
        }
    };
    let requested_product = ctx
        .get("product")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let product_matches = match (&requested_product, &outcome.license) {
        (Some(requested), Some(license)) => license.product == *requested,
        _ => true,
    };
    let mut result = serde_json::json!({
        "valid": outcome.valid && product_matches,
    });
    if let Some(license) = &outcome.license {
        result["license_id"] = serde_json::json!(license.id);
    }
    let message = if !outcome.valid {
        outcome.reason.clone()
    } else if !product_matches {
        Some("product_mismatch".to_string())
    } else {
        None
    };
    if let Some(message) = message {
        result["message"] = serde_json::json!(message);
    }
    ctx.add_result(result);
}

/// Auto-issue licenses for a new order (from the Store module). Only
/// items flagged `license_product` get a license, one per item, keyed by
/// the item name as the product; already-licensed product/owner pairs
/// are skipped so a repeated event does not issue duplicates. The seat
/// count comes from the item's `seats` field (times its quantity) and
/// the expiry from the item's `expires_at` field, both set by the store
/// from the purchased pricing tier and pricing model, so a software
/// purchase issues the right license: one-time purchases get the
/// updates-window expiry, subscriptions get one billing period.
pub(crate) fn on_order_created(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "issued": false,
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let items = ctx
        .get("items")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    for item in items {
        if item
            .get("license_product")
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            continue;
        }
        let product = item
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if product.is_empty() {
            continue;
        }
        if license_exists_for_product(&app_ctx, &product, user_id) {
            continue;
        }
        let quantity = item
            .get("quantity")
            .and_then(|value| value.as_u64())
            .unwrap_or(1)
            .max(1);
        let seats_per_unit = item
            .get("seats")
            .and_then(|value| value.as_i64())
            .unwrap_or(1)
            .max(1);
        let max_activations = (seats_per_unit * quantity as i64).min(i32::MAX as i64) as i32;
        let expires_at = item
            .get("expires_at")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<DateTime<Utc>>().ok());
        let software_id = item
            .get("software_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let software_slug = item
            .get("software_slug")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let download_limit = item
            .get("download_limit")
            .and_then(|value| value.as_u64());
        match issue_license(
            &app_ctx,
            generate_license_key(),
            product.clone(),
            user_id,
            company_id,
            max_activations,
            expires_at,
        ) {
            Ok(license) => {
                ctx.add_result(serde_json::json!({
                    "issued": true,
                    "order_id": order_id,
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "product": product,
                    "seats": max_activations,
                    "expires_at": license.expires_at,
                    "software_id": software_id,
                    "software_slug": software_slug,
                    "download_limit": download_limit,
                }));
            }
            Err(error) => {
                ctx.add_result(serde_json::json!({
                    "issued": false,
                    "order_id": order_id,
                    "product": product,
                    "error": error.message,
                }));
            }
        }
    }
}

/// Revoke the real licenses of a refunded order (from the Store
/// module, `store.order.refunded`, payload
/// `{ "order_id": ..., "license_ids": [...] }`).
///
/// The store only suspends its own mirror of the issued licenses and
/// revokes its download entitlements, so without this listener a
/// refunded license key would stay valid. A refund is final, so the
/// license is revoked rather than suspended: a revoked key fails
/// validation with `revoked` and can never activate again. Licenses
/// are resolved from the in-memory store first and from the database
/// when a pool is attached and the license is not in memory; ids that
/// match nothing are reported in `not_found` instead of failing the
/// whole refund. Each revocation emits `license.revoked`.
pub(crate) fn on_order_refunded(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "revoked": [],
            "not_found": [],
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let license_ids: Vec<String> = ctx
        .get("license_ids")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut revoked: Vec<String> = Vec::new();
    let mut not_found: Vec<String> = Vec::new();
    for license_id in license_ids {
        let Ok(id) = license_id.parse::<Uuid>() else {
            not_found.push(license_id);
            continue;
        };
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone());
        if license.is_none() {
            if let Some(pool) = db_pool(&app_ctx) {
                license = db_find_license_by_id(&pool, id).ok().flatten();
            }
        }
        let Some(mut license) = license else {
            not_found.push(license_id);
            continue;
        };
        if license.status == "revoked" {
            revoked.push(license_id);
            continue;
        }
        license.status = "revoked".to_string();
        license.updated_at = Utc::now();
        if persist_license_status(&app_ctx, &license).is_err() {
            not_found.push(license_id);
            continue;
        }
        app_ctx.hooks.emit(
            events::LICENSE_REVOKED,
            serde_json::json!({
                "license_id": license.id,
                "license_key": license.license_key,
                "order_id": order_id,
                "reason": "order_refunded",
            }),
        );
        // Refund coverage for the SaaS tables: the revocation is
        // chained in the event log, every offline token of this
        // license is revoked server-side (so online checks fail; an
        // already-issued offline verification still validates until
        // its token expires -- see the offline trust model), and the
        // activation history rows stay as an audit trail.
        append_license_event(
            &app_ctx,
            license.id,
            "revoked",
            None,
            serde_json::json!({
                "order_id": order_id,
                "reason": "order_refunded",
            }),
        );
        revoke_offline_tokens_for_license(license.id);
        revoked.push(license_id);
    }
    ctx.add_result(serde_json::json!({
        "revoked": revoked,
        "not_found": not_found,
        "order_id": order_id,
    }));
}

// SaaS hook handler: upgrades (store / billing plan changes).

/// Handle `store.order.upgraded` and `billing.plan.changed` by
/// adjusting the named license(s): extend the expiry by
/// `extend_days` (from the later of now / current expiry), set an
/// absolute `expires_at`, change `max_activations` (`seats` or
/// `new_max_activations`), or move the license to `new_product`.
/// Emits `license.upgraded` and chains an `upgraded` event per
/// license; expiry/grace/reminder alert state is cleared so the
/// scan re-alerts against the new expiry.
pub(crate) fn on_order_upgraded(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "upgraded": [],
            "not_found": [],
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut license_ids: Vec<String> = ctx
        .get("license_ids")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if license_ids.is_empty() {
        if let Some(id) = ctx
            .get("license_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        {
            license_ids.push(id);
        }
    }
    // An emitter that only names the order (the billing plan
    // change) resolves its licenses through the store, which is
    // the authority on which licenses an order issued.
    if license_ids.is_empty() {
        if let Some(order_id) = &order_id {
            let context = app_ctx.hooks.emit(
                "store.order.licenses",
                serde_json::json!({ "order_id": order_id }),
            );
            for result in &context.results {
                if let Some(ids) = result.get("license_ids").and_then(|value| value.as_array()) {
                    license_ids.extend(
                        ids.iter()
                            .filter_map(|value| value.as_str().map(str::to_string)),
                    );
                }
            }
        }
    }
    let extend_days = ctx
        .get("extend_days")
        .and_then(|value| value.as_i64())
        .or_else(|| {
            ctx.get("extend_validity_days")
                .and_then(|value| value.as_i64())
        });
    let new_max = ctx
        .get("new_max_activations")
        .and_then(|value| value.as_i64())
        .or_else(|| ctx.get("seats").and_then(|value| value.as_i64()));
    let new_product = ctx
        .get("new_product")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let new_expires_at = ctx
        .get("expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let mut upgraded: Vec<String> = Vec::new();
    let mut not_found: Vec<String> = Vec::new();
    for license_id in license_ids {
        let Ok(id) = license_id.parse::<Uuid>() else {
            not_found.push(license_id);
            continue;
        };
        let Some(mut license) = load_license(&app_ctx, id).ok().flatten() else {
            not_found.push(license_id);
            continue;
        };
        let now = Utc::now();
        if let Some(days) = extend_days {
            if days > 0 {
                let base = license.expires_at.filter(|exp| *exp > now).unwrap_or(now);
                license.expires_at = Some(base + chrono::Duration::days(days));
            }
        }
        if let Some(expires_at) = new_expires_at {
            license.expires_at = Some(expires_at);
        }
        if let Some(max) = new_max {
            if max >= 1 {
                license.max_activations = (max.min(i32::MAX as i64)) as i32;
            }
        }
        if let Some(product) = &new_product {
            if !product.trim().is_empty() {
                license.product = product.trim().to_string();
            }
        }
        // An upgraded license is usable again: a revoked license
        // stays revoked (a refund is final), but an expired license
        // with a new expiry returns to active.
        if license.status == "expired" && license.expires_at.is_some_and(|exp| exp > now) {
            license.status = "active".to_string();
        }
        // The reverse correction: an upgrade that sets an
        // already-past expiry must not leave the license `active`
        // - it is expired now, whatever it was before. A suspended
        // license stays suspended and a revoked one stays revoked;
        // only the live `active` state is corrected.
        if license.status == "active" && license.expires_at.is_some_and(|exp| exp <= now) {
            license.status = "expired".to_string();
        }
        license.updated_at = now;
        if persist_license(&app_ctx, &license).is_err() {
            not_found.push(license_id);
            continue;
        }
        expiry_alerted_store().remove(&id);
        reminder_alerted_store().remove(&id);
        grace_alerted_store().remove(&id);
        append_license_event(
            &app_ctx,
            id,
            "upgraded",
            None,
            serde_json::json!({
                "order_id": order_id,
                "extend_days": extend_days,
                "expires_at": license.expires_at,
                "max_activations": license.max_activations,
                "product": license.product,
            }),
        );
        app_ctx.hooks.emit(
            LICENSE_UPGRADED,
            serde_json::json!({
                "license_id": id,
                "order_id": order_id,
                "expires_at": license.expires_at,
                "max_activations": license.max_activations,
                "product": license.product,
            }),
        );
        upgraded.push(license_id);
    }
    ctx.add_result(serde_json::json!({
        "upgraded": upgraded,
        "not_found": not_found,
        "order_id": order_id,
    }));
}
