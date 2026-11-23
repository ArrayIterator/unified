// documents — HTTP handlers.

use super::types::UpdateDocumentMetadataRequest;
use super::validation::push_multipart_text;
use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_multipart::Multipart;
use actix_web::{HttpRequest, HttpResponse, web};
use bytes::Bytes;
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Handlers — documents
// ---------------------------------------------------------------------------

pub(crate) async fn list_documents(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let is_admin = check_capability(&ctx, &found.role, PERM_ADMIN);
    let mut items: Vec<DocumentRecord> = state
        .documents
        .iter()
        .map(|entry| entry.value().clone())
        // Role-scoped listing (task 355): the endpoint itself
        // filters — an author never receives another owner's
        // private row (see `list_visible` in validation.rs).
        .filter(|document| list_visible(document, &_scope, is_admin, &found.actor))
        .filter(|document| {
            matches_search(&document.name, &params)
                && params
                    .filters
                    .get("module_id")
                    .is_none_or(|value| document.module_id == *value)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|value| document.company_id == *value)
                && params
                    .filters
                    .get("mime")
                    .is_none_or(|value| document.mime == *value)
                && params
                    .filters
                    .get("scan_status")
                    .is_none_or(|value| document.scan_status == *value)
                && params
                    .filters
                    .get("owner_id")
                    .is_none_or(|value| document.owner_id == *value)
                && params
                    .filters
                    .get("visibility")
                    .is_none_or(|value| document.visibility == *value)
                && params.filters.get("folder_id").is_none_or(|value| {
                    // Present-but-empty means the library root; a
                    // missing filter means every folder.
                    document.folder_id == *value
                })
                && params
                    .filters
                    .get("type")
                    .is_none_or(|value| document_type(&document.mime) == value.as_str())
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}


pub(crate) async fn get_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = (|| {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if !state.can_read(&ctx, &found, &document) {
            return Err(UnifiedError::forbidden(
                "You do not have access to this document",
            ));
        }
        Ok(document)
    })();
    json::respond(result, debug)
}

/// Update a document's metadata (`PATCH /{id}`, body
/// `UpdateDocumentMetadataRequest`). Only the owner or an admin
/// may edit metadata (same rule as delete / flip / re-upload);
/// the file, its versions, and its visibility are untouched.
/// The updated record is published to the Search module
/// (`search.index`) when anything actually changed.
pub(crate) async fn update_document_metadata(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDocumentMetadataRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = (|| {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if document.owner_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may edit this document's metadata",
            ));
        }
        state.update_document_metadata(path.as_str(), body.into_inner())
    })();
    json::respond(result, debug)
}

