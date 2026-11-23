//! files module - HTTP handlers for files and folders.
//!
//! Every handler requires an authenticated caller: `X-User-Id` and
//! `X-Role` headers, plus a `files.view` (reads) or `files.manage`
//! (writes) capability answered through the `role.check` hook, failing
//! closed when the hook has no answer. Records are scoped to the
//! caller's tenant (see `resolve_caller_company` in types.rs);
//! only an explicitly-decided platform scope (control-plane request
//! by a verified admin with no resolvable company) may access any
//! tenant — a caller with no company is unresolved and reads global
//! rows only — and a manage capability never widens the tenant scope.

use actix_multipart::Multipart;
use actix_web::{HttpRequest, HttpResponse, web};
use bytes::Bytes;
use futures_util::TryStreamExt;
use futures_util::stream::unfold;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;

use super::helpers::content_disposition;
use super::helpers::matches_search;
use super::helpers::mime_for;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::paginate;
use super::helpers::require_tenant;
use super::helpers::respond_created;
use super::helpers::sanitize_file_name;
use super::helpers::validate_folder_name;
use super::types::CreateFolder;
use super::types::FileRecord;
use super::types::FilesState;
use super::types::MAX_UPLOAD_BYTES;
use super::types::MIME_SNIFF_LEN;
use super::types::MoveRequest;
use super::types::PERM_FILES_MANAGE;
use super::types::PERM_FILES_VIEW;
use super::types::caller;
use super::types::require_permission;
use super::types::resolve_caller_company;
use unified_core::tenancy::CallerScope;

