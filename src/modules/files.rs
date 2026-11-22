// File Manager module.
//
// Folders and files with metadata, multipart upload under
// `data/files/<folder>/<record-id>`, streaming download, move and delete.
// Path traversal is rejected, uploads are capped at 100 MB, and the MIME
// type is detected from content with the `infer` crate. The `file.shared`
// hook (from Chat) registers a shared file. Interconnection happens only
// through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_multipart::Multipart;
use actix_web::{web, HttpResponse};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::{stream::unfold, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub name: String,
    /// `file` or `folder`.
    pub kind: String,
    pub size: u64,
    pub mime: String,
    /// Parent folder id; empty means the root.
    pub folder_id: String,
    /// Relative path on disk, or the shared URL for shared files.
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFolder {
    pub name: String,
    pub folder_id: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveRequest {
    pub folder_id: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct FilesState {
    hooks: RwLock<Arc<HookBus>>,
    records: DashMap<String, FileRecord>,
    base_dir: PathBuf,
}

impl FilesState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            records: DashMap::new(),
            base_dir: PathBuf::from("data/files"),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// True when `folder_id` is the root or an existing folder record.
    fn folder_exists(&self, folder_id: &str) -> bool {
        folder_id.is_empty()
            || self
                .records
                .get(folder_id)
                .is_some_and(|entry| entry.value().kind == "folder")
    }

    /// Disk path for a file record: `data/files/<folder>/<record-id>`.
    fn disk_path(&self, folder_id: &str, id: &str) -> PathBuf {
        if folder_id.is_empty() {
            self.base_dir.join(id)
        } else {
            self.base_dir.join(folder_id).join(id)
        }
    }

    /// All descendant file records of a folder (recursive, by folder id).
    fn descendant_files(&self, folder_id: &str) -> Vec<FileRecord> {
        let mut out = Vec::new();
        let mut pending: Vec<String> = vec![folder_id.to_string()];
        while let Some(current) = pending.pop() {
            for entry in self.records.iter() {
                let record = entry.value();
                if record.folder_id == current {
                    if record.kind == "folder" {
                        pending.push(record.id.clone());
                    } else {
                        out.push(record.clone());
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Strip any directory components from an uploaded name. Returns an error
/// when nothing usable remains (for example `..` or `/`).
fn sanitize_file_name(raw: &str) -> UnifiedResult<String> {
    let name = Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." {
        return Err(UnifiedError::bad_request(
            "Uploaded file name is not usable",
        ));
    }
    Ok(name)
}

fn mime_for(name: &str, bytes: &[u8]) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_string();
    }
    match Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "txt" => "text/plain".to_string(),
        "html" | "htm" => "text/html".to_string(),
        "css" => "text/css".to_string(),
        "js" | "mjs" => "text/javascript".to_string(),
        "json" => "application/json".to_string(),
        "pdf" => "application/pdf".to_string(),
        "zip" => "application/zip".to_string(),
        "md" => "text/markdown".to_string(),
        "csv" => "text/csv".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_files(
    state: web::Data<Arc<FilesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<FileRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            matches_search(&record.name, &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| record.kind == *kind)
                && params.filters.get("folder_id").is_none_or(|folder_id| {
                    record.folder_id == *folder_id
                })
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

async fn create_folder(
    state: web::Data<Arc<FilesState>>,
    body: web::Json<CreateFolder>,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| {
        let input = body.into_inner();
        // This endpoint creates folders; files arrive through /upload.
        if let Some(kind) = &input.kind {
            if kind != "folder" {
                return Err(UnifiedError::unprocessable(
                    "Files must be uploaded through /api/v1/files/upload",
                ));
            }
        }
        let name = input.name.trim().to_string();
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return Err(UnifiedError::bad_request(
                "Folder name must not be empty and must not contain path separators",
            ));
        }
        let folder_id = input.folder_id.unwrap_or_default();
        if !state.folder_exists(&folder_id) {
            return Err(UnifiedError::not_found("Parent folder not found"));
        }
        let record = FileRecord {
            id: new_id(),
            name,
            kind: "folder".to_string(),
            size: 0,
            mime: String::new(),
            folder_id,
            path: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        state.records.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    respond_created(result)
}

async fn get_file(
    state: web::Data<Arc<FilesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = state
        .records
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("File not found"));
    json::respond(result, false)
}

async fn upload_file(
    state: web::Data<Arc<FilesState>>,
    mut payload: Multipart,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| async {
        let mut folder_id = String::new();
        let mut file_name: Option<String> = None;
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(mut field) = payload
            .try_next()
            .await
            .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
        {
            let name = field
                .content_disposition()
                .get_name()
                .unwrap_or("")
                .to_string();
            match name.as_str() {
                "folder_id" => {
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        folder_id.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
                "file" => {
                    let raw_name = field
                        .content_disposition()
                        .get_filename()
                        .unwrap_or("upload")
                        .to_string();
                    file_name = Some(sanitize_file_name(&raw_name)?);
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        if bytes.len() + chunk.len() > MAX_UPLOAD_BYTES {
                            return Err(UnifiedError::new(
                                "Upload exceeds the 100 MB size limit",
                                413,
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                }
                _ => {
                    // Drain unknown fields so the stream stays well-formed.
                    while let Some(_chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })?
                    {}
                }
            }
        }
        let name = file_name
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"file\" is required"))?;
        let folder_id = folder_id.trim().to_string();
        if !state.folder_exists(&folder_id) {
            return Err(UnifiedError::not_found("Target folder not found"));
        }
        let id = new_id();
        let disk_path = state.disk_path(&folder_id, &id);
        if let Some(parent) = disk_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| UnifiedError::internal(format!("Cannot store upload: {error}")))?;
        }
        tokio::fs::write(&disk_path, &bytes)
            .await
            .map_err(|error| UnifiedError::internal(format!("Cannot store upload: {error}")))?;
        let record = FileRecord {
            id: id.clone(),
            name: name.clone(),
            kind: "file".to_string(),
            size: bytes.len() as u64,
            mime: mime_for(&name, &bytes),
            folder_id,
            path: disk_path.to_string_lossy().to_string(),
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
    respond_created(result)
}

async fn download_file(
    state: web::Data<Arc<FilesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let record = match state
        .records
        .get(path.as_str())
        .map(|entry| entry.value().clone())
    {
        Some(record) => record,
        None => return json::error_response(&UnifiedError::not_found("File not found"), false),
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
        .append_header((
            "Content-Disposition",
            format!("attachment; filename=\"{name}\""),
        ))
        .streaming(stream)
}

async fn delete_file(
    state: web::Data<Arc<FilesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| async {
        let record = state
            .records
            .get(path.as_str())
            .map(|entry| entry.value().clone())
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
    json::respond(result, false)
}

async fn move_file(
    state: web::Data<Arc<FilesState>>,
    path: web::Path<String>,
    body: web::Json<MoveRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FileRecord> = (|| async {
        let target = body.into_inner().folder_id.trim().to_string();
        if !state.folder_exists(&target) {
            return Err(UnifiedError::not_found("Target folder not found"));
        }
        let record = state
            .records
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("File not found"))?;
        if record.kind == "folder" {
            if target == record.id {
                return Err(UnifiedError::unprocessable(
                    "A folder cannot be moved into itself",
                ));
            }
            // Refuse moving a folder into one of its own descendants.
            let descendants = state.descendant_files(&record.id);
            let mut descendant_folder_ids: Vec<String> = Vec::new();
            let mut pending: Vec<String> = vec![record.id.clone()];
            while let Some(current) = pending.pop() {
                for entry in state.records.iter() {
                    let child = entry.value();
                    if child.folder_id == current && child.kind == "folder" {
                        descendant_folder_ids.push(child.id.clone());
                        pending.push(child.id.clone());
                    }
                }
            }
            if descendant_folder_ids.contains(&target) {
                return Err(UnifiedError::unprocessable(
                    "A folder cannot be moved into its own descendant",
                ));
            }
            // Move every descendant file on disk to the new folder.
            for file in descendants {
                let old_path = state.disk_path(&file.folder_id, &file.id);
                let new_path = state.disk_path(&target, &file.id);
                if old_path.exists() {
                    if let Some(parent) = new_path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|error| {
                            UnifiedError::internal(format!("Cannot move file: {error}"))
                        })?;
                    }
                    tokio::fs::rename(&old_path, &new_path).await.map_err(|error| {
                        UnifiedError::internal(format!("Cannot move file: {error}"))
                    })?;
                }
                if let Some(mut entry) = state.records.get_mut(&file.id) {
                    entry.path = new_path.to_string_lossy().to_string();
                    entry.updated_at = now();
                }
            }
        } else if record.kind == "file" {
            let old_path = state.disk_path(&record.folder_id, &record.id);
            let new_path = state.disk_path(&target, &record.id);
            if old_path.exists() {
                if let Some(parent) = new_path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|error| {
                        UnifiedError::internal(format!("Cannot move file: {error}"))
                    })?;
                }
                tokio::fs::rename(&old_path, &new_path).await.map_err(|error| {
                    UnifiedError::internal(format!("Cannot move file: {error}"))
                })?;
            }
        }
        let mut updated = record.clone();
        updated.folder_id = target;
        if updated.kind == "file" {
            updated.path = state
                .disk_path(&updated.folder_id, &updated.id)
                .to_string_lossy()
                .to_string();
        }
        updated.updated_at = now();
        state.records.insert(updated.id.clone(), updated.clone());
        Ok(updated)
    })()
    .await;
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_file_shared(state: Arc<FilesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if let Some(file_id) = data.get("file_id").and_then(|v| v.as_str()) {
        if state.records.contains_key(file_id) {
            ctx.add_result(json!({ "file_id": file_id, "already_registered": true }));
            return;
        }
    }
    let name = data
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("shared-file")
        .to_string();
    let folder_id = data
        .get("folder_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !state.folder_exists(&folder_id) {
        ctx.add_result(json!({ "error": "Target folder not found" }));
        return;
    }
    let id = data
        .get("file_id")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(new_id);
    let record = FileRecord {
        id,
        name,
        kind: "file".to_string(),
        size: data.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        mime: data
            .get("mime")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream")
            .to_string(),
        folder_id,
        path: data
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: now(),
        updated_at: now(),
    };
    state.records.insert(record.id.clone(), record.clone());
    ctx.add_result(serde_json::to_value(&record).unwrap_or(Value::Null));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct FilesModule {
    state: Arc<FilesState>,
}

impl FilesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(FilesState::new()),
        }
    }
}

impl Default for FilesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FilesModule {
    fn id(&self) -> &'static str {
        "files"
    }

    fn name(&self) -> &'static str {
        "File Manager"
    }

    fn description(&self) -> &'static str {
        "File manager: folders, uploads, streaming downloads, move and delete."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_file_shared(state.clone(), ctx));
        bus.register("file.shared", "files", handler);
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/files", owner: "files", description: "List files" },
            RouteInfo { method: "POST", path: "/api/v1/files", owner: "files", description: "Create folder" },
            RouteInfo { method: "POST", path: "/api/v1/files/upload", owner: "files", description: "Upload file" },
            RouteInfo { method: "GET", path: "/api/v1/files/{id}", owner: "files", description: "Get file" },
            RouteInfo { method: "DELETE", path: "/api/v1/files/{id}", owner: "files", description: "Delete file" },
            RouteInfo { method: "GET", path: "/api/v1/files/{id}/download", owner: "files", description: "Download file" },
            RouteInfo { method: "POST", path: "/api/v1/files/{id}/move", owner: "files", description: "Move file" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/files")
                .route("", web::get().to(list_files))
                .route("", web::post().to(create_folder))
                .route("/upload", web::post().to(upload_file))
                .route("/{id}", web::get().to(get_file))
                .route("/{id}", web::delete().to(delete_file))
                .route("/{id}/download", web::get().to(download_file))
                .route("/{id}/move", web::post().to(move_file)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FilesModule::new()));
}
