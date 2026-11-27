//! HTTP handlers for release files and integrity verification.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use std::sync::{Arc};
use super::helpers::{
    caller, caller_company, require_company_for_write, require_file_in_scope, require_release_in_scope, require_tenant,
    require_permission,
};
use super::types::{CreateReleaseFile, PERM_SOFTWARE_CREATE, PERM_SOFTWARE_MANAGE, PERM_SOFTWARE_READ, ReleaseFile, UpdateReleaseFile, VerifyFile};
use super::state::SoftwareState;
use super::validation::{new_id, now, respond_created, sha256_hex, sha512_hex, validate_hex_checksum, validate_url};
use super::handlers_versions::release_files;

// Release file handlers

pub(crate) async fn create_release_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReleaseFile>,
) -> HttpResponse {
    let result: UnifiedResult<ReleaseFile> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        let input = body.into_inner();
        let release = require_release_in_scope(&state, path.as_str(), &scope)?;
        if input.filename.trim().is_empty() {
            return Err(UnifiedError::bad_request("Filename must not be empty"));
        }
        if let Some(checksum) = &input.checksum_sha256 {
            validate_hex_checksum(&checksum.to_lowercase(), 64, "checksum_sha256")?;
        }
        if let Some(checksum) = &input.checksum_sha512 {
            validate_hex_checksum(&checksum.to_lowercase(), 128, "checksum_sha512")?;
        }
        if let Some(url) = &input.download_url {
            validate_url(url, "Download URL")?;
        }
        if input.download_url.is_none() && input.storage_path.is_none() {
            return Err(UnifiedError::bad_request(
                "Either download_url or storage_path is required",
            ));
        }
        let integrity_status = if input.checksum_sha256.is_some() || input.checksum_sha512.is_some() {
            "pending".to_string()
        } else {
            "unverified".to_string()
        };
        let file = ReleaseFile {
            id: new_id(),
            release_id: release.id,
            version_id: release.version_id,
            filename: input.filename.trim().to_string(),
            size_bytes: input.size_bytes,
            checksum_sha256: input.checksum_sha256.map(|value| value.to_lowercase()),
            checksum_sha512: input.checksum_sha512.map(|value| value.to_lowercase()),
            download_url: input.download_url,
            storage_path: input.storage_path,
            integrity_status,
            verified_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.release_files.insert(file.id.clone(), file.clone());
        state.emit(
            "software.file.created",
            serde_json::to_value(&file).unwrap_or(Value::Null),
        );
        Ok(file)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_release_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<ReleaseFile> =
        require_file_in_scope(&state, &release_id, &file_id, &scope);
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_release_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateReleaseFile>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<ReleaseFile> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_file_in_scope(&state, &release_id, &file_id, &scope)?;
        let input = body.into_inner();
        let mut file = state
            .release_files
            .get_mut(file_id.as_str())
            .filter(|entry| entry.release_id == release_id)
            .ok_or_else(|| UnifiedError::not_found("Release file not found"))?;
        if let Some(filename) = input.filename {
            if filename.trim().is_empty() {
                return Err(UnifiedError::bad_request("Filename must not be empty"));
            }
            file.filename = filename.trim().to_string();
        }
        if let Some(size_bytes) = input.size_bytes {
            file.size_bytes = size_bytes;
        }
        if let Some(checksum) = input.checksum_sha256 {
            validate_hex_checksum(&checksum.to_lowercase(), 64, "checksum_sha256")?;
            file.checksum_sha256 = Some(checksum.to_lowercase());
            file.integrity_status = "pending".to_string();
            file.verified_at = None;
        }
        if let Some(checksum) = input.checksum_sha512 {
            validate_hex_checksum(&checksum.to_lowercase(), 128, "checksum_sha512")?;
            file.checksum_sha512 = Some(checksum.to_lowercase());
            file.integrity_status = "pending".to_string();
            file.verified_at = None;
        }
        if let Some(url) = input.download_url {
            validate_url(&url, "Download URL")?;
            file.download_url = Some(url);
        }
        if let Some(path) = input.storage_path {
            file.storage_path = Some(path);
        }
        file.updated_at = now();
        Ok(file.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_release_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_file_in_scope(&state, &release_id, &file_id, &scope)?;
        state.release_files.remove(file_id.as_str());
        Ok(json!({ "deleted": true, "id": file_id }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn verify_release_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<VerifyFile>,
) -> HttpResponse {
    let (release_id, file_id) = path.into_inner();
    let result: UnifiedResult<ReleaseFile> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_file_in_scope(&state, &release_id, &file_id, &scope)?;
        let content = BASE64.decode(body.content_base64.trim()).map_err(|_| {
            UnifiedError::bad_request("content_base64 is not valid base64")
        })?;
        let mut file = state
            .release_files
            .get_mut(file_id.as_str())
            .filter(|entry| entry.release_id == release_id)
            .ok_or_else(|| UnifiedError::not_found("Release file not found"))?;
        if content.len() as u64 != file.size_bytes {
            file.integrity_status = "failed".to_string();
            file.verified_at = Some(now());
            file.updated_at = now();
            return Err(UnifiedError::unprocessable(format!(
                "Size mismatch: content is {} bytes, expected {} bytes",
                content.len(),
                file.size_bytes
            )));
        }
        let computed_sha256 = sha256_hex(&content);
        let computed_sha512 = sha512_hex(&content);
        let sha256_ok = file
            .checksum_sha256
            .as_ref()
            .is_none_or(|expected| *expected == computed_sha256);
        let sha512_ok = file
            .checksum_sha512
            .as_ref()
            .is_none_or(|expected| *expected == computed_sha512);
        if file.checksum_sha256.is_none() {
            file.checksum_sha256 = Some(computed_sha256);
        }
        if file.checksum_sha512.is_none() {
            file.checksum_sha512 = Some(computed_sha512);
        }
        file.verified_at = Some(now());
        file.updated_at = now();
        if sha256_ok && sha512_ok {
            file.integrity_status = "verified".to_string();
            let verified = file.clone();
            drop(file);
            state.emit(
                "software.file.verified",
                serde_json::to_value(&verified).unwrap_or(Value::Null),
            );
            Ok(verified)
        } else {
            file.integrity_status = "failed".to_string();
            let failed = file.clone();
            drop(file);
            state.emit(
                "software.file.verified",
                serde_json::to_value(&failed).unwrap_or(Value::Null),
            );
            Ok(failed)
        }
    })();
    // A failed verification is a real result, not a handler error: the
    // file record (with integrity_status "failed") is the response.
    match result {
        Ok(file) => json::success_response(file, 200),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}
