// documents — HTTP handlers: resumable upload sessions.

use super::*;
use super::validation::push_multipart_text;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::sha256_hex;
use actix_multipart::Multipart;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use dashmap::DashMap;
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Handlers — chunked / resumable upload sessions
// ---------------------------------------------------------------------------

fn session_status(session: &UploadSession, parts: &DashMap<u32, UploadPart>) -> UploadSession {
    let mut received: Vec<u32> = parts.iter().map(|entry| *entry.key()).collect();
    received.sort_unstable();
    let mut missing: Vec<u32> = (0..session.part_count)
        .filter(|index| !received.contains(index))
        .collect();
    missing.sort_unstable();
    let mut view = session.clone();
    view.received_parts = received;
    view.missing_parts = missing;
    view
}

pub(crate) async fn initiate_session(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<InitiateSessionRequest>,
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
    let result: UnifiedResult<UploadSession> = (|| {
        let input = body.into_inner();
        let name = sanitize_file_name(&input.name)?;
        let module_id = valid_module_id(&input.module_id)?;
        let company_id =
            require_company_match(&_tenant, &req, &input.company_id)?.unwrap_or_default();
        if company_id.len() > 128 || company_id.chars().any(|c| c.is_control()) {
            return Err(UnifiedError::bad_request("company_id is not valid"));
        }
        if input.total_size == 0 || input.total_size > state.settings().max_upload_bytes {
            return Err(UnifiedError::new(
                format!(
                    "total_size must be between 1 and {} bytes",
                    state.settings().max_upload_bytes
                ),
                413,
            ));
        }
        if !is_valid_sha256(&input.sha256) {
            return Err(UnifiedError::bad_request(
                "sha256 must be a 64-character lowercase hex digest",
            ));
        }
        let chunk_size = input
            .chunk_size
            .unwrap_or_else(|| state.settings().default_chunk_size)
            .clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE);
        let part_count = ((input.total_size + chunk_size - 1) / chunk_size) as u32;
        // Requested visibility for the completed document; the
        // scan gate may still force `private` at finalize time.
        let visibility = valid_visibility(
            input.visibility.as_deref().unwrap_or("private"),
        )?;
        let document_id = match &input.document_id {
            Some(id) => {
                let document = state.load_document(id)?;
                if document.owner_id != found.actor
                    && !check_capability(&ctx, &found.role, PERM_ADMIN)
                {
                    return Err(UnifiedError::forbidden(
                        "Only the owner or an admin may re-upload this document",
                    ));
                }
                Some(document.id)
            }
            None => None,
        };
        let session_id = new_id();
        // The spool directory is captured at creation, so a later
        // settings change never moves this session's parts. The
        // directory is date-nested by the session creation date
        // (UTC), so the same date is recorded on the session.
        let created_at = now();
        let spool_dir = state.session_spool_dir(&session_id, created_at)?;
        let session = UploadSession {
            id: session_id,
            document_id,
            name,
            module_id,
            company_id,
            owner_id: found.actor.clone(),
            total_size: input.total_size,
            expected_sha256: input.sha256.to_lowercase(),
            chunk_size,
            part_count,
            received_parts: Vec::new(),
            missing_parts: (0..part_count).collect(),
            status: "active".to_string(),
            visibility,
            // Captured now: the finalize worker needs the tenant
            // scope to locate the stored object later.
            tenant_id: Some(_tenant.tenant_id),
            expires_at: now() + chrono::Duration::seconds(state.settings().session_ttl_secs),
            created_at,
            updated_at: now(),
            spool_dir: Some(spool_dir),
        };
        state.sessions.insert(session.id.clone(), session.clone());
        state.parts.insert(session.id.clone(), DashMap::new());
        Ok(session)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_sessions(
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
    let mut items: Vec<UploadSession> = state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|session| in_company_scope(&session.company_id, &_scope))
        .filter(|session| {
            (is_admin || session.owner_id == found.actor)
                && matches_search(&session.name, &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| session.status == *value)
                && params
                    .filters
                    .get("module_id")
                    .is_none_or(|value| session.module_id == *value)
        })
        .map(|session| {
            let empty = DashMap::new();
            let parts = state.parts.get(&session.id).map(|entry| entry.clone());
            match parts {
                Some(parts) => session_status(&session, &parts),
                None => session_status(&session, &empty),
            }
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn get_session(
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
    let result: UnifiedResult<UploadSession> = (|| {
        let session = state
            .sessions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Upload session not found"))?;
        if session.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "You do not have access to this upload session",
            ));
        }
        let empty = DashMap::new();
        let parts = state.parts.get(&session.id).map(|entry| entry.clone());
        Ok(match parts {
            Some(parts) => session_status(&session, &parts),
            None => session_status(&session, &empty),
        })
    })();
    json::respond(result, debug)
}

/// Upload one part: multipart fields `part_index` (0-based),
/// `checksum` (SHA-256 hex of this part's bytes), and `part`
/// (the bytes). A part whose checksum does not match is rejected
/// and not stored, so the client can retry just that part.
pub(crate) async fn upload_part(
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
    let result: UnifiedResult<UploadSession> = async move {
        let session = state
            .sessions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Upload session not found"))?;
        if session.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "You do not have access to this upload session",
            ));
        }
        if session.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Upload session is {} and accepts no more parts",
                session.status
            )));
        }
        if session.expires_at <= Utc::now() {
            return Err(UnifiedError::new(
                "Upload session has expired; initiate a new session",
                410,
            ));
        }
        let mut part_index: Option<u32> = None;
        let mut checksum: Option<String> = None;
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
                "part_index" | "checksum" => {
                    let mut text = String::new();
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        push_multipart_text(&mut text, &chunk)?;
                    }
                    if name == "part_index" {
                        part_index = Some(text.trim().parse().map_err(|_| {
                            UnifiedError::bad_request("part_index must be a non-negative integer")
                        })?);
                    } else {
                        checksum = Some(text.trim().to_lowercase());
                    }
                }
                "part" => {
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        if bytes.len() + chunk.len() > MAX_CHUNK_SIZE as usize {
                            return Err(UnifiedError::new(
                                "Part exceeds the maximum chunk size",
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
        let part_index = part_index.ok_or_else(|| {
            UnifiedError::bad_request("Multipart field \"part_index\" is required")
        })?;
        let checksum = checksum
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"checksum\" is required"))?;
        if part_index >= session.part_count {
            return Err(UnifiedError::bad_request(format!(
                "part_index {part_index} is out of range (session has {} parts)",
                session.part_count
            )));
        }
        if !is_valid_sha256(&checksum) {
            return Err(UnifiedError::bad_request(
                "checksum must be a 64-character lowercase hex digest",
            ));
        }
        if bytes.is_empty() {
            return Err(UnifiedError::bad_request("Part bytes are empty"));
        }
        let actual = sha256_hex(&bytes);
        if actual != checksum {
            return Err(UnifiedError::bad_request(format!(
                "Part {part_index} checksum mismatch: expected {checksum}, got {actual}"
            )));
        }
        let expected_len = if part_index + 1 == session.part_count {
            session.total_size - u64::from(part_index) * session.chunk_size
        } else {
            session.chunk_size
        };
        if bytes.len() as u64 != expected_len {
            return Err(UnifiedError::bad_request(format!(
                "Part {part_index} must be {expected_len} bytes, got {}",
                bytes.len()
            )));
        }
        // The in-RAM cap binds here: a part is accumulated in
        // memory before it is spooled, so it must fit the cap (the
        // 8 MiB chunk cap binds first in practice).
        let max_ram = state.settings().max_chunk_ram_bytes;
        if bytes.len() as u64 > max_ram {
            return Err(UnifiedError::new(
                format!("Part exceeds the {max_ram} byte in-RAM chunk cap"),
                413,
            ));
        }
        let part = state.store_part_bytes(&session, part_index, &bytes, checksum)?;
        state
            .parts
            .entry(session.id.clone())
            .or_default()
            .insert(part_index, part);
        if let Some(mut entry) = state.sessions.get_mut(&session.id) {
            entry.updated_at = now();
        }
        let empty = DashMap::new();
        let parts = state.parts.get(&session.id).map(|entry| entry.clone());
        Ok(match parts {
            Some(parts) => session_status(&session, &parts),
            None => session_status(&session, &empty),
        })
    }
    .await;
    json::respond(result, debug)
}

