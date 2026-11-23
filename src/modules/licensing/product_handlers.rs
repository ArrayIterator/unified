//! Licensing - HTTP handlers for license products.

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
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// SaaS request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateLicenseProductRequest {
    pub(crate) product: String,
    #[serde(default)]
    pub(crate) display_name: Option<String>,
    #[serde(default)]
    pub(crate) owner_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) key_prefix: Option<String>,
    #[serde(default = "default_product_segments")]
    pub(crate) segments: i32,
    #[serde(default = "default_key_format")]
    pub(crate) key_format: String,
    #[serde(default)]
    pub(crate) key_pattern: Option<String>,
    #[serde(default = "default_product_max_activations")]
    pub(crate) default_max_activations: i32,
    #[serde(default)]
    pub(crate) default_validity_days: Option<i64>,
    #[serde(default)]
    pub(crate) grace_period_days: i64,
    #[serde(default)]
    pub(crate) renewal_reminder_days: Option<Vec<i64>>,
    #[serde(default = "default_offline_max_days_fn")]
    pub(crate) offline_max_days: i64,
    #[serde(default)]
    pub(crate) allowed_distribution_channels: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) api_allowed_endpoints: Option<Vec<String>>,
    #[serde(default = "default_api_rate_limit_fn")]
    pub(crate) api_rate_limit_per_minute: i64,
}

pub(crate) fn default_product_segments() -> i32 {
    4
}

pub(crate) fn default_key_format() -> String {
    KEY_FORMAT_STANDARD.to_string()
}

pub(crate) fn default_product_max_activations() -> i32 {
    1
}

pub(crate) fn default_offline_max_days_fn() -> i64 {
    DEFAULT_OFFLINE_MAX_DAYS
}