/// Multipart multi-file upload (drag-drop): text fields
/// `module_id`, `company_id` (optional), `scope` (optional,
/// default `owner`), `visibility` (optional, `public` or
/// `private`, default `private`), `folder_id` (optional, the
/// target folder — empty or absent means the library root) plus
/// one `file` field per file.
/// Each file is validated against the per-module allowlist before
/// acceptance; per-file outcomes are reported so one bad file does
/// not lose the rest of the batch. A requested `public` visibility
/// is honored only when the scan verdict is `Clean` (the scan
/// gate); otherwise the file lands private.
pub(crate) async fn upload_documents(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    mut payload: Multipart,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<MultiUploadResult> = async move {
        let mut module_id = "documents".to_string();
        let mut company_id = String::new();
        let mut scope = "owner".to_string();
        let mut visibility = "private".to_string();
        let mut folder_id = String::new();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
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
                "module_id" | "company_id" | "scope" | "visibility" | "folder_id" => {
                    let mut text = String::new();
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        push_multipart_text(&mut text, &chunk)?;
                    }
                    match name.as_str() {
                        "module_id" => module_id = text,
                        "company_id" => company_id = text,
                        "visibility" => visibility = text,
                        "folder_id" => folder_id = text.trim().to_string(),
                        _ => scope = text,
                    }
                }
                "file" => {
                    let raw_name = field
                        .content_disposition()
                        .and_then(|cd| cd.get_filename())
                        .unwrap_or("upload")
                        .to_string();
                    let file_name = sanitize_file_name(&raw_name)?;
                    let mut bytes: Vec<u8> = Vec::new();
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        if bytes.len() + chunk.len() > MAX_UPLOAD_BYTES as usize {
                            return Err(UnifiedError::new(
                                "Upload exceeds the 100 MB size limit",
                                413,
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    files.push((file_name, bytes));
                }
                _ => {
                    while let Some(_chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })?
                    {}
                }
            }
        }
        if files.is_empty() {
            return Err(UnifiedError::bad_request(
                "Multipart field \"file\" is required (at least one file)",
            ));
        }
        let module_id = valid_module_id(&module_id)?;
        let company_id = company_id.trim().to_string();
        if company_id.len() > 128 || company_id.chars().any(|c| c.is_control()) {
            return Err(UnifiedError::bad_request("company_id is not valid"));
        }
        // The multipart company is a client claim: verify it against
        // the caller scope (adopt the scope when omitted). The upload
        // runs in an `async move` block, so the scope computed
        // outside it is used here, not the request.
        let company_id = require_company_match_for(
            &_scope,
            &if company_id.is_empty() { None } else { Some(company_id) },
        )?
        .unwrap_or_default();
        let scope = valid_scope(&scope)?;
        let visibility = valid_visibility(&visibility)?;
        // The target folder must exist and be in the caller's
        // company scope before any byte is stored, so a bad folder
        // never leaves an uploaded file stranded at the root.
        if !folder_id.is_empty() && !state.folder_accepts(&folder_id, &_scope) {
            return Err(UnifiedError::not_found("Folder not found"));
        }
        let mut uploaded: Vec<DocumentRecord> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();
        for (file_name, bytes) in files {
            match state
                .finalize_upload(
                    file_name.clone(),
                    module_id.clone(),
                    company_id.clone(),
                    found.actor.clone(),
                    scope.clone(),
                    _tenant.tenant_id,
                    None,
                    bytes,
                    visibility.clone(),
                    folder_id.clone(),
                    None,
                )
                .await
            {
                Ok(document) => uploaded.push(document),
                Err(error) => failed.push(json!({
                    "name": file_name,
                    "error": error.message,
                })),
            }
        }
        if uploaded.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "All files were rejected: {}",
                failed
                    .first()
                    .and_then(|value| value.get("error"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown error")
            )));
        }
        Ok(MultiUploadResult { uploaded, failed })
    }.await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        // Only the owner or an admin may delete a document.
        if document.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may delete this document",
            ));
        }
        // Stored keys are scope-free: the provider root is
        // re-resolved from the request's scope here.
        let storage_scope = StorageScope::Tenant(_tenant.tenant_id);
        state.delete_document(storage_scope, path.as_str()).await?;
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn preview_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentPreview> = (|| async {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if !state.can_read(&ctx, &found, &document) {
            return Err(UnifiedError::forbidden(
                "You do not have access to this document",
            ));
        }
        // Preview runs the same download gate as the download
        // routes. It returns metadata only, but it reads the stored
        // bytes server-side to derive the image dimensions, so an
        // unscanned, pending, or infected file must not leak even
        // its preview metadata (checksum, dimensions, scan status)
        // to a caller who may not download it.
        state.download_gate(&document)?;
        // Stored keys are scope-free: re-resolve the provider
        // root + the document's visibility area from the request's
        // scope before downloading.
        let storage_scope = StorageScope::Tenant(_tenant.tenant_id);
        let area = StorageArea::from_visibility(&document.visibility);
        let (width, height) = match state
            .provider()?
            .download(&storage_scope.provider_key_for_area(area, &document.storage_key))
            .await
        {
            Ok(bytes) => image_dimensions(&document.mime, &bytes).unzip(),
            Err(_) => (None, None),
        };
        Ok(DocumentPreview {
            document_id: document.id.clone(),
            name: document.name.clone(),
            mime: document.mime.clone(),
            size: document.size,
            checksum: document.checksum.clone(),
            version_count: document.version_count,
            current_version: document.current_version,
            scan_status: document.scan_status.clone(),
            scan_threat: document.scan_threat.clone(),
            width,
            height,
            created_at: document.created_at,
            updated_at: document.updated_at,
        })
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn issue_signed_url(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignedUrlRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<SignedUrlResponse> = (|| {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if !state.can_read(&ctx, &found, &document) {
            return Err(UnifiedError::forbidden(
                "You do not have access to this document",
            ));
        }
        let expires_secs = body
            .into_inner()
            .expires_secs
            .unwrap_or(DEFAULT_SIGNED_URL_SECS)
            .clamp(1, MAX_SIGNED_URL_SECS);
        // The scope is embedded in the token: the redemption route
        // has no tenant context, so the token itself must carry the
        // scope the provider root is resolved from. The URL points
        // at the private file route (the API is JSON-only — file
        // bytes never flow through `/api/v1/*`); the token travels
        // as a query parameter.
        let storage_scope = StorageScope::Tenant(_tenant.tenant_id);
        let (token, expires_at) =
            state.issue_signed_token(&document.id, storage_scope, expires_secs);
        Ok(SignedUrlResponse {
            document_id: document.id.clone(),
            url: format!("/files/private/{}?token={token}", document.storage_key),
            token,
            expires_at,
        })
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers — versions
// ---------------------------------------------------------------------------

/// Re-upload one file as a new version of an existing document
/// (multipart field `file`).
pub(crate) async fn upload_new_version(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: Multipart,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = async move {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if document.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may add a version to this document",
            ));
        }
        let mut file_name: Option<String> = None;
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
            if name == "file" {
                let raw_name = field
                    .content_disposition()
                    .and_then(|cd| cd.get_filename())
                    .unwrap_or(&document.name)
                    .to_string();
                file_name = Some(sanitize_file_name(&raw_name)?);
                while let Some(chunk) = field
                    .try_next()
                    .await
                    .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                {
                    if bytes.len() + chunk.len() > MAX_UPLOAD_BYTES as usize {
                        return Err(UnifiedError::new(
                            "Upload exceeds the 100 MB size limit",
                            413,
                        ));
                    }
                    bytes.extend_from_slice(&chunk);
                }
            } else {
                while let Some(_chunk) = field.try_next().await.map_err(|error| {
                    UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                })?
                {}
            }
        }
        let file_name = file_name
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"file\" is required"))?;
        // A re-upload inherits the document's visibility (it never
        // changes it); the new version's object lands in the
        // document's current area. It also keeps the document's
        // folder, so the folder argument is empty here.
        let visibility = document.visibility.clone();
        state
            .finalize_upload(
                file_name,
                document.module_id.clone(),
                document.company_id.clone(),
                found.actor.clone(),
                document.scope.clone(),
                _tenant.tenant_id,
                None,
                bytes,
                visibility,
                String::new(),
                Some(document.id.clone()),
            )
            .await
    }.await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_versions(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let document = match state.load_document(path.as_str()) {
        Ok(document) => document,
        Err(error) => return json::error_response(&error, debug),
    };
    if !state.can_read(&ctx, &found, &document) {
        return json::error_response(
            &UnifiedError::forbidden("You do not have access to this document"),
            debug,
        );
    }
    let mut items: Vec<DocumentVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| {
            version.document_id == document.id
                && params
                    .filters
                    .get("scan_status")
                    .is_none_or(|value| version.scan_status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.version.cmp(&a.version));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn get_version(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let (document_id, version_number) = path.into_inner();
    let result: UnifiedResult<DocumentVersion> = (|| {
        let document = state.load_document(&document_id)?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if !state.can_read(&ctx, &found, &document) {
            return Err(UnifiedError::forbidden(
                "You do not have access to this document",
            ));
        }
        state
            .versions
            .iter()
            .find(|entry| {
                entry.value().document_id == document_id
                    && entry.value().version == version_number
            })
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Document version not found"))
    })();
    json::respond(result, debug)
}

/// Restore an old version: creates a NEW version with the old
/// content (the stored object reference count grows by one).
/// History is never mutated.
pub(crate) async fn restore_version(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let (document_id, version_number) = path.into_inner();
    let result: UnifiedResult<DocumentRecord> = async move {
        let document = state.load_document(&document_id)?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if document.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may restore a version of this document",
            ));
        }
        let old = state
            .versions
            .iter()
            .find(|entry| {
                entry.value().document_id == document_id
                    && entry.value().version == version_number
            })
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Document version not found"))?;
        if old.version == document.current_version {
            return Err(UnifiedError::conflict(
                "That version is already the current version",
            ));
        }
        let bytes = {
            // Stored keys are scope-free: re-resolve the provider
            // root + the document's visibility area from the
            // request's scope before downloading.
            let storage_scope = StorageScope::Tenant(_tenant.tenant_id);
            let area = StorageArea::from_visibility(&document.visibility);
            state
                .provider()?
                .download(&storage_scope.provider_key_for_area(area, &old.storage_key))
                .await?
        };
        // Same per-document lock as `finalize_upload`: the version
        // read, the +1, and the version/document inserts happen
        // atomically, so two concurrent restores cannot create the
        // same version number. The current version is re-read under
        // the lock (a concurrent restore may have completed first).
        let version_lock = state.document_lock(&document.id);
        let _version_guard = version_lock
            .lock()
            .expect("document lock poisoned");
        let current = state.load_document(&document_id)?;
        if old.version == current.current_version {
            return Err(UnifiedError::conflict(
                "That version is already the current version",
            ));
        }
        let new_version_number = current.current_version + 1;
        let (scan_status, scan_threat, _scanner) =
            state.scan_document_bytes(&document.id, new_version_number, &bytes);
        let version = DocumentVersion {
            id: new_id(),
            document_id: document.id.clone(),
            version: new_version_number,
            checksum: old.checksum.clone(),
            storage_key: old.storage_key.clone(),
            size: old.size,
            mime: old.mime.clone(),
            uploader_id: found.actor.clone(),
            scan_status: scan_status.as_str().to_string(),
            scan_threat: scan_threat.clone(),
            created_at: now(),
        };
        state.versions.insert(version.id.clone(), version.clone());
        let mut updated = current.clone();
        updated.mime = old.mime.clone();
        updated.size = old.size;
        updated.checksum = old.checksum.clone();
        updated.storage_key = old.storage_key.clone();
        updated.current_version = new_version_number;
        updated.version_count += 1;
        updated.scan_status = scan_status.as_str().to_string();
        updated.scan_threat = scan_threat.clone();
        updated.updated_at = now();
        state.documents.insert(updated.id.clone(), updated.clone());
        drop(_version_guard);
        state.emit(
            events::VERSION_CREATED,
            serde_json::to_value(&version).unwrap_or(Value::Null),
        );
        Ok(updated)
    }.await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Re-run the virus scan on the current version (admin). The
