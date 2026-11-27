//! Licensing - File-upload releases and signed downloads.

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
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, module::*};

// SaaS handlers: file-upload releases.

pub(crate) fn sanitize_release_file_name(raw: &str) -> UnifiedResult<String> {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." || name.len() > 255 {
        return Err(UnifiedError::bad_request(
            "Uploaded file name is invalid",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(UnifiedError::bad_request(
            "Uploaded file name may only contain letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(name)
}

/// Semver pattern, compiled once and shared by every `valid_semver` call.
fn semver_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")
            .expect("semver pattern is valid")
    })
}

pub(crate) fn valid_semver(version: &str) -> bool {
    semver_pattern().is_match(version)
}

/// Build a signed, expiring download link for a release, bound to
/// one license: `/distribute/releases/{id}/file?license_id=...&
/// expires=...&sig=...`. The signature is HMAC-SHA256 over
/// `licensing-download|{release_id}|{license_id}|{expires}`.
pub(crate) fn build_signed_download_url(
    ctx: &AppContext,
    release_id: Uuid,
    license_id: Uuid,
) -> (String, DateTime<Utc>) {
    let expires_at = Utc::now() + chrono::Duration::seconds(SIGNED_DOWNLOAD_TTL_SECS);
    let expires = expires_at.timestamp();
    let message = format!("licensing-download|{release_id}|{license_id}|{expires}");
    let mut mac = HmacSha256::new_from_slice(&hmac_secret(ctx))
        .expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let sig: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    (
        format!(
            "/api/v1/licensing/distribute/releases/{release_id}/file?license_id={license_id}&expires={expires}&sig={sig}"
        ),
        expires_at,
    )
}

/// Upload a build/package for a licensed product (multipart:
/// `version` text field, `file` binary field, optional
/// `storage_target` text field). The bytes go to the named storage
/// target through the shared `StorageRegistry` when one is
/// registered; when no target is registered the bytes are kept in
/// memory and the response says so (`stored_via: "memory"`). The
/// SHA-256 of the upload is recorded and the release is versioned
/// with a validated semver string.
pub(crate) async fn create_release(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    product_id: web::Path<Uuid>,
    mut payload: Multipart,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let product_id = product_id.into_inner();
        let product = license_products_store()
            .get(&product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))?;
        if !product
            .allowed_distribution_channels
            .iter()
            .any(|channel| channel == DISTRIBUTION_CHANNEL_FILE)
        {
            return Err(UnifiedError::bad_request(format!(
                "License product \"{}\" does not allow the file distribution channel",
                product.product
            )));
        }
        let mut version = String::new();
        let mut file_name: Option<String> = None;
        let mut storage_target: Option<String> = None;
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(mut field) = payload
            .try_next()
            .await
            .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
        {
            let name = field
                .content_disposition()
                .and_then(|cd| cd.get_name())
                .unwrap_or("")
                .to_string();
            match name.as_str() {
                "version" => {
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })?
                    {
                        version.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
                "storage_target" => {
                    let mut value = String::new();
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })?
                    {
                        value.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    let value = value.trim().to_string();
                    if !value.is_empty() {
                        storage_target = Some(value);
                    }
                }
                "file" => {
                    let raw_name = field
                        .content_disposition()
                        .and_then(|cd| cd.get_filename())
                        .unwrap_or("upload")
                        .to_string();
                    file_name = Some(sanitize_release_file_name(&raw_name)?);
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })?
                    {
                        if bytes.len() + chunk.len() > MAX_RELEASE_UPLOAD_BYTES {
                            return Err(UnifiedError::new(
                                "Upload exceeds the 100 MB size limit",
                                413,
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                }
                _ => {
                    while let Some(_chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })?
                    {}
                }
            }
        }
        let version = version.trim().to_string();
        if version.is_empty() || !valid_semver(&version) {
            return Err(UnifiedError::bad_request(
                "version is required and must be a semantic version (for example 1.2.3)",
            ));
        }
        let file_name = file_name
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"file\" is required"))?;
        if bytes.is_empty() {
            return Err(UnifiedError::bad_request(
                "Uploaded file must not be empty",
            ));
        }
        if license_releases_store()
            .iter()
            .any(|entry| {
                entry.value().product_id == product_id && entry.value().version == version
            })
        {
            return Err(UnifiedError::conflict(format!(
                "Release version \"{version}\" already exists for this product"
            )));
        }
        let sha256_hex = sha256_hex(&bytes);
        let size_bytes = bytes.len() as i64;
        let storage_key = format!(
            "licensing-releases/{}/{}/{}",
            product.product, version, file_name
        );
        let target_name = storage_target
            .clone()
            .unwrap_or_else(|| DEFAULT_RELEASE_TARGET.to_string());
        let release_id = Uuid::now_v7();
        let (stored_target, stored_via) = match ctx.storage_registry.get(&target_name) {
            Some(provider) => {
                provider
                    .upload(&storage_key, bytes)
                    .await
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot store release: {error}"))
                    })?;
                (Some(target_name), "provider")
            }
            None => {
                release_blobs_store().insert(release_id, bytes);
                (None, "memory")
            }
        };
        let release = LicenseRelease {
            id: release_id,
            product_id,
            product: product.product.clone(),
            version,
            file_name,
            storage_target: stored_target,
            storage_key,
            sha256_hex,
            size_bytes,
            download_count: 0,
            created_at: Utc::now(),
        };
        license_releases_store().insert(release.id, release.clone());
        Ok(serde_json::json!({
            "release": release,
            "stored_via": stored_via,
        }))
    })()
    .await;
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_releases(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    product_id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let product_id = product_id.into_inner();
        if !license_products_store().contains_key(&product_id) {
            return Err(UnifiedError::not_found("License product not found"));
        }
        let mut items: Vec<LicenseRelease> = license_releases_store()
            .iter()
            .filter(|entry| entry.value().product_id == product_id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.version.to_lowercase().contains(&needle)
                    || row.file_name.to_lowercase().contains(&needle)
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

/// Issue a signed download link for a release. The caller proves
/// the license (`license_key` + `instance_id`); a successful call
/// activates the instance (consumes a seat for a new instance) and
/// logs the download. The link itself is served by
/// `download_release_file` and expires after one hour.
pub(crate) async fn download_release(
    ctx: web::Data<Arc<AppContext>>,
    release_id: web::Path<Uuid>,
    query: web::Query<DownloadQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let release_id = release_id.into_inner();
        let release = license_releases_store()
            .get(&release_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        let outcome = validate_and_maybe_activate(
            &ctx,
            &query.license_key,
            Some(query.instance_id.trim()),
            query.hostname.as_deref(),
            true,
        )?;
        let Some(license) = &outcome.license else {
            return Err(UnifiedError::unprocessable(format!(
                "License download failed: {}",
                outcome.reason.clone().unwrap_or_else(|| "invalid".to_string())
            ))
            .with_metadata(
                "reason",
                Value::String(outcome.reason.clone().unwrap_or_default()),
            ));
        };
        if license.product != release.product {
            return Err(UnifiedError::forbidden(
                "License product does not match this release",
            )
            .with_metadata("reason", Value::String("product_mismatch".to_string())));
        }
        if !outcome.valid {
            return Err(UnifiedError::unprocessable(format!(
                "License download failed: {}",
                outcome.reason.clone().unwrap_or_else(|| "invalid".to_string())
            ))
            .with_metadata(
                "reason",
                Value::String(outcome.reason.clone().unwrap_or_default()),
            ));
        }
        if let Some(mut entry) = license_releases_store().get_mut(&release_id) {
            entry.download_count += 1;
        }
        let (download_url, expires_at) =
            build_signed_download_url(&ctx, release_id, license.id);
        append_license_event(
            &ctx,
            license.id,
            "downloaded",
            None,
            serde_json::json!({
                "release_id": release_id,
                "version": release.version,
                "instance_id": query.instance_id,
            }),
        );
        ctx.hooks.emit(
            LICENSE_DOWNLOAD,
            serde_json::json!({
                "license_id": license.id,
                "release_id": release_id,
                "version": release.version,
                "instance_id": query.instance_id,
            }),
        );
        Ok(serde_json::json!({
            "download_url": download_url,
            "expires_at": expires_at,
            "release_id": release_id,
            "version": release.version,
            "file_name": release.file_name,
            "size_bytes": release.size_bytes,
            "sha256_hex": release.sha256_hex,
            "license_id": license.id,
        }))
    })();
    json::respond(result, debug)
}

/// True when `license` may still download through a signed link:
/// it must be `active`, and when it has an expiry that expiry must
/// not have passed outside the product grace period (the same rule
/// validation applies). A signed link lives for one hour, so
/// without this check a revoked license's link would keep
/// downloading until the link itself expired.
pub(crate) fn license_may_download(license: &License) -> bool {
    if license.status != "active" {
        return false;
    }
    match license.expires_at {
        Some(expires_at) => expires_at > Utc::now() || grace_state(license).0,
        None => true,
    }
}

/// Serve a release through a signed link issued by
/// `download_release` (or `git_access_handler`). The HMAC signature
/// is the credential here: no license key and no API key are needed,
/// and the link expires after one hour. When the release lives in a
/// storage target the caller is redirected to that provider's signed
/// URL; otherwise the bytes kept in memory are served directly.
/// The license is also re-checked after the signature verifies: a
/// revoked, suspended, or expired license's link stops working
/// immediately instead of serving for the rest of the hour (see
/// `license_may_download`).
pub(crate) async fn download_release_file(
    ctx: web::Data<Arc<AppContext>>,
    release_id: web::Path<Uuid>,
    query: web::Query<SignedFileQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let release_id = release_id.into_inner();
    let release = match license_releases_store()
        .get(&release_id)
        .map(|entry| entry.value().clone())
    {
        Some(release) => release,
        None => {
            return json::error_response(&UnifiedError::not_found("Release not found"), debug);
        }
    };
    let Ok(license_id) = query.license_id.parse::<Uuid>() else {
        return json::error_response(
            &UnifiedError::bad_request("license_id must be a UUID"),
            debug,
        );
    };
    if query.expires <= Utc::now().timestamp() {
        return json::error_response(
            &UnifiedError::unauthorized("Signed download link has expired"),
            debug,
        );
    }
    let message = format!("licensing-download|{release_id}|{license_id}|{}", query.expires);
    let mut mac = match HmacSha256::new_from_slice(&hmac_secret(&ctx)) {
        Ok(mac) => mac,
        Err(_) => {
            return json::error_response(
                &UnifiedError::internal("Cannot initialise download verification"),
                debug,
            );
        }
    };
    mac.update(message.as_bytes());
    let expected: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    if !constant_time_equal(&expected, &query.sig.to_ascii_lowercase()) {
        return json::error_response(
            &UnifiedError::unauthorized("Signed download link verification failed"),
            debug,
        );
    }
    // The signature is verified, so the caller holds a genuine
    // link; now re-check the license itself. A revoked, suspended,
    // or expired license's link stops working immediately instead
    // of serving for the rest of the hour.
    match load_license_db_first(&ctx, license_id) {
        Ok(Some(license)) if license_may_download(&license) => {}
        Ok(Some(_)) => {
            return json::error_response(
                &UnifiedError::forbidden(
                    "The license for this download link is no longer active",
                )
                .with_metadata("reason", Value::String("license_not_active".to_string())),
                debug,
            );
        }
        Ok(None) => {
            return json::error_response(
                &UnifiedError::not_found("License not found"),
                debug,
            );
        }
        Err(error) => return json::error_response(&error, debug),
    }
    if let Some(target) = &release.storage_target {
        if let Some(provider) = ctx.storage_registry.get(target) {
            return match provider.signed_url(&release.storage_key, 3600).await {
                Ok(url) => HttpResponse::Found()
                    .insert_header((header::LOCATION, url))
                    .finish(),
                Err(error) => json::error_response(&error, debug),
            };
        }
    }
    match release_blobs_store().get(&release_id) {
        Some(bytes) => {
            let file_name = release.file_name.replace('"', "");
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .body(bytes.value().clone())
        }
        None => json::error_response(
            &UnifiedError::new(
                "Release bytes are no longer available in memory (the server restarted after an in-memory upload); re-upload the release",
                410,
            ),
            debug,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_release_file_name_cases() {
        assert_eq!(
            sanitize_release_file_name("release.zip").expect("plain name"),
            "release.zip"
        );
        assert_eq!(
            sanitize_release_file_name("a/b/c.zip").expect("path"),
            "c.zip"
        );
        assert_eq!(
            sanitize_release_file_name("../evil").expect("traversal"),
            "evil"
        );
        assert_eq!(
            sanitize_release_file_name("  spaced.zip  ").expect("trimmed"),
            "spaced.zip"
        );
    }

    #[test]
    fn sanitize_release_file_name_rejects_bad_names() {
        assert!(sanitize_release_file_name("").is_err());
        assert!(sanitize_release_file_name("   ").is_err());
        assert!(sanitize_release_file_name(".").is_err());
        assert!(sanitize_release_file_name("..").is_err());
        assert!(sanitize_release_file_name("a b.zip").is_err());
        assert!(sanitize_release_file_name("a/b.zip").is_err());
        assert!(sanitize_release_file_name(&"a".repeat(256)).is_err());
        assert_eq!(
            sanitize_release_file_name(&"a".repeat(255)).expect("255 chars"),
            "a".repeat(255)
        );
    }

    #[test]
    fn valid_semver_cases() {
        assert!(valid_semver("1.2.3"));
        assert!(valid_semver("0.0.1"));
        assert!(valid_semver("1.2.3-beta.1"));
        assert!(valid_semver("1.2.3+build.5"));
        assert!(valid_semver("1.2.3-beta+build"));
        assert!(!valid_semver(""));
        assert!(!valid_semver("1.2"));
        assert!(!valid_semver("v1.2.3"));
        assert!(!valid_semver("1.2.3.4"));
        assert!(!valid_semver("1.2.x"));
    }

    #[test]
    fn valid_semver_is_stable_across_repeated_calls() {
        // The semver pattern is compiled once and shared: repeated
        // calls must agree exactly with the first call.
        for _ in 0..3 {
            assert!(valid_semver("1.2.3"));
            assert!(!valid_semver("1.2"));
        }
    }

    fn download_test_license(
        status: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> License {
        License {
            id: uuid::Uuid::new_v4(),
            license_key: "TEST-KEY".to_string(),
            product: "download-test-no-settings-9f2c".to_string(),
            user_id: None,
            company_id: None,
            max_activations: 1,
            status: status.to_string(),
            expires_at,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn license_may_download_cases() {
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        // Active and unexpired: downloads.
        assert!(license_may_download(&download_test_license(
            "active",
            Some(future)
        )));
        // Active with no expiry: downloads.
        assert!(license_may_download(&download_test_license("active", None)));
        // A revoked license's signed link must stop working at once,
        // not keep serving for the rest of the hour.
        assert!(!license_may_download(&download_test_license(
            "revoked",
            Some(future)
        )));
        assert!(!license_may_download(&download_test_license(
            "suspended",
            Some(future)
        )));
        assert!(!license_may_download(&download_test_license(
            "expired",
            Some(future)
        )));
        // Active on paper but past expiry (never re-validated since
        // expiry, so the status was never flipped): no download.
        assert!(!license_may_download(&download_test_license(
            "active",
            Some(past)
        )));
    }
    #[test]
    fn build_signed_download_url_has_64_hex_signature() {
        let ctx = unified_core::context::AppContext::new(
            unified_core::config::AppConfig::default(),
            std::sync::Arc::new(unified_core::hooks::HookBus::new()),
            std::sync::Arc::new(unified_core::modules::ModuleRegistry::new()),
        );
        let release_id = Uuid::now_v7();
        let license_id = Uuid::now_v7();
        let (url, _) = build_signed_download_url(&ctx, release_id, license_id);
        assert!(url.starts_with(&format!(
            "/api/v1/licensing/distribute/releases/{release_id}/file?license_id={license_id}&expires="
        )));
        let sig = url.split("sig=").nth(1).expect("sig query param");
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

}
