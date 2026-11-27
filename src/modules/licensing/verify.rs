//! Licensing - Public verification API, activation history, and the event log endpoints.

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
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, offline::*, distribution::*, releases::*, module::*};

// SaaS handlers: public verification API.

/// Public license verification for the product owner's own
/// applications: `POST /api/v1/licensing/public/verify`.
///
/// Authentication is an API key (`Authorization: Bearer <key>` or
/// `X-Api-Key`) scoped to `licensing:read` (or `licensing:*` / `*`)
/// and, when the product configures an `owner_id`, belonging to that
/// owner. The license key in the body is the credential being
/// verified, never the authentication credential. Successful
/// verification with a new `instance_id` auto-registers the
/// activation (the same seat behaviour as `/validate`). Failed
/// attempts are counted and emit `license.verify.failed`.
/// Rate limit: the key's own tier limit per minute (429 past it).
pub(crate) async fn public_verify_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<PublicVerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = public_verify(&ctx, &req, &body);
    json::respond(result, debug)
}

/// Synchronous core of `public_verify_handler`, extracted so tests
/// can drive it without an HTTP server.
///
/// The license is looked up and its product compared BEFORE any
/// validation with auto-registration: validating first and only
/// then reporting `product_mismatch` consumed a seat for a new
/// instance on a wrong-product verification. A mismatch now returns
/// before `validate_and_maybe_activate` runs, so no seat is ever
/// consumed for another product's license. (The mismatch branch
/// after validation below stays as a belt-and-braces check for a
/// product change racing between the lookup and the validation.)
pub(crate) fn public_verify(
    ctx: &AppContext,
    req: &HttpRequest,
    body: &PublicVerifyRequest,
) -> UnifiedResult<Value> {
    let authenticated = authenticate_public_key(req, "read")?;
    let product = body.product.trim().to_string();
    if product.is_empty() || body.license_key.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "license_key and product are required",
        ));
    }
    if body.instance_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("instance_id is required"));
    }
    if let Some(settings) = product_settings_for(&product) {
        if let Some(owner_id) = settings.owner_id {
            if authenticated.owner_id != Some(owner_id) {
                return Err(UnifiedError::forbidden(
                    "API key does not belong to this product owner",
                ));
            }
        }
    }
    let normalized = normalize_key(&body.license_key);
    let existing = match db_pool(ctx) {
        Some(pool) => db_find_license_by_key(&pool, &normalized)?,
        None => find_license_by_key(&normalized),
    };
    if let Some(license) = &existing {
        if license.product != product {
            record_failed_verification(
                ctx,
                &body.license_key,
                &product,
                "product_mismatch",
                Some(authenticated.id),
            );
            let activations_used = match db_pool(ctx) {
                Some(pool) => db_load_activations(&pool, license.id)?.len(),
                None => activations_for(license.id).len(),
            };
            return Ok(serde_json::json!({
                "valid": false,
                "reason": "product_mismatch",
                "license_id": license.id,
                "product": license.product,
                "activations_used": activations_used,
                "max_activations": license.max_activations,
                "expires_at": license.expires_at,
                "grace": false,
                "days_in_grace": 0,
                "offline": false,
            }));
        }
    }
    let outcome = validate_and_maybe_activate(
        ctx,
        &body.license_key,
        Some(body.instance_id.trim()),
        body.hostname.as_deref(),
        true,
    )?;
    let reason = match (&outcome.valid, &outcome.reason) {
        (true, _) => "valid".to_string(),
        (false, Some(reason)) => match reason.as_str() {
            "not_found" => "not_found".to_string(),
            "invalid_checksum" => "invalid".to_string(),
            "seats_exceeded" => "seats-exceeded".to_string(),
            other => other.to_string(),
        },
        (false, None) => "invalid".to_string(),
    };
    let Some(license) = &outcome.license else {
        record_failed_verification(
            ctx,
            &body.license_key,
            &product,
            &reason,
            Some(authenticated.id),
        );
        return Ok(serde_json::json!({
            "valid": false,
            "reason": reason,
            "license_id": Value::Null,
            "product": product,
            "activations_used": 0,
            "max_activations": 0,
            "expires_at": Value::Null,
            "grace": false,
            "days_in_grace": 0,
            "offline": false,
        }));
    };
    if license.product != product {
        record_failed_verification(
            ctx,
            &body.license_key,
            &product,
            "product_mismatch",
            Some(authenticated.id),
        );
        return Ok(serde_json::json!({
            "valid": false,
            "reason": "product_mismatch",
            "license_id": license.id,
            "product": license.product,
            "activations_used": outcome.activations_used,
            "max_activations": outcome.max_activations,
            "expires_at": license.expires_at,
            "grace": outcome.grace,
            "days_in_grace": outcome.days_in_grace,
            "offline": false,
        }));
    }
    if !outcome.valid {
        record_failed_verification(
            ctx,
            &body.license_key,
            &product,
            &reason,
            Some(authenticated.id),
        );
    }
    Ok(serde_json::json!({
        "valid": outcome.valid,
        "reason": reason,
        "license_id": license.id,
        "product": license.product,
        "activations_used": outcome.activations_used,
        "max_activations": outcome.max_activations,
        "expires_at": license.expires_at,
        "grace": outcome.grace,
        "days_in_grace": outcome.days_in_grace,
        "offline": false,
    }))
}