/// recorded verdict replaces the previous one; the version history
/// is untouched.
pub(crate) async fn rescan_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let _found = match authorize(&ctx, &req, PERM_ADMIN) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = async move {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        // Stored keys are scope-free: re-resolve the provider
        // root + the document's visibility area from the request's
        // scope before downloading.
        let storage_scope = StorageScope::Tenant(_tenant.tenant_id);
        let area = StorageArea::from_visibility(&document.visibility);
        let bytes = state
            .provider()?
            .download(&storage_scope.provider_key_for_area(area, &document.storage_key))
            .await?;
        let (scan_status, scan_threat, _scanner) = state.scan_document_bytes(
            &document.id,
            document.current_version,
            &bytes,
        );
        let mut updated = document.clone();
        updated.scan_status = scan_status.as_str().to_string();
        updated.scan_threat = scan_threat.clone();
        updated.updated_at = now();
        state.documents.insert(updated.id.clone(), updated.clone());
        if let Some(mut entry) = state
            .versions
            .iter_mut()
            .find(|entry| {
                entry.value().document_id == document.id
                    && entry.value().version == document.current_version
            })
        {
            entry.scan_status = scan_status.as_str().to_string();
            entry.scan_threat = scan_threat.clone();
        }
        Ok(updated)
    }.await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers — visibility + picker