/// Authenticate the caller, require `permission`, and return the
/// caller's resolved company scope (see `CallerScope`: platform is
/// an explicitly-decided scope, never the absence of a company).
/// The tenant gate runs first and fails closed: a request with no
/// resolved tenant context is rejected before any identity or
/// capability check, so it can never proceed as Unresolved.
fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<CallerScope> {
    require_tenant(req)?;
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)?;
    Ok(resolve_caller_company(ctx, req, &caller))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_files(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller_company = match authorize(&ctx, &req, PERM_FILES_VIEW) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let mut items: Vec<FileRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            caller_company.can_access_record(&record.company_id)
                && matches_search(&record.name, &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| record.kind == *kind)
                && params
                    .filters
                    .get("folder_id")
                    .is_none_or(|folder_id| record.folder_id == *folder_id)
                && params
                    .filters
                    .get("mime")
                    .is_none_or(|mime| record.mime == *mime)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    body: web::Json<CreateFolder>,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| {
        let caller_company = authorize(&ctx, &req, PERM_FILES_MANAGE)?;
        let input = body.into_inner();
        // This endpoint creates folders; files arrive through /upload.
        if let Some(kind) = &input.kind {
            if kind != "folder" {
                return Err(UnifiedError::unprocessable(
                    "Files must be uploaded through /api/v1/files/upload",
                ));
            }
        }
        let name = validate_folder_name(&input.name)?;
        let folder_id = input.folder_id.unwrap_or_default();
        if !state.folder_accessible(&folder_id, &caller_company) {
            return Err(UnifiedError::not_found("Parent folder not found"));
        }
        let id = new_id();
        let record = FileRecord {
            id: id.clone(),
            name,
            kind: "folder".to_string(),
            size: 0,
            mime: String::new(),
            folder_id: folder_id.clone(),
            // Folders carry the same derived path as files, so `path`
            // stays derivable from `folder_id` after a move.
            path: state
                .disk_path(&folder_id, &id)?
                .to_string_lossy()
                .to_string(),
            company_id: caller_company.stored_company_id().to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.records.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| {
        let caller_company = authorize(&ctx, &req, PERM_FILES_VIEW)?;
        state
            .records
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .filter(|record| caller_company.can_access_record(&record.company_id))
            .ok_or_else(|| UnifiedError::not_found("File not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Stream a multipart upload to a temp file, returning the file name,
/// folder id, total size, and the sniffed leading bytes used for MIME
/// detection. Chunks are written to disk as they arrive instead of
/// being buffered in memory, so concurrent large uploads cannot
/// multiply memory use. On error the partial temp file is removed.
async fn stream_upload_to_temp(
    tmp_path: &Path,
    mut payload: Multipart,
) -> UnifiedResult<(String, String, u64, Vec<u8>)> {
    let mut out: Option<File> = None;
    let mut folder_id = String::new();
    let mut file_name: Option<String> = None;
    let mut total: u64 = 0;
    let mut sniff: Vec<u8> = Vec::new();
    let result: UnifiedResult<(String, String, u64, Vec<u8>)> = (|| async {
        while let Some(mut field) = payload.try_next().await.map_err(|error| {
            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
        })? {
            let name = field
                .content_disposition()
                .and_then(|cd| cd.get_name())
                .unwrap_or("")
                .to_string();
            match name.as_str() {
                "folder_id" => {
                    while let Some(chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })? {
                        folder_id.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
                "file" => {
                    if file_name.is_some() {
                        // A second file field is drained; the first one wins.
                        while let Some(_chunk) = field.try_next().await.map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })? {}
                        continue;
                    }
                    let raw_name = field
                        .content_disposition()
                        .and_then(|cd| cd.get_filename())
                        .unwrap_or("upload")
                        .to_string();
                    file_name = Some(sanitize_file_name(&raw_name)?);
                    while let Some(chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })? {
                        total += chunk.len() as u64;
                        if total > MAX_UPLOAD_BYTES as u64 {
                            return Err(UnifiedError::new(
                                "Upload exceeds the 100 MB size limit",
                                413,
                            ));
                        }
                        if sniff.len() < MIME_SNIFF_LEN {
                            let take = (MIME_SNIFF_LEN - sniff.len()).min(chunk.len());
                            sniff.extend_from_slice(&chunk[..take]);
                        }
                        if out.is_none() {
                            if let Some(parent) = tmp_path.parent() {
                                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                                    UnifiedError::internal(format!("Cannot store upload: {error}"))
                                })?;
                            }
                            out =
                                Some(tokio::fs::File::create(tmp_path).await.map_err(|error| {
                                    UnifiedError::internal(format!("Cannot store upload: {error}"))
                                })?);
                        }
                        out.as_mut()
                            .expect("upload output is open")
                            .write_all(&chunk)
                            .await
                            .map_err(|error| {
                                UnifiedError::internal(format!("Cannot store upload: {error}"))
                            })?;
                    }
                }
                _ => {
                    // Drain unknown fields so the stream stays well-formed.
                    while let Some(_chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })? {}
                }
            }
        }
        if out.is_none() && file_name.is_some() {
            // An empty upload still materialises its temp file so the
            // caller can rename it into place.
            if let Some(parent) = tmp_path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    UnifiedError::internal(format!("Cannot store upload: {error}"))
                })?;
            }
            out = Some(tokio::fs::File::create(tmp_path).await.map_err(|error| {
                UnifiedError::internal(format!("Cannot store upload: {error}"))
            })?);
        }
        if let Some(file) = out.as_mut() {
            file.flush()
                .await
                .map_err(|error| UnifiedError::internal(format!("Cannot store upload: {error}")))?;
        }
        let name = file_name
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"file\" is required"))?;
        Ok((name, folder_id.trim().to_string(), total, sniff))
    })()
    .await;
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if out.is_some() {
                let _ = tokio::fs::remove_file(tmp_path).await;
            }
            Err(error)
        }
    }
}