// SaaS handlers: activation lifecycle.

pub(crate) async fn list_activation_history(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items: Vec<LicenseActivationHistory> = license_activation_history_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.instance_id.to_lowercase().contains(&needle)
                    || row.action.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn deactivate_activation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        let caller = require_caller_capability(&ctx, &req, "licensing:manage")?;
        let (license_id, activation_id) = path.into_inner();
        let actor = caller.actor.clone();
        let license = load_license(&ctx, license_id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        let activation = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license_id)?
                .into_iter()
                .find(|row| row.id == activation_id),
            None => activations_store()
                .get(&activation_id)
                .map(|entry| entry.value().clone())
                .filter(|row| row.license_id == license_id),
        }
        .ok_or_else(|| UnifiedError::not_found("Activation not found for this license"))?;
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::license_activations::dsl as act_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(act_dsl::license_activations.filter(act_dsl::id.eq(activation_id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot deactivate license: {error}"))
                })?;
        }
        activations_store().remove(&activation_id);
        record_activation_history(
            license_id,
            "deactivated",
            Some(actor.as_str()),
            &activation.instance_id,
            activation.hostname.as_deref(),
        );
        append_license_event(
            &ctx,
            license_id,
            "deactivated",
            Some(actor.as_str()),
            serde_json::json!({
                "instance_id": activation.instance_id,
                "activation_id": activation_id,
                "remote": true,
            }),
        );
        ctx.hooks.emit(
            "license.deactivated",
            serde_json::json!({
                "license_id": license_id,
                "instance_id": activation.instance_id,
                "activation_id": activation_id,
                "remote": true,
            }),
        );
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license_id)?.len(),
            None => activations_for(license_id).len(),
        };
        Ok(serde_json::json!({
            "license": license,
            "activation_id": activation_id,
            "instance_id": activation.instance_id,
            "deactivated": true,
            "activations_used": activations_used,
            "max_activations": license.max_activations,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_seats(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateSeatsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_control_plane(&req)?;
        let caller = require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        let mut license = load_license(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if body.max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "max_activations must be at least 1",
            ));
        }
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, id)?.len(),
            None => activations_for(id).len(),
        };
        if body.max_activations < activations_used as i32 {
            return Err(UnifiedError::conflict(format!(
                "max_activations ({}) is below the {} activation(s) already in use; deactivate an instance first",
                body.max_activations, activations_used
            )));
        }
        let previous = license.max_activations;
        license.max_activations = body.max_activations;
        license.updated_at = Utc::now();
        persist_license(&ctx, &license)?;
        append_license_event(
            &ctx,
            id,
            "seats_changed",
            Some(caller.actor.as_str()),
            serde_json::json!({
                "previous_max_activations": previous,
                "max_activations": license.max_activations,
                "activations_used": activations_used,
            }),
        );
        ctx.hooks.emit(
            LICENSE_SEATS_CHANGED,
            serde_json::json!({
                "license_id": id,
                "previous_max_activations": previous,
                "max_activations": license.max_activations,
            }),
        );
        Ok(license)
    })();
    json::respond(result, debug)
}

