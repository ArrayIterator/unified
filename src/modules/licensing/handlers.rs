//! Licensing - HTTP handlers for the core license endpoints.

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
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_licenses(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let mut items: Vec<License> = match db_pool(&ctx) {
            Some(pool) => db_load_licenses(&pool)?,
            None => licenses_store().iter().map(|entry| entry.value().clone()).collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.license_key.to_lowercase().contains(&needle)
                    || row.product.to_lowercase().contains(&needle)
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(product) = params.filters.get("product") {
            items.retain(|row| row.product == *product);
        }
        let sort = params.allowed_sort(&["product", "status", "created_at"], "created_at");
        match sort.as_str() {
            "product" => items.sort_by(|a, b| a.product.cmp(&b.product)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Validate a caller-supplied custom key: shape (an optional
/// product prefix plus `XXXX-XXXX-XXXX-XXXX`) and checksum. A key
/// with a bad checksum can never validate, so it is rejected here
/// instead of issuing a license that fails every later check.
pub(crate) fn validate_custom_license_key(key: &str, product: &str) -> UnifiedResult<String> {
    let key = normalize_key(key);
    let body = strip_product_prefix(&key, product);
    if body.len() != 19 || !body.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(UnifiedError::bad_request(
            "license_key must use the format XXXX-XXXX-XXXX-XXXX",
        ));
    }
    if !license_key_checksum_valid(&key, product) {
        return Err(UnifiedError::bad_request(
            "license_key checksum is invalid; a key with a bad checksum can never validate",
        ));
    }
    Ok(key)
}

pub(crate) async fn create_license(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateLicenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        if body.product.trim().is_empty() {
            return Err(UnifiedError::bad_request("product is required"));
        }
        if body.max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "max_activations must be at least 1",
            ));
        }
        let key = match &body.license_key {
            Some(custom) => validate_custom_license_key(custom, body.product.trim())?,
            None => generate_license_key(),
        };
        issue_license(
            &ctx,
            key,
            body.product.trim().to_string(),
            body.user_id,
            body.company_id,
            body.max_activations,
            body.expires_at,
        )
    })();
    match result {
        Ok(license) => json::success_response(license, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_license(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::licenses::dsl as lic_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            if let Some(row) = lic_dsl::licenses
                .filter(lic_dsl::id.eq(id))
                .select(License::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load license: {error}"))
                })?
            {
                return Ok(row);
            }
        }
        licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_license(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateLicenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        // Database first: the in-memory store may not hold a license
        // that only exists in the database (see `load_license_db_first`).
        let mut license = load_license_db_first(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if let Some(status) = &body.status {
            validate_status(status)?;
            // Terminal-state guard: a revoked license cannot be
            // resurrected and an expired one returns to active only
            // as a renewal (see `check_status_transition`).
            let effective_expires_at = match body.expires_at {
                Some(Some(expires_at)) => Some(expires_at),
                Some(None) => None,
                None => license.expires_at,
            };
            check_status_transition(
                &license.status,
                status,
                effective_expires_at,
                Utc::now(),
            )?;
            license.status = status.clone();
        }
        if let Some(product) = &body.product {
            if product.trim().is_empty() {
                return Err(UnifiedError::bad_request("product must not be empty"));
            }
            license.product = product.trim().to_string();
        }
        if let Some(max) = body.max_activations {
            if max < 1 {
                return Err(UnifiedError::bad_request(
                    "max_activations must be at least 1",
                ));
            }
            license.max_activations = max;
        }
        if let Some(expires_at) = body.expires_at {
            license.expires_at = expires_at;
        }
        license.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::licenses::dsl as lic_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = unified_core::db::schema::licenses)]
            struct LicenseChanges {
                product: Option<String>,
                max_activations: Option<i32>,
                status: Option<String>,
                expires_at: Option<Option<DateTime<Utc>>>,
            }
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(id)))
                .set(LicenseChanges {
                    product: body.product.clone(),
                    max_activations: body.max_activations,
                    status: body.status.clone(),
                    expires_at: body.expires_at,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update license: {error}"))
                })?;
        }
        licenses_store().insert(id, license.clone());
        Ok(license)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_license(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        // A license that only exists in the database still counts
        // as deleted: the row count, not the in-memory removal,
        // decides the outcome.
        let mut removed = false;
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::license_activations::dsl as act_dsl;
            use unified_core::db::schema::licenses::dsl as lic_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(act_dsl::license_activations.filter(act_dsl::license_id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete license activations: {error}"))
                })?;
            let deleted = diesel::delete(lic_dsl::licenses.filter(lic_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete license: {error}"))
                })?;
            removed = deleted > 0;
        }
        activations_store().retain(|_, activation| activation.license_id != id);
        if licenses_store().remove(&id).is_some() {
            removed = true;
        }
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("License not found"))
        }
    })();
    json::respond(result, debug)
}