pub(crate) async fn upload_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    payload: Multipart,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| async {
        let caller_company = authorize(&ctx, &req, PERM_FILES_MANAGE)?;
        let id = new_id();
        // Stream to a temp file first: the target folder is only known
        // once the whole multipart body has been read, and the temp
        // file is renamed into place only after the folder checks out.
        let tmp_path = state.base_dir.join(".tmp").join(format!("{id}.part"));
        let (name, folder_id, total, sniff) = stream_upload_to_temp(&tmp_path, payload).await?;
        if !state.folder_accessible(&folder_id, &caller_company) {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(UnifiedError::not_found("Target folder not found"));
        }
        let disk_path = state.disk_path(&folder_id, &id)?;
        if let Some(parent) = disk_path.parent() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(UnifiedError::internal(format!(
                    "Cannot store upload: {error}"
                )));
            }
        }
        if let Err(error) = tokio::fs::rename(&tmp_path, &disk_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(UnifiedError::internal(format!(
                "Cannot store upload: {error}"
            )));
        }
        let record = FileRecord {
            id: id.clone(),
            name: name.clone(),
            kind: "file".to_string(),
            size: total,
            mime: mime_for(&name, &sniff),
            folder_id,
            path: disk_path.to_string_lossy().to_string(),
            company_id: caller_company.stored_company_id().to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.records.insert(id, record.clone());
        state.emit(
            "file.uploaded",
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        Ok(record)
    })()
    .await;
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn download_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let caller_company = match authorize(&ctx, &req, PERM_FILES_VIEW) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let record = match state
        .records
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|record| caller_company.can_access_record(&record.company_id))
    {
        Some(record) => record,
        None => {
            return json::error_response(
                &UnifiedError::not_found("File not found"),
                ctx.is_debug(),
            );
        }
    };
    if record.kind != "file" {
        return json::error_response(
            &UnifiedError::unprocessable("Folders cannot be downloaded"),
            false,
        );
    }
    let disk_path = PathBuf::from(&record.path);
    let file = match File::open(&disk_path).await {
        Ok(file) => file,
        Err(_) => {
            return json::error_response(
                &UnifiedError::not_found("Stored file is missing on disk"),
                false,
            );
        }
    };
    let mime = record.mime.clone();
    let name = record.name.clone();
    let stream = unfold(file, |mut file| async move {
        let mut buffer = vec![0u8; 65_536];
        match file.read(&mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buffer)), file))
            }
            Err(error) => Some((Err(error), file)),
        }
    });
    HttpResponse::Ok()
        .content_type(mime)
        .append_header(("Content-Disposition", content_disposition(&name)))
        .streaming(stream)
}