// ---------------------------------------------------------------------------

/// Flip a document's visibility between public and private
/// (`POST /{id}/flip`, body `{"visibility": "public"|"private"}`).
/// Permissioned + audited: only the owner or an admin may flip,
/// and the flip emits `document.visibility.flipped`. The object
/// MOVES between the public and private areas; the stored key (and
/// the URL) never changes. A flip to public is refused unless the
/// scan is clean (the scan gate).
pub(crate) async fn flip_document_visibility(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FlipVisibilityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<DocumentRecord> = async move {
        let document = state.load_document(path.as_str())?;
        require_record_in_company_scope(&document.company_id, &_scope)?;
        if document.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may change this document's visibility",
            ));
        }
        let visibility = valid_visibility(&body.into_inner().visibility)?;
        let storage_scope = StorageScope::Tenant(_tenant.tenant_id);
        state
            .flip_visibility(storage_scope, path.as_str(), visibility, &found.actor)
            .await
    }
    .await;
    json::respond(result, debug)
}

/// Shared media picker (`GET /picker`): the public-only tab of
/// the media library. Returns ONLY public documents in the
/// caller's company scope — private files are invisible here
/// until a manager/admin flips them public. Any author in the
/// company may attach a public file (public visibility means
/// "visible to the tenant"); the document's owner/admin rules
/// still govern flips and deletes. Supports the standard search +
/// pagination parameters.
/// Picker visibility: only PUBLIC documents, inside the caller's
/// company scope, ever reach the picker list. A private file is
/// invisible until a manager/admin flips it public — the picker tab
/// cannot leak it, whatever the search terms.
fn picker_visible(document: &DocumentRecord, caller_company: &Option<String>) -> bool {
    document.visibility == "public" && in_company_scope(&document.company_id, caller_company)
}