pub(crate) async fn validate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ValidateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = validate_and_maybe_activate(
        &ctx,
        &body.key,
        body.instance_id.as_deref(),
        body.hostname.as_deref(),
        true,
    );
    json::respond(result, debug)
}

pub(crate) async fn activate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ActivationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let outcome = validate_and_maybe_activate(
            &ctx,
            &body.key,
            Some(&body.instance_id),
            body.hostname.as_deref(),
            true,
        )?;
        if !outcome.valid {
            let reason = outcome
                .reason
                .clone()
                .unwrap_or_else(|| "invalid".to_string());
            return Err(UnifiedError::unprocessable(format!(
                "License activation failed: {reason}"
            ))
            .with_metadata("reason", Value::String(outcome.reason.clone().unwrap_or_default())));
        }
        let license = outcome.license.clone().expect("validated license");
        let activations = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license.id)?,
            None => activations_for(license.id),
        };
        let activation = activations
            .into_iter()
            .find(|activation| activation.instance_id == body.instance_id);
        Ok(serde_json::json!({
            "license": license,
            "activation": activation,
            "activations_used": outcome.activations_used,
            "max_activations": outcome.max_activations,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn deactivate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ActivationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let key = normalize_key(&body.key);
        let license = match db_pool(&ctx) {
            Some(pool) => db_find_license_by_key(&pool, &key)?,
            None => find_license_by_key(&key),
        }
        .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        let mut removed = false;
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::license_activations::dsl as act_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            let count = diesel::delete(
                act_dsl::license_activations
                    .filter(act_dsl::license_id.eq(license.id))
                    .filter(act_dsl::instance_id.eq(&body.instance_id)),
            )
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot deactivate license: {error}"))
            })?;
            removed = count > 0;
        }
        activations_store().retain(|_, activation| {
            !(activation.license_id == license.id && activation.instance_id == body.instance_id)
                || {
                    removed = true;
                    false
                }
        });
        // The retain closure above both removes and records the outcome;
        // re-check the in-memory store when no pool was present.
        if db_pool(&ctx).is_none() {
            removed = !activations_store()
                .iter()
                .any(|entry| {
                    entry.value().license_id == license.id
                        && entry.value().instance_id == body.instance_id
                })
                && removed;
        }
        if !removed {
            return Err(UnifiedError::not_found(
                "No activation exists for this instance",
            ));
        }
        ctx.hooks.emit(
            "license.deactivated",
            serde_json::json!({
                "license_id": license.id,
                "instance_id": body.instance_id,
            }),
        );
        // Activation history + hash-chained event log (SaaS).
        record_activation_history(
            license.id,
            "deactivated",
            None,
            &body.instance_id,
            None,
        );
        append_license_event(
            &ctx,
            license.id,
            "deactivated",
            None,
            serde_json::json!({ "instance_id": body.instance_id }),
        );
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license.id)?.len(),
            None => activations_for(license.id).len(),
        };
        Ok(serde_json::json!({
            "license": license,
            "instance_id": body.instance_id,
            "activations_used": activations_used,
            "max_activations": license.max_activations,
        }))
    })();
    json::respond(result, debug)
}

/// Run the expiry scan now and return its summary. This is the
/// operator trigger for the scan until the scheduler gains a
/// license job kind (see `LICENSE_EXPIRY_CHECK_REQUESTED`).
pub(crate) async fn expiry_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        check_license_expiry(&ctx)
    })();
    json::respond(result, debug)
}

pub(crate) async fn suspend_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        // Database first: the in-memory store may not hold a license
        // that only exists in the database (see `load_license_db_first`).
        let mut license = load_license_db_first(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        // Terminal-state guard: only an active or expired license
        // can be suspended; suspending a revoked license would move
        // it out of its terminal state.
        check_suspend_allowed(&license.status)?;
        license.status = "suspended".to_string();
        license.updated_at = Utc::now();
        persist_license_status(&ctx, &license)?;
        ctx.hooks.emit(
            "license.suspended",
            serde_json::json!({ "license_id": license.id, "license_key": license.license_key }),
        );
        append_license_event(
            &ctx,
            license.id,
            "suspended",
            None,
            serde_json::json!({ "license_key": license.license_key }),
        );
        Ok(license)
    })();
    json::respond(result, debug)
}