// SaaS handlers: hash-chained event log.

pub(crate) async fn list_license_events(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items: Vec<LicenseEvent> = license_events_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.event.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn verify_license_events(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let (chain_valid, broken_at) = verify_event_chain(id);
        let count = license_events_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .count();
        Ok(serde_json::json!({
            "license_id": id,
            "events": count,
            "chain_valid": chain_valid,
            "broken_at": broken_at,
        }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::apikeys::{CreateApiKeyParams, create_api_key};
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_api_key() -> String {
        create_api_key(
            CreateApiKeyParams {
                name: "licensing-verify-test".to_string(),
                owner_id: None,
                scopes: Some(vec!["licensing:read".to_string()]),
                rate_tier: None,
                requests_per_minute: Some(1000),
                expires_in_days: None,
            },
            &["licensing".to_string()],
        )
        .expect("test API key")
        .full_key
    }

    fn test_request(api_key: &str) -> HttpRequest {
        TestRequest::default()
            .insert_header(("authorization", format!("Bearer {api_key}")))
            .to_http_request()
    }

    fn verify_body(product: &str, license_key: &str, instance_id: &str) -> PublicVerifyRequest {
        PublicVerifyRequest {
            license_key: license_key.to_string(),
            product: product.to_string(),
            instance_id: instance_id.to_string(),
            hostname: None,
        }
    }

    #[test]
    fn public_verify_wrong_product_consumes_no_seat() {
        let ctx = test_context();
        let api_key = test_api_key();
        let req = test_request(&api_key);
        let product = format!("verify-alpha-{}", Uuid::new_v4());
        let other_product = format!("verify-beta-{}", Uuid::new_v4());
        let license = issue_license(
            &ctx,
            generate_license_key(),
            product.clone(),
            None,
            None,
            1,
            None,
        )
        .expect("license");
        // Wrong product with a free seat: a mismatch, and nothing is consumed.
        let response = public_verify(
            &ctx,
            &req,
            &verify_body(&other_product, &license.license_key, "inst-1"),
        )
        .expect("verify");
        assert_eq!(response["valid"].as_bool(), Some(false));
        assert_eq!(response["reason"].as_str(), Some("product_mismatch"));
        assert_eq!(activations_for(license.id).len(), 0);
        // The right product still validates and consumes the one seat.
        let response = public_verify(
            &ctx,
            &req,
            &verify_body(&product, &license.license_key, "inst-1"),
        )
        .expect("verify");
        assert_eq!(response["valid"].as_bool(), Some(true));
        assert_eq!(response["activations_used"].as_u64(), Some(1));
        assert_eq!(activations_for(license.id).len(), 1);
        // A wrong-product verification after the seat is taken still
        // reports the mismatch (and cannot consume a second seat).
        let response = public_verify(
            &ctx,
            &req,
            &verify_body(&other_product, &license.license_key, "inst-2"),
        )
        .expect("verify");
        assert_eq!(response["reason"].as_str(), Some("product_mismatch"));
        assert_eq!(activations_for(license.id).len(), 1);
    }

    #[test]
    fn public_verify_unknown_key_reports_not_found() {
        let ctx = test_context();
        let api_key = test_api_key();
        let req = test_request(&api_key);
        let response = public_verify(
            &ctx,
            &req,
            &verify_body("some-product", &generate_license_key(), "inst-1"),
        )
        .expect("verify");
        assert_eq!(response["valid"].as_bool(), Some(false));
        assert_eq!(response["reason"].as_str(), Some("not_found"));
    }

    #[test]
    fn public_verify_rejects_a_missing_api_key() {
        let ctx = test_context();
        let req = TestRequest::default().to_http_request();
        let body = verify_body("some-product", &generate_license_key(), "inst-1");
        let error = public_verify(&ctx, &req, &body).expect_err("missing API key");
        assert_eq!(error.code, 401u16);
    }
}