pub(crate) fn default_api_rate_limit_fn() -> i64 {
    60
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateLicenseProductRequest {
    #[serde(default)]
    pub(crate) display_name: Option<Option<String>>,
    #[serde(default)]
    pub(crate) owner_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub(crate) key_prefix: Option<Option<String>>,
    #[serde(default)]
    pub(crate) segments: Option<i32>,
    #[serde(default)]
    pub(crate) key_format: Option<String>,
    #[serde(default)]
    pub(crate) key_pattern: Option<Option<String>>,
    #[serde(default)]
    pub(crate) default_max_activations: Option<i32>,
    #[serde(default)]
    pub(crate) default_validity_days: Option<Option<i64>>,
    #[serde(default)]
    pub(crate) grace_period_days: Option<i64>,
    #[serde(default)]
    pub(crate) renewal_reminder_days: Option<Vec<i64>>,
    #[serde(default)]
    pub(crate) offline_max_days: Option<i64>,
    #[serde(default)]
    pub(crate) allowed_distribution_channels: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) api_allowed_endpoints: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) api_rate_limit_per_minute: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PublicVerifyRequest {
    pub(crate) license_key: String,
    pub(crate) product: String,
    pub(crate) instance_id: String,
    #[serde(default)]
    pub(crate) hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSeatsRequest {
    pub(crate) max_activations: i32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueOfflineTokenRequest {
    #[serde(default)]
    pub(crate) max_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OfflineVerifyRequest {
    pub(crate) token: String,
    #[serde(default)]
    pub(crate) instance_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateDistributionRequest {
    pub(crate) product: String,
    pub(crate) provider: String,
    pub(crate) repo: String,
    #[serde(default)]
    pub(crate) git_ref: Option<String>,
    #[serde(default)]
    pub(crate) webhook_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateDistributionRequest {
    #[serde(default)]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[serde(default)]
    pub(crate) git_ref: Option<String>,
    #[serde(default)]
    pub(crate) webhook_secret: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GitAccessQuery {
    pub(crate) license_key: String,
    pub(crate) instance_id: String,
    #[serde(default)]
    pub(crate) hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DownloadQuery {
    pub(crate) license_key: String,
    pub(crate) instance_id: String,
    #[serde(default)]
    pub(crate) hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SignedFileQuery {
    pub(crate) license_id: String,
    pub(crate) expires: i64,
    pub(crate) sig: String,
}

pub(crate) fn validate_product_slug(slug: &str) -> UnifiedResult<String> {
    let slug = slug.trim().to_lowercase();
    if slug.is_empty() {
        return Err(UnifiedError::bad_request("product slug is required"));
    }
    let valid = slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && slug
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !valid {
        return Err(UnifiedError::bad_request(
            "product slug must be lowercase letters, digits, '-' or '_', up to 64 characters",
        ));
    }
    Ok(slug)
}

pub(crate) fn validate_key_format(format: &str, pattern: &Option<String>) -> UnifiedResult<()> {
    match format {
        KEY_FORMAT_STANDARD => Ok(()),
        KEY_FORMAT_CUSTOM => {
            let Some(pattern) = pattern else {
                return Err(UnifiedError::bad_request(
                    "Custom key format requires a key_pattern regex",
                ));
            };
            Regex::new(pattern).map_err(|error| {
                UnifiedError::bad_request(format!("Invalid key_pattern regex: {error}"))
            })?;
            Ok(())
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown key_format \"{other}\" (expected standard or custom)"
        ))),
    }
}

pub(crate) fn validate_channels(channels: &[String]) -> UnifiedResult<()> {
    for channel in channels {
        match channel.as_str() {
            DISTRIBUTION_CHANNEL_GIT | DISTRIBUTION_CHANNEL_API | DISTRIBUTION_CHANNEL_FILE => {}
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown distribution channel \"{other}\" (expected git, api, or file)"
                )))
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_prefix(prefix: &Option<String>) -> UnifiedResult<Option<String>> {
    let Some(prefix) = prefix else {
        return Ok(None);
    };
    let prefix = prefix.trim().to_uppercase();
    if prefix.is_empty() {
        return Ok(None);
    }
    if prefix.len() > 8
        || !prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Err(UnifiedError::bad_request(
            "key_prefix must be 1-8 uppercase letters or digits",
        ));
    }
    Ok(Some(prefix))
}

// ---------------------------------------------------------------------------
// SaaS handlers: license products.
// ---------------------------------------------------------------------------

pub(crate) async fn list_license_products(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let mut items: Vec<LicenseProduct> = license_products_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.product.to_lowercase().contains(&needle)
                    || row
                        .display_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        let sort = params.allowed_sort(&["product", "created_at"], "created_at");
        match sort.as_str() {
            "product" => items.sort_by(|a, b| a.product.cmp(&b.product)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_license_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateLicenseProductRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseProduct> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let product = validate_product_slug(&body.product)?;
        if product_settings_for(&product).is_some() {
            return Err(UnifiedError::conflict(format!(
                "A license product \"{product}\" already exists"
            )));
        }
        validate_key_format(&body.key_format, &body.key_pattern)?;
        let key_prefix = validate_prefix(&body.key_prefix)?;
        let segments = body.segments.clamp(1, 8);
        if body.default_max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "default_max_activations must be at least 1",
            ));
        }
        if body.grace_period_days < 0 || body.grace_period_days > 365 {
            return Err(UnifiedError::bad_request(
                "grace_period_days must be between 0 and 365",
            ));
        }
        if body.offline_max_days < 1 || body.offline_max_days > MAX_OFFLINE_TOKEN_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "offline_max_days must be between 1 and {MAX_OFFLINE_TOKEN_DAYS}"
            )));
        }
        let reminder_days = body
            .renewal_reminder_days
            .clone()
            .unwrap_or_else(|| DEFAULT_RENEWAL_REMINDER_DAYS.to_vec());
        for window in &reminder_days {
            if *window < 0 || *window > 365 {
                return Err(UnifiedError::bad_request(
                    "renewal_reminder_days entries must be between 0 and 365",
                ));
            }
        }
        let channels = body
            .allowed_distribution_channels
            .clone()
            .unwrap_or_else(|| {
                vec![
                    DISTRIBUTION_CHANNEL_GIT.to_string(),
                    DISTRIBUTION_CHANNEL_API.to_string(),
                    DISTRIBUTION_CHANNEL_FILE.to_string(),
                ]
            });
        validate_channels(&channels)?;
        if body.api_rate_limit_per_minute < 0 {
            return Err(UnifiedError::bad_request(
                "api_rate_limit_per_minute must not be negative",
            ));
        }
        let now = Utc::now();
        let record = LicenseProduct {
            id: Uuid::now_v7(),
            product,
            display_name: body.display_name.clone().map(|name| name.trim().to_string()),
            owner_id: body.owner_id,
            key_prefix,
            segments,
            key_format: body.key_format.trim().to_string(),
            key_pattern: body.key_pattern.clone(),
            default_max_activations: body.default_max_activations,
            default_validity_days: body.default_validity_days,
            grace_period_days: body.grace_period_days,
            renewal_reminder_days: reminder_days,
            offline_max_days: body.offline_max_days,
            allowed_distribution_channels: channels,
            api_allowed_endpoints: body.api_allowed_endpoints.clone().unwrap_or_default(),
            api_rate_limit_per_minute: body.api_rate_limit_per_minute,
            created_at: now,
            updated_at: now,
        };
        license_products_store().insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_license_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseProduct> = (|| {
        require_caller_capability(&ctx, &req, "licensing:view")?;
        license_products_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_license_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateLicenseProductRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseProduct> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        let mut record = license_products_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))?;
        // The slug is immutable: licenses, releases, and
        // distributions reference it, so renaming would orphan them.
        if let Some(name) = &body.display_name {
            record.display_name = name.clone().map(|value| value.trim().to_string());
        }
        if let Some(owner_id) = body.owner_id {
            record.owner_id = owner_id;
        }
        if let Some(prefix) = &body.key_prefix {
            record.key_prefix = validate_prefix(prefix)?;
        }
        if let Some(segments) = body.segments {
            record.segments = segments.clamp(1, 8);
        }
        if let Some(format) = &body.key_format {
            let pattern = body
                .key_pattern
                .clone()
                .unwrap_or_else(|| record.key_pattern.clone());
            validate_key_format(format, &pattern)?;
            record.key_format = format.trim().to_string();
        }
        if let Some(pattern) = &body.key_pattern {
            if let Some(pattern) = pattern {
                Regex::new(pattern).map_err(|error| {
                    UnifiedError::bad_request(format!("Invalid key_pattern regex: {error}"))
                })?;
            }
            record.key_pattern = pattern.clone();
        }
        if let Some(max) = body.default_max_activations {
            if max < 1 {
                return Err(UnifiedError::bad_request(
                    "default_max_activations must be at least 1",
                ));
            }
            record.default_max_activations = max;
        }
        if let Some(validity) = body.default_validity_days {
            record.default_validity_days = validity;
        }
        if let Some(grace) = body.grace_period_days {
            if grace < 0 || grace > 365 {
                return Err(UnifiedError::bad_request(
                    "grace_period_days must be between 0 and 365",
                ));
            }
            record.grace_period_days = grace;
        }
        if let Some(windows) = &body.renewal_reminder_days {
            for window in windows {
                if *window < 0 || *window > 365 {
                    return Err(UnifiedError::bad_request(
                        "renewal_reminder_days entries must be between 0 and 365",
                    ));
                }
            }
            record.renewal_reminder_days = windows.clone();
        }
        if let Some(days) = body.offline_max_days {
            if days < 1 || days > MAX_OFFLINE_TOKEN_DAYS {
                return Err(UnifiedError::bad_request(format!(
                    "offline_max_days must be between 1 and {MAX_OFFLINE_TOKEN_DAYS}"
                )));
            }
            record.offline_max_days = days;
        }
        if let Some(channels) = &body.allowed_distribution_channels {
            validate_channels(channels)?;
            record.allowed_distribution_channels = channels.clone();
        }
        if let Some(endpoints) = &body.api_allowed_endpoints {
            record.api_allowed_endpoints = endpoints.clone();
        }
        if let Some(limit) = body.api_rate_limit_per_minute {
            if limit < 0 {
                return Err(UnifiedError::bad_request(
                    "api_rate_limit_per_minute must not be negative",
                ));
            }
            record.api_rate_limit_per_minute = limit;
        }
        record.updated_at = Utc::now();
        license_products_store().insert(id, record.clone());
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_license_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        let record = license_products_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))?;
        if licenses_store()
            .iter()
            .any(|entry| entry.value().product == record.product)
        {
            return Err(UnifiedError::conflict(format!(
                "License product \"{}\" still has licenses; revoke or move them first",
                record.product
            )));
        }
        license_products_store().remove(&id);
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_product_slug_ok_and_normalizes() {
        assert_eq!(
            validate_product_slug("My-Product_1").expect("valid slug"),
            "my-product_1"
        );
        assert_eq!(validate_product_slug("  abc  ").expect("valid slug"), "abc");
    }

    #[test]
    fn validate_product_slug_rejects_bad_slugs() {
        assert!(validate_product_slug("").is_err());
        assert!(validate_product_slug("   ").is_err());
        assert!(validate_product_slug("a b").is_err());
        assert!(validate_product_slug("-abc").is_err());
        assert!(validate_product_slug("_abc").is_err());
        assert!(validate_product_slug(&"a".repeat(65)).is_err());
        assert_eq!(
            validate_product_slug(&"a".repeat(64)).expect("64 chars"),
            "a".repeat(64)
        );
    }

    #[test]
    fn validate_key_format_cases() {
        assert!(validate_key_format(KEY_FORMAT_STANDARD, &None).is_ok());
        assert!(validate_key_format(KEY_FORMAT_CUSTOM, &None).is_err());
        assert!(
            validate_key_format(KEY_FORMAT_CUSTOM, &Some("[A-Z]+".to_string())).is_ok()
        );
        assert!(validate_key_format(KEY_FORMAT_CUSTOM, &Some("[".to_string())).is_err());
        assert!(validate_key_format("weird", &None).is_err());
    }

    #[test]
    fn validate_channels_cases() {
        assert!(validate_channels(&[]).is_ok());
        assert!(validate_channels(&[
            DISTRIBUTION_CHANNEL_GIT.to_string(),
            DISTRIBUTION_CHANNEL_API.to_string(),
            DISTRIBUTION_CHANNEL_FILE.to_string(),
        ])
        .is_ok());
        assert!(validate_channels(&["ftp".to_string()]).is_err());
        assert!(validate_channels(&["GIT".to_string()]).is_err());
    }

    #[test]
    fn validate_prefix_cases() {
        assert_eq!(validate_prefix(&None).expect("none"), None);
        assert_eq!(
            validate_prefix(&Some("  ".to_string())).expect("blank"),
            None
        );
        assert_eq!(
            validate_prefix(&Some("abc".to_string())).expect("lowercase"),
            Some("ABC".to_string())
        );
        assert!(validate_prefix(&Some("toolong99".to_string())).is_err());
        assert!(validate_prefix(&Some("ab-c".to_string())).is_err());
    }
}