pub(crate) async fn revoke_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        // Database first: the in-memory store may not hold a license
        // that only exists in the database (see `load_license_db_first`).
        // Revoking is idempotent: a revoked license stays revoked,
        // so no terminal-state guard is needed here.
        let mut license = load_license_db_first(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        license.status = "revoked".to_string();
        license.updated_at = Utc::now();
        persist_license_status(&ctx, &license)?;
        ctx.hooks.emit(
            events::LICENSE_REVOKED,
            serde_json::json!({ "license_id": license.id, "license_key": license.license_key }),
        );
        append_license_event(
            &ctx,
            license.id,
            "revoked",
            None,
            serde_json::json!({ "license_key": license.license_key }),
        );
        revoke_offline_tokens_for_license(license.id);
        Ok(license)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_activations(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let id = id.into_inner();
        let exists = match db_pool(&ctx) {
            Some(pool) => {
                use unified_core::db::schema::licenses::dsl as lic_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                lic_dsl::licenses
                    .filter(lic_dsl::id.eq(id))
                    .select(License::as_select())
                    .first(&mut conn)
                    .optional()
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load license: {error}"))
                    })?
                    .is_some()
            }
            None => licenses_store().contains_key(&id),
        };
        if !exists {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, id)?,
            None => activations_for(id),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.instance_id.to_lowercase().contains(&needle)
                    || row
                        .hostname
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_prefixed_product(slug: &str, prefix: &str) {
        let now = chrono::Utc::now();
        license_products_store().insert(
            uuid::Uuid::new_v4(),
            LicenseProduct {
                id: uuid::Uuid::new_v4(),
                product: slug.to_string(),
                display_name: None,
                owner_id: None,
                key_prefix: Some(prefix.to_string()),
                segments: 4,
                key_format: KEY_FORMAT_STANDARD.to_string(),
                key_pattern: None,
                default_max_activations: 1,
                default_validity_days: None,
                grace_period_days: 0,
                renewal_reminder_days: Vec::new(),
                offline_max_days: 0,
                allowed_distribution_channels: Vec::new(),
                api_allowed_endpoints: Vec::new(),
                api_rate_limit_per_minute: 60,
                created_at: now,
                updated_at: now,
            },
        );
    }

    #[test]
    fn validate_custom_license_key_accepts_a_valid_generated_key() {
        let key = generate_license_key();
        assert_eq!(
            validate_custom_license_key(&key, "no-such-product-xyz").expect("valid key"),
            key
        );
        // Lowercase input is normalised.
        assert_eq!(
            validate_custom_license_key(&key.to_lowercase(), "no-such-product-xyz")
                .expect("lowercase key"),
            key
        );
    }

    #[test]
    fn validate_custom_license_key_rejects_a_bad_checksum() {
        // A well-shaped key whose checksum does not match can never
        // validate, so it must be rejected at creation time.
        assert!(validate_custom_license_key("AAAAAAAAAAAAAAAB", "no-such-product-xyz").is_err());
        assert!(validate_custom_license_key("AAAA-AAAA-AAAA-AAAB", "no-such-product-xyz").is_err());
        // A generated key with its checksum character swapped fails.
        let mut key = generate_license_key();
        let last = key.chars().last().unwrap();
        let replacement = KEY_ALPHABET
            .iter()
            .map(|byte| *byte as char)
            .find(|c| *c != last)
            .unwrap();
        key.pop();
        key.push(replacement);
        assert!(!key_checksum_valid(&key));
        assert!(validate_custom_license_key(&key, "no-such-product-xyz").is_err());
    }

    #[test]
    fn validate_custom_license_key_rejects_a_bad_shape() {
        assert!(validate_custom_license_key("", "no-such-product-xyz").is_err());
        assert!(validate_custom_license_key("SHORT", "no-such-product-xyz").is_err());
        assert!(validate_custom_license_key(
            "AAAAAAAAAAAAAAAAAAA",
            "no-such-product-xyz"
        )
        .is_err());
        assert!(validate_custom_license_key(
            "AAAA AAAA AAAA AAAA",
            "no-such-product-xyz"
        )
        .is_err());
    }

    #[test]
    fn validate_custom_license_key_accepts_a_prefixed_product_key() {
        let slug = format!("prefixed-create-{}", uuid::Uuid::new_v4());
        insert_prefixed_product(&slug, "UNF");
        let body = generate_license_key();
        let key = format!("UNF-{body}");
        assert_eq!(
            validate_custom_license_key(&key, &slug).expect("prefixed key"),
            key
        );
        // The same body with a broken checksum is rejected, prefix or not.
        let mut bad = body.clone();
        let last = bad.chars().last().unwrap();
        let replacement = KEY_ALPHABET
            .iter()
            .map(|byte| *byte as char)
            .find(|c| *c != last)
            .unwrap();
        bad.pop();
        bad.push(replacement);
        assert!(!key_checksum_valid(&bad));
        let key = format!("UNF-{bad}");
        assert!(validate_custom_license_key(&key, &slug).is_err());
    }
}