pub(crate) async fn delete_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| async {
        let caller_company = authorize(&ctx, &req, PERM_FILES_MANAGE)?;
        let record = state
            .records
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .filter(|record| caller_company.can_access_record(&record.company_id))
            .ok_or_else(|| UnifiedError::not_found("File not found"))?;
        if record.kind == "folder" {
            let has_children = state
                .records
                .iter()
                .any(|entry| entry.value().folder_id == record.id);
            if has_children {
                return Err(UnifiedError::conflict(
                    "Folder is not empty; move or delete its contents first",
                ));
            }
        }
        if record.kind == "file" {
            let disk_path = PathBuf::from(&record.path);
            if disk_path.exists() {
                tokio::fs::remove_file(&disk_path).await.map_err(|error| {
                    UnifiedError::internal(format!("Cannot remove stored file: {error}"))
                })?;
            }
        }
        state.records.remove(path.as_str());
        state.emit(
            "file.deleted",
            json!({ "id": path.as_str(), "kind": record.kind, "name": record.name }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })()
    .await;
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn move_file(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<FilesState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| async {
        let caller_company = authorize(&ctx, &req, PERM_FILES_MANAGE)?;
        let target = body.into_inner().folder_id.trim().to_string();
        if !state.folder_accessible(&target, &caller_company) {
            return Err(UnifiedError::not_found("Target folder not found"));
        }
        let record = state
            .records
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .filter(|record| caller_company.can_access_record(&record.company_id))
            .ok_or_else(|| UnifiedError::not_found("File not found"))?;
        if record.kind == "folder" {
            if target == record.id {
                return Err(UnifiedError::unprocessable(
                    "A folder cannot be moved into itself",
                ));
            }
            // Refuse moving a folder into one of its own descendants.
            let descendant_folders = state.descendant_folders(&record.id);
            if descendant_folders.iter().any(|folder| folder.id == target) {
                return Err(UnifiedError::unprocessable(
                    "A folder cannot be moved into its own descendant",
                ));
            }
            // Only the moved folder's own id is remapped to the target.
            // A descendant keeps its parent folder id (and its disk
            // path) unless its parent IS the moved folder; flattening
            // every descendant into the target would lose the nesting
            // and desync the stored path from disk_path(folder_id, id).
            let remap = |folder_id: &str| -> String {
                if folder_id == record.id {
                    target.clone()
                } else {
                    folder_id.to_string()
                }
            };
            for file in state.descendant_files(&record.id) {
                let new_folder_id = remap(&file.folder_id);
                if new_folder_id == file.folder_id {
                    continue;
                }
                let old_path = state.disk_path(&file.folder_id, &file.id)?;
                let new_path = state.disk_path(&new_folder_id, &file.id)?;
                if old_path.exists() {
                    if let Some(parent) = new_path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|error| {
                            UnifiedError::internal(format!("Cannot move file: {error}"))
                        })?;
                    }
                    tokio::fs::rename(&old_path, &new_path)
                        .await
                        .map_err(|error| {
                            UnifiedError::internal(format!("Cannot move file: {error}"))
                        })?;
                }
                if let Some(mut entry) = state.records.get_mut(&file.id) {
                    entry.folder_id = new_folder_id.clone();
                    // Shared files carry an external URL in `path`;
                    // only recompute the path when it was disk-derived.
                    // (`old_path` above is exactly
                    // `disk_path(file.folder_id, file.id)`.)
                    if old_path.to_string_lossy().into_owned() == file.path {
                        entry.path = new_path.to_string_lossy().to_string();
                    }
                    entry.updated_at = now();
                }
            }
            // Child folders move with the folder: only their parent id
            // changes (to the target); deeper folders keep theirs.
            // Their stored path is recomputed too, so `path` stays
            // derivable from `folder_id` for folders as well as files.
            for folder in descendant_folders {
                let new_folder_id = remap(&folder.folder_id);
                if new_folder_id == folder.folder_id {
                    continue;
                }
                if let Some(mut entry) = state.records.get_mut(&folder.id) {
                    entry.folder_id = new_folder_id.clone();
                    entry.path = state
                        .disk_path(&new_folder_id, &folder.id)?
                        .to_string_lossy()
                        .to_string();
                    entry.updated_at = now();
                }
            }
            // Best-effort removal of the moved folder's old directory,
            // now empty after its direct files moved out.
            let old_dir = state.disk_path("", &record.id)?;
            let _ = tokio::fs::remove_dir(&old_dir).await;
        } else if record.kind == "file" {
            let old_path = state.disk_path(&record.folder_id, &record.id)?;
            let new_path = state.disk_path(&target, &record.id)?;
            if old_path.exists() {
                if let Some(parent) = new_path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|error| {
                        UnifiedError::internal(format!("Cannot move file: {error}"))
                    })?;
                }
                tokio::fs::rename(&old_path, &new_path)
                    .await
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot move file: {error}"))
                    })?;
            }
        }
        let mut updated = record.clone();
        updated.folder_id = target.clone();
        if updated.kind == "folder" {
            updated.path = state
                .disk_path(&target, &updated.id)
                .to_string_lossy()
                .to_string();
        } else if state
            .disk_path(&record.folder_id, &record.id)
            .to_string_lossy()
            .into_owned()
            == record.path
        {
            // Locally stored file: keep `path` derived from
            // `folder_id`. Shared files carry an external URL in
            // `path`; leave that untouched.
            updated.path = state
                .disk_path(&target, &updated.id)
                .to_string_lossy()
                .to_string();
        }
        updated.updated_at = now();
        state.records.insert(updated.id.clone(), updated.clone());
        Ok(updated)
    })()
    .await;
    json::respond(result, ctx.is_debug())
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::{TestRequest, read_body};
    use chrono::Utc;
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;

    /// Context whose `role.check` hook grants exactly `allowed`.
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let held: Vec<String> = allowed.iter().map(|item| item.to_string()).collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(held.iter().any(|item| item == permission)),
                );
            }),
        );
        // Company resolution for the test caller: `resolve_caller_company`
        // asks the `user.company_requested` hook, never a header. The
        // test tenant ("tester") belongs to "company-a", so the
        // tenant-scoping tests see their own records only.
        let tester = test_user_uuid("tester").to_string();
        hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let user_id = ctx
                    .get("user_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if user_id == tester {
                    ctx.set("company_id", serde_json::json!("company-a"));
                }
            }),
        );
        let ctx = AppContext::new(AppConfig::default(), hooks, Arc::new(ModuleRegistry::new()));
        web::Data::new(Arc::new(ctx))
    }

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    /// A request for the test tenant: the verified caller is the
    /// tester with the admin role. The company ("company-a") comes
    /// from the `user.company_requested` hook registered by
    /// `test_ctx`, never from a header.
    fn tenant_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn state_data() -> web::Data<Arc<FilesState>> {
        web::Data::new(Arc::new(FilesState::new()))
    }

    fn seed_record(
        state: &FilesState,
        id: &str,
        kind: &str,
        folder_id: &str,
        company_id: &str,
    ) -> FileRecord {
        let record = FileRecord {
            id: id.to_string(),
            name: format!("{id}.txt"),
            kind: kind.to_string(),
            size: 0,
            mime: String::new(),
            folder_id: folder_id.to_string(),
            path: state.disk_path(folder_id, id).to_string_lossy().to_string(),
            company_id: company_id.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.records.insert(id.to_string(), record.clone());
        record
    }

    #[tokio::test]
    async fn list_rejects_anonymous_callers() {
        let resp = list_files(
            test_ctx(&[PERM_FILES_VIEW]),
            state_data(),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_rejects_callers_without_view() {
        let resp = list_files(
            test_ctx(&[]),
            state_data(),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_scopes_records_to_the_caller_tenant() {
        let state = state_data();
        seed_record(&state, "own", "file", "", "company-a");
        seed_record(&state, "theirs", "file", "", "company-b");
        seed_record(&state, "legacy", "file", "", "");
        let resp = list_files(
            test_ctx(&[PERM_FILES_VIEW]),
            state.clone(),
            tenant_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
        assert!(body.contains("\"own\""));
        assert!(body.contains("\"legacy\""));
        assert!(!body.contains("\"theirs\""));
    }

    #[tokio::test]
    async fn get_hides_other_tenant_records() {
        let state = state_data();
        seed_record(&state, "theirs", "file", "", "company-b");
        let resp = get_file(
            test_ctx(&[PERM_FILES_VIEW]),
            state,
            tenant_request(),
            web::Path::from("theirs".to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_folder_rejects_overlong_names() {
        let resp = create_folder(
            test_ctx(&[PERM_FILES_MANAGE]),
            state_data(),
            authed_request(),
            web::Json(CreateFolder {
                name: "x".repeat(121),
                folder_id: None,
                kind: Some("folder".to_string()),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_folder_stamps_the_caller_tenant() {
        let state = state_data();
        let resp = create_folder(
            test_ctx(&[PERM_FILES_MANAGE]),
            state.clone(),
            tenant_request(),
            web::Json(CreateFolder {
                name: "Docs".to_string(),
                folder_id: None,
                kind: Some("folder".to_string()),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
        assert!(body.contains("\"company_id\":\"company-a\""));
        let created: Value = serde_json::from_str(&body).unwrap();
        let id = created["data"]["id"].as_str().unwrap().to_string();
        let stored = state.records.get(&id).unwrap();
        assert_eq!(
            stored.path,
            state.disk_path("", &id).to_string_lossy().to_string()
        );
    }

    #[tokio::test]
    async fn download_header_is_injection_safe() {
        let mut raw = FilesState::new();
        let base = std::env::temp_dir().join(format!("files-download-test-{}", new_id()));
        raw.base_dir = base.clone();
        let state = Arc::new(raw);
        let disk_path = base.join("r1");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&disk_path, b"contents").unwrap();
        let record = FileRecord {
            id: "r1".to_string(),
            name: "we\"ird\r\nX: y.txt".to_string(),
            kind: "file".to_string(),
            size: 8,
            mime: "text/plain".to_string(),
            folder_id: String::new(),
            path: disk_path.to_string_lossy().to_string(),
            company_id: String::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.records.insert("r1".to_string(), record);
        let resp = download_file(
            test_ctx(&[PERM_FILES_VIEW]),
            web::Data::new(state),
            authed_request(),
            web::Path::from("r1".to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let disposition = resp
            .headers()
            .get("Content-Disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(!disposition.contains('\r'));
        assert!(!disposition.contains('\n'));
        assert_eq!(
            disposition,
            "attachment; filename=\"we_ird__X:_y.txt\"; \
             filename*=UTF-8''we%22ird%0D%0AX%3A%20y.txt"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn move_folder_keeps_descendant_nesting() {
        let mut raw = FilesState::new();
        let base = std::env::temp_dir().join(format!("files-move-test-{}", new_id()));
        raw.base_dir = base.clone();
        let state = Arc::new(raw);
        // Layout: a/ (f1), a/b/ (f2); target folder t at the root.
        seed_record(&state, "a", "folder", "", "");
        seed_record(&state, "b", "folder", "a", "");
        seed_record(&state, "t", "folder", "", "");
        seed_record(&state, "f1", "file", "a", "");
        seed_record(&state, "f2", "file", "b", "");
        for (folder, id) in [("a", "f1"), ("b", "f2")] {
            let path = state.disk_path(folder, id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("contents of {id}")).unwrap();
        }
        let resp = move_file(
            test_ctx(&[PERM_FILES_MANAGE]),
            web::Data::new(state.clone()),
            authed_request(),
            web::Path::from("a".to_string()),
            web::Json(MoveRequest {
                folder_id: "t".to_string(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Direct child file moved to the target; its path stays derived.
        let f1 = state.records.get("f1").unwrap();
        assert_eq!(f1.folder_id, "t");
        assert_eq!(
            f1.path,
            state.disk_path("t", "f1").to_string_lossy().to_string()
        );
        assert!(state.disk_path("t", "f1").exists());
        assert!(!state.disk_path("a", "f1").exists());

        // Nested file untouched: still in b, still on disk where it was.
        let f2 = state.records.get("f2").unwrap();
        assert_eq!(f2.folder_id, "b");
        assert_eq!(
            f2.path,
            state.disk_path("b", "f2").to_string_lossy().to_string()
        );
        assert!(state.disk_path("b", "f2").exists());

        // Child folder remapped to the target, path recomputed too.
        let b = state.records.get("b").unwrap();
        assert_eq!(b.folder_id, "t");
        assert_eq!(
            b.path,
            state.disk_path("t", "b").to_string_lossy().to_string()
        );

        // The moved folder itself: parent and path recomputed.
        let a = state.records.get("a").unwrap();
        assert_eq!(a.folder_id, "t");
        assert_eq!(
            a.path,
            state.disk_path("t", "a").to_string_lossy().to_string()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn move_folder_into_descendant_is_refused() {
        let state = state_data();
        seed_record(&state, "a", "folder", "", "");
        seed_record(&state, "b", "folder", "a", "");
        let resp = move_file(
            test_ctx(&[PERM_FILES_MANAGE]),
            state,
            authed_request(),
            web::Path::from("a".to_string()),
            web::Json(MoveRequest {
                folder_id: "b".to_string(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn delete_rejects_other_tenant_records() {
        let state = state_data();
        seed_record(&state, "theirs", "file", "", "company-b");
        let resp = delete_file(
            test_ctx(&[PERM_FILES_MANAGE]),
            state,
            tenant_request(),
            web::Path::from("theirs".to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn upload_streams_chunks_to_disk() {
        use actix_web::App;
        use actix_web::test::{call_service, init_service};
        let mut raw = FilesState::new();
        let base = std::env::temp_dir().join(format!("files-upload-test-{}", new_id()));
        raw.base_dir = base.clone();
        let state = Arc::new(raw);
        let app = init_service(
            App::new()
                .app_data(test_ctx(&[PERM_FILES_MANAGE]))
                .app_data(web::Data::new(state.clone()))
                .route("/api/v1/files/upload", web::post().to(upload_file)),
        )
        .await;
        let boundary = "----files-test-boundary";
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"folder_id\"\r\n\r\n\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\n\
             Content-Type: text/plain\r\n\r\nhello world\r\n--{b}--\r\n",
            b = boundary
        );
        let req = TestRequest::post()
            .uri("/api/v1/files/upload")
            .insert_header((
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            ))
            .set_payload(Bytes::from(body))
            .to_request();
        // The service request carries the verified caller in its
        // extensions, exactly as the auth middleware would attach
        // it; the company comes from the `user.company_requested`
        // hook in `test_ctx`, never from a header.
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("tester"), Vec::new())
                .with_roles(vec!["admin".to_string()]),
        );
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
        let created: Value = serde_json::from_str(&body).unwrap();
        let id = created["data"]["id"].as_str().unwrap().to_string();
        assert_eq!(created["data"]["size"], 11);
        assert_eq!(created["data"]["mime"], "text/plain");
        assert_eq!(created["data"]["company_id"], "company-a");
        let stored = std::fs::read(state.disk_path("", &id)).unwrap();
        assert_eq!(stored, b"hello world");
        let _ = std::fs::remove_dir_all(&base);
    }
}