/// Assemble the parts in order, store the object, scan it, and
/// create the document (or a new version) — WITHOUT hashing the
/// whole file. LAZY FINALIZE: the upload returns as soon as all
/// parts are received; the whole-file SHA-256 runs afterwards in
/// the finalize worker (see `state::finalize`). The session moves
/// to status `received` and a `pending` finalize job is created;
/// the document row carries `checksum: None` until the worker
/// verifies it.
pub(crate) async fn complete_session(
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
    let result: UnifiedResult<DocumentRecord> = (|| async {
        let session = state
            .sessions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Upload session not found"))?;
        if session.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "You do not have access to this upload session",
            ));
        }
        if session.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Upload session is {} and cannot be completed",
                session.status
            )));
        }
        if session.expires_at <= Utc::now() {
            return Err(UnifiedError::new(
                "Upload session has expired; initiate a new session",
                410,
            ));
        }
        // Claim the session atomically: `active` -> `completing`
        // is a compare-and-swap under the session entry's lock, so
        // a second concurrent `complete` gets a 409 instead of
        // finalizing the same parts into a second document. The
        // claim is released (back to `active`) when any step below
        // fails, so a failed completion can be retried.
        state.try_begin_completion(&session.id)?;
        let outcome: UnifiedResult<DocumentRecord> = (|| async {
            let empty = DashMap::new();
            let parts = state.parts.get(&session.id).map(|entry| entry.clone());
            let view = match &parts {
                Some(parts) => session_status(&session, parts),
                None => session_status(&session, &empty),
            };
            if !view.missing_parts.is_empty() {
                return Err(UnifiedError::unprocessable(format!(
                    "Upload session is incomplete; missing parts: {:?}",
                    view.missing_parts
                )));
            }
            // Stream-assemble the spooled parts into a temporary
            // file WITHOUT hashing: the whole-file hash is the
            // finalize worker's job (lazy finalize), not the
            // request path's. The whole file is never in RAM.
            let assembled_path = state.assemble_session_file_unverified(&session)?;
            let result = state
                .finalize_upload_from_file(
                    session.name.clone(),
                    session.module_id.clone(),
                    session.company_id.clone(),
                    session.owner_id.clone(),
                    "owner".to_string(),
                    _tenant.tenant_id,
                    None,
                    &assembled_path,
                    session.total_size,
                    session.expected_sha256.clone(),
                    session.visibility.clone(),
                    session.document_id.clone(),
                )
                .await;
            if result.is_err() {
                let _ = std::fs::remove_file(&assembled_path);
            }
            result
        })()
        .await;
        match outcome {
            Ok(document) => {
                // Received, not finished: the document exists with
                // checksum NULL and a pending finalize job; the
                // worker hashes the stored object off the request
                // path. The spool directory is removed now (the
                // object is already in the store).
                state.mark_session_received(&session.id);
                state.create_finalize_job(&session, Some(document.id.clone()));
                state.start_finalize_worker();
                state.parts.remove(&session.id);
                state.remove_spool_dir(&session);
                Ok(document)
            }
            Err(error) => {
                state.finish_completion(&session.id, false);
                Err(error)
            }
        }
    })()
    .await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Abort a session and delete its partial parts.
pub(crate) async fn abort_session(
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
    let result: UnifiedResult<Value> = (|| {
        let session = state
            .sessions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Upload session not found"))?;
        if session.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "You do not have access to this upload session",
            ));
        }
        // A completed session (or one mid-completion) cannot be
        // aborted: its document is already finalized. Anything
        // else is removed together with its partial parts.
        state.remove_session_if_abortable(path.as_str())?;
        Ok(json!({ "aborted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn purge_expired_sessions(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
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
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let purged = state.purge_expired_sessions();
    json::success_response(json!({ "purged": purged }), 200)
}