pub(crate) async fn list_picker(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let _found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let mut items: Vec<PickerItem> = state
        .documents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|document| picker_visible(document, &_scope))
        .filter(|document| {
            matches_search(&document.name, &params)
                && params
                    .filters
                    .get("module_id")
                    .is_none_or(|value| document.module_id == *value)
                && params
                    .filters
                    .get("mime")
                    .is_none_or(|value| document.mime == *value)
                && params.filters.get("folder_id").is_none_or(|value| {
                    document.folder_id == *value
                })
                && params
                    .filters
                    .get("type")
                    .is_none_or(|value| document_type(&document.mime) == value.as_str())
        })
        .map(|document| PickerItem {
            document_id: document.id.clone(),
            name: document.name.clone(),
            mime: document.mime.clone(),
            size: document.size,
            storage_key: document.storage_key.clone(),
            url: format!("/files/public/{}", document.storage_key),
            visibility: document.visibility.clone(),
            created_at: document.created_at,
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_document(visibility: &str, company_id: &str) -> DocumentRecord {
        DocumentRecord {
            id: "doc-1".to_string(),
            name: "cover.jpg".to_string(),
            title: "cover.jpg".to_string(),
            alt_text: None,
            caption: None,
            description: None,
            custom_metadata: serde_json::Map::new(),
            module_id: "content".to_string(),
            company_id: company_id.to_string(),
            owner_id: "user-1".to_string(),
            scope: "owner".to_string(),
            mime: "image/jpeg".to_string(),
            size: 1024,
            checksum: Some("aa".repeat(32)),
            storage_key: "image/2026/11/23/019a1234-5678-7abc-def0-123456789abc.jpg".to_string(),
            current_version: 1,
            version_count: 1,
            scan_status: "clean".to_string(),
            scan_threat: None,
            visibility: visibility.to_string(),
            folder_id: String::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn picker_shows_only_public_documents_in_company_scope() {
        let company = Some("company-1".to_string());
        // Public + in scope: visible.
        assert!(picker_visible(
            &test_document("public", "company-1"),
            &company
        ));
        // Private + in scope: invisible — the picker tab cannot
        // leak it, whatever the search terms.
        assert!(!picker_visible(
            &test_document("private", "company-1"),
            &company
        ));
        // Public but another company's: invisible.
        assert!(!picker_visible(
            &test_document("public", "company-2"),
            &company
        ));
        // A company-less caller sees only unscoped public rows.
        assert!(picker_visible(&test_document("public", ""), &None));
        assert!(!picker_visible(
            &test_document("public", "company-1"),
            &None
        ));
        assert!(!picker_visible(&test_document("private", ""), &None));
    }
}
