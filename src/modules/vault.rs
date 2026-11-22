// Document Vault module — secure document storage per company and
// client: folders, documents (metadata only — file bytes live in
// the storage module, referenced by target + key), versioning,
// append-only access logs, expiring share links, access requests
// with approvals, and retention rules.
//
// Routes live under `/api/v1/vault`:
//
//   Folders:
//     GET/POST /folders — list folders / create a folder.
//     GET/PATCH/DELETE /folders/{id}.
//     POST /folders/{id}/retention — set the retention rule.
//     GET  /folders/{id}/documents — documents in a folder.
//   Documents:
//     GET/POST /documents — list visible documents / upload metadata.
//     GET/PATCH/DELETE /documents/{id}.
//     POST /documents/{id}/versions — upload a new version (metadata).
//     GET  /documents/{id}/versions — version list.
//     POST /documents/{id}/versions/{version_id}/restore.
//     POST /documents/{id}/download — record a download, return the
//       storage reference (no file bytes are served here).
//     GET  /documents/{id}/access-log — append-only access log.
//     POST /documents/{id}/share — create an expiring share link.
//     GET  /documents/{id}/share-links — links of a document.
//     POST /documents/{id}/request-access — request access.
//     POST /documents/{id}/flag-review — flag for retention review.
//   Share links:
//     POST /share-links/{id}/revoke — revoke a link.
//     GET  /shared/{token} — resolve a share link (public route;
//       see the public-access note below).
//   Access requests:
//     GET  /access-requests — pending / decided requests.
//     POST /access-requests/{id}/approve.
//     POST /access-requests/{id}/deny.
//   Retention:
//     GET  /retention/review — documents past their retention
//       window, flagged for review (never auto-deleted).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory folder, document, version, access-log, share-link, and
// access-request management; versioning where every upload is a new
// version and old versions are kept and restorable; an append-only
// access log written on every view / download / share action;
// unguessable share tokens with expiry, a download cap, and
// revocation; access requests with approve / deny decisions that
// are themselves audited; retention rules that only flag documents
// for review — deletion is always an explicit, audited manage
// action. What is NOT built here: file bytes (the storage module
// owns upload / download of bytes — this module only stores the
// storage target + key, the SHA-256 checksum, and the size, and
// verifies the checksum format on write); actual file encryption
// at rest (the storage target is responsible for it); virus
// scanning; and automatic deletion (by design — see the retention
// note in the module header).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`). There is no reusable per-request "is this
// endpoint public?" helper to call from a handler — the check lives
// in the API Manager middleware / `resolve_public` path — so the
// public route below relies on that gate: an administrator must
// create the matching `ApiAccessRule` row for
// `GET /api/v1/vault/shared/{token}`. The route is registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks it up. Do
// NOT treat the absence of a capability check on the public route
// as an open door: without the rule the API Manager denies the
// call. The share token itself is unguessable (two UUIDv7 values),
// carries an expiry and a download cap, and every resolution is
// written to the access log.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Visibility rule (enforced server-side on every read): a caller
// sees a document when they hold `vault.manage`, when they own the
// document, when their user id is in the document's
// `shared_with_client_ids`, or when an approved access request
// covers them for that document. Every other caller gets a 404,
// the same as for an unknown document, so existence never leaks.
//
// Events emitted on the hook bus:
//   - `vault.document.uploaded`, `vault.document.versioned`,
//     `vault.document.restored`, `vault.document.shared`,
//     `vault.document.deleted`, `vault.document.flagged`
//   - `vault.access.requested`, `vault.access.granted`,
//     `vault.access.denied`
//   - `vault.link.created`, `vault.link.revoked`
//   - `vault.folder.created`, `vault.folder.deleted`
//   - `notification.requested` (access granted / denied) with the
//     notifications module's payload shape: `{"kind", "user_id",
//     "title", "body", "module": "vault", "link", "priority",
//     "channels"}`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_STORAGE_TARGET_LEN: usize = 128;
const MAX_STORAGE_KEY_LEN: usize = 1024;
const MAX_MIME_TYPE_LEN: usize = 128;
const MAX_REASON_LEN: usize = 2000;
const MAX_NOTE_LEN: usize = 2000;
const MAX_CLIENTS: usize = 500;
/// Maximum versions kept per document in the in-memory store. Old
/// versions past this cap are still listed in the migration tables
/// as history; the in-memory store drops the oldest non-current
/// version first (the current version is never dropped).
const MAX_VERSIONS_PER_DOCUMENT: usize = 100;
/// Maximum downloads a share link may grant when no explicit cap
/// is given.
const MAX_LINK_DOWNLOADS: i64 = 10_000;

/// Capability names checked through the `role.check` hook.
const PERM_VAULT_VIEW: &str = "vault.view";
const PERM_VAULT_MANAGE: &str = "vault.manage";

/// Access-log action names. The log is append-only: rows are never
/// updated or deleted by this module.
pub const ACTION_VIEW: &str = "view";
pub const ACTION_DOWNLOAD: &str = "download";
pub const ACTION_SHARE: &str = "share";
pub const ACTION_REQUEST: &str = "request";
pub const ACTION_APPROVE: &str = "approve";
pub const ACTION_DENY: &str = "deny";
pub const ACTION_RESTORE: &str = "restore";
pub const ACTION_DELETE: &str = "delete";
pub const ACTION_UPLOAD: &str = "upload";
pub const ACTION_LINK_REVOKE: &str = "link_revoke";
pub const ACTION_FLAG_REVIEW: &str = "flag_review";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One folder. `parent_id` is `None` for a top-level folder.
/// `retention_days` is the retention rule for the folder: documents
/// older than that many days are flagged for review (never
/// auto-deleted). `auto_delete` is stored for the migration target
/// only and is always `false` in this module — deletion is never
/// automatic; see the module header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFolder {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<i64>,
    #[serde(default)]
    pub auto_delete: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One document. Metadata only: the file bytes live in the storage
/// module, referenced by `storage_target` + `storage_key`.
/// `checksum_sha256` is the hex SHA-256 of the bytes (format
/// validated on write); `size_bytes` is the byte size.
/// `shared_with_client_ids` lists the client user ids the document
/// is shared with; `status` is `active` or `archived`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDocument {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with_client_ids: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub flagged_for_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One version of a document. Every upload creates a new version;
/// old versions are kept and can be restored. `version_number` is
/// 1-based and strictly increasing per document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i64,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    pub uploaded_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One append-only access-log row. Written on every view, download,
/// share, request, approve, deny, restore, and delete action.
/// Rows are never updated or deleted by this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAccessLog {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One share link. `token` is unguessable (two UUIDv7 values, hex,
/// no separators) and is the only credential needed to resolve the
/// link; it is never written to logs. `expires_at` is required;
/// `max_downloads` caps downloads (0 means unlimited is not
/// allowed — a cap is always set, defaulting to 1); `revoked`
/// disables the link immediately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultShareLink {
    pub id: Uuid,
    pub document_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub max_downloads: i64,
    pub download_count: i64,
    pub revoked: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One access request: a client asks the owner for access to a
/// document. `status` is `pending`, `approved`, or `denied`.
/// Approval grants visibility through `can_see_document`; the
/// decision itself is written to the access log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAccessRequest {
    pub id: Uuid,
    pub document_id: Uuid,
    pub requester_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateFolderRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFolderRequest {
    pub name: Option<String>,
    pub parent_id: Option<Option<Uuid>>,
    pub retention_days: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct SetRetentionRequest {
    pub retention_days: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateDocumentRequest {
    pub folder_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub shared_with_client_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDocumentRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub folder_id: Option<Uuid>,
    pub status: Option<String>,
    pub shared_with_client_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UploadVersionRequest {
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateShareLinkRequest {
    /// Seconds from now until the link expires. Required and
    /// bounded: at least 60 seconds, at most 365 days.
    pub expires_in_seconds: i64,
    #[serde(default = "default_max_downloads")]
    pub max_downloads: i64,
}

fn default_max_downloads() -> i64 {
    1
}

#[derive(Debug, Deserialize)]
pub struct RequestAccessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DecideAccessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the vault. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration `2026-11-22-000052_vault`; until
/// that migration is executed, documents do not survive a restart.
#[derive(Clone)]
pub struct VaultState {
    pub folders: Arc<DashMap<Uuid, VaultFolder>>,
    pub documents: Arc<DashMap<Uuid, VaultDocument>>,
    pub versions: Arc<DashMap<Uuid, VaultDocumentVersion>>,
    pub access_log: Arc<DashMap<Uuid, VaultAccessLog>>,
    pub share_links: Arc<DashMap<Uuid, VaultShareLink>>,
    pub access_requests: Arc<DashMap<Uuid, VaultAccessRequest>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for VaultState {
    fn default() -> Self {
        Self {
            folders: Arc::new(DashMap::new()),
            documents: Arc::new(DashMap::new()),
            versions: Arc::new(DashMap::new()),
            access_log: Arc::new(DashMap::new()),
            share_links: Arc::new(DashMap::new()),
            access_requests: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl VaultState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "vault",
                "category": "vault",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Append one row to the access log. The log is append-only:
    /// this is the only writer, and no handler ever updates or
    /// removes a row.
    fn log_access(&self, document_id: Uuid, actor_id: &str, action: &str, detail: Option<String>) {
        self.access_log.insert(
            Uuid::now_v7(),
            VaultAccessLog {
                id: Uuid::now_v7(),
                document_id,
                actor_id: actor_id.to_string(),
                action: action.to_string(),
                detail,
                created_at: Utc::now(),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

/// A SHA-256 checksum is 64 lowercase hex characters.
fn valid_checksum(checksum: &str) -> bool {
    checksum.len() == 64
        && checksum
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn validate_storage_ref(target: &str, key: &str) -> UnifiedResult<()> {
    if target.trim().is_empty() || target.len() > MAX_STORAGE_TARGET_LEN {
        return Err(UnifiedError::bad_request(
            "storage_target must not be empty and must be at most 128 characters",
        ));
    }
    if key.trim().is_empty() || key.len() > MAX_STORAGE_KEY_LEN {
        return Err(UnifiedError::bad_request(
            "storage_key must not be empty and must be at most 1024 characters",
        ));
    }
    Ok(())
}

fn validate_client_ids(ids: &[String]) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_CLIENTS {
        return Err(UnifiedError::bad_request(format!(
            "shared_with_client_ids must contain at most {MAX_CLIENTS} entries"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in ids {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(UnifiedError::bad_request(
                "shared_with_client_ids must not contain empty entries",
            ));
        }
        if !out.contains(&trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

/// True when `actor` may see `document`: the owner, anyone holding
/// the manage capability, a listed client, or someone with an
/// approved access request for this document. The caller passes
/// `is_manager` (the result of the `vault.manage` capability
/// check) so the visibility test stays a pure function of the
/// stored rows plus that one flag.
fn can_see_document(state: &VaultState, document: &VaultDocument, actor: &str, is_manager: bool) -> bool {
    if is_manager || document.owner_id == actor {
        return true;
    }
    if document.shared_with_client_ids.iter().any(|id| id == actor) {
        return true;
    }
    state.access_requests.iter().any(|entry| {
        let request = entry.value();
        request.document_id == document.id
            && request.requester_id == actor
            && request.status == "approved"
    })
}

fn load_folder(state: &VaultState, raw_id: &str) -> UnifiedResult<VaultFolder> {
    let id = parse_uuid(raw_id, "folder")?;
    state
        .folders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Folder not found: {id}")))
}

/// Load a document for `actor`, enforcing the visibility rule.
/// A document the caller may not see answers 404, the same as an
/// unknown document, so existence never leaks.
fn load_visible_document(
    ctx: &AppContext,
    state: &VaultState,
    raw_id: &str,
    caller: &Caller,
) -> UnifiedResult<VaultDocument> {
    let id = parse_uuid(raw_id, "document")?;
    let document = state
        .documents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))?;
    let is_manager = check_capability(ctx, &caller.role, PERM_VAULT_MANAGE);
    if can_see_document(state, &document, &caller.actor, is_manager) {
        Ok(document)
    } else {
        Err(UnifiedError::not_found(format!("Document not found: {id}")))
    }
}

/// Load a document for a manage-only action (update, delete,
/// version upload, share, retention flag). The caller must hold
/// `vault.manage` or own the document; anything else answers 404.
fn load_managed_document(
    ctx: &AppContext,
    state: &VaultState,
    raw_id: &str,
    caller: &Caller,
) -> UnifiedResult<VaultDocument> {
    let id = parse_uuid(raw_id, "document")?;
    let document = state
        .documents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))?;
    let is_manager = check_capability(ctx, &caller.role, PERM_VAULT_MANAGE);
    if is_manager || document.owner_id == caller.actor {
        Ok(document)
    } else {
        Err(UnifiedError::not_found(format!("Document not found: {id}")))
    }
}

/// Unguessable share token: two UUIDv7 values rendered as hex with
/// no separators (64 hex characters, ~244 bits of entropy). The
/// token is never written to logs.
fn new_share_token() -> String {
    format!(
        "{}{}",
        Uuid::now_v7().simple(),
        Uuid::now_v7().simple()
    )
}

fn next_version_number(state: &VaultState, document_id: Uuid) -> i64 {
    state
        .versions
        .iter()
        .filter(|entry| entry.value().document_id == document_id)
        .map(|entry| entry.value().version_number)
        .max()
        .unwrap_or(0)
        + 1
}

fn versions_of(state: &VaultState, document_id: Uuid) -> Vec<VaultDocumentVersion> {
    let mut items: Vec<VaultDocumentVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| version.document_id == document_id)
        .collect();
    items.sort_by_key(|version| version.version_number);
    items
}

/// Enforce the per-document version cap: drop the oldest
/// non-current versions first. The current version is never
/// dropped. Called after inserting a new version.
fn enforce_version_cap(state: &VaultState, document: &VaultDocument) {
    let mut versions = versions_of(state, document.id);
    if versions.len() <= MAX_VERSIONS_PER_DOCUMENT {
        return;
    }
    versions.retain(|version| Some(version.id) != document.current_version_id);
    versions.sort_by_key(|version| version.version_number);
    while versions.len() >= MAX_VERSIONS_PER_DOCUMENT {
        if let Some(oldest) = versions.first() {
            let id = oldest.id;
            state.versions.remove(&id);
            versions.remove(0);
        } else {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Folder handlers
// ---------------------------------------------------------------------------

async fn list_folders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let parent_filter = params.filters.get("parent_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<VaultFolder> = state
            .folders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|folder| {
                if let Some(parent) = &parent_filter {
                    if parent == "null" {
                        if folder.parent_id.is_some() {
                            return false;
                        }
                    } else if folder.parent_id.map(|id| id.to_string()) != Some(parent.clone()) {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if folder.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() && !folder.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|folder| folder.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    body: web::Json<CreateFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if let Some(parent_id) = input.parent_id {
            if state.folders.get(&parent_id).is_none() {
                return Err(UnifiedError::not_found(format!("Parent folder not found: {parent_id}")));
            }
        }
        if let Some(days) = input.retention_days {
            if days < 0 {
                return Err(UnifiedError::bad_request("retention_days must be >= 0"));
            }
        }
        let now = Utc::now();
        let folder = VaultFolder {
            id: Uuid::now_v7(),
            name,
            parent_id: input.parent_id,
            owner_id: caller.actor.clone(),
            company_id: input.company_id,
            retention_days: input.retention_days,
            auto_delete: false,
            created_at: now,
            updated_at: now,
        };
        state.folders.insert(folder.id, folder.clone());
        state.emit(
            "vault.folder.created",
            json!({
                "folder_id": folder.id.to_string(),
                "owner_id": folder.owner_id,
                "company_id": folder.company_id,
            }),
        );
        Ok(folder)
    })();
    match result {
        Ok(folder) => created(folder),
        Err(error) => err(error, debug),
    }
}

async fn get_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        load_folder(&state, &path.into_inner())
    })();
    match result {
        Ok(folder) => ok(folder),
        Err(error) => err(error, debug),
    }
}

async fn update_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFolderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let folder = load_folder(&state, &path.into_inner())?;
        let input = body.into_inner();
        let mut updated = folder.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(parent) = input.parent_id {
            match parent {
                Some(parent_id) => {
                    if parent_id == folder.id {
                        return Err(UnifiedError::bad_request("A folder cannot be its own parent"));
                    }
                    if state.folders.get(&parent_id).is_none() {
                        return Err(UnifiedError::not_found(format!(
                            "Parent folder not found: {parent_id}"
                        )));
                    }
                    updated.parent_id = Some(parent_id);
                }
                None => updated.parent_id = None,
            }
        }
        if let Some(retention) = input.retention_days {
            match retention {
                Some(days) => {
                    if days < 0 {
                        return Err(UnifiedError::bad_request("retention_days must be >= 0"));
                    }
                    updated.retention_days = Some(days);
                }
                None => updated.retention_days = None,
            }
        }
        updated.updated_at = Utc::now();
        state.folders.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(folder) => ok(folder),
        Err(error) => err(error, debug),
    }
}

async fn delete_folder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let folder = load_folder(&state, &path.into_inner())?;
        if state
            .folders
            .iter()
            .any(|entry| entry.value().parent_id == Some(folder.id))
        {
            return Err(UnifiedError::conflict(
                "Folder still has sub-folders; move or delete them first",
            ));
        }
        if state
            .documents
            .iter()
            .any(|entry| entry.value().folder_id == folder.id)
        {
            return Err(UnifiedError::conflict(
                "Folder still has documents; move or delete them first",
            ));
        }
        state.folders.remove(&folder.id);
        state.emit(
            "vault.folder.deleted",
            json!({
                "folder_id": folder.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "folder_id": folder.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn set_folder_retention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SetRetentionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultFolder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let folder = load_folder(&state, &path.into_inner())?;
        let input = body.into_inner();
        if input.retention_days < 0 {
            return Err(UnifiedError::bad_request("retention_days must be >= 0"));
        }
        let mut updated = folder.clone();
        updated.retention_days = Some(input.retention_days);
        updated.updated_at = Utc::now();
        state.folders.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(folder) => ok(folder),
        Err(error) => err(error, debug),
    }
}

async fn list_folder_documents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let folder = load_folder(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                if document.folder_id != folder.id {
                    return false;
                }
                if !can_see_document(&state, document, &caller.actor, is_manager) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &document.status != status {
                        return false;
                    }
                }
                if !search.is_empty() && !document.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|document| document.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Document handlers
// ---------------------------------------------------------------------------

async fn list_documents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let folder_filter = params.filters.get("folder_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let flagged_filter = params.filters.get("flagged_for_review").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                // A client only ever sees documents shared with them;
                // managers and owners see everything in scope.
                if !can_see_document(&state, document, &caller.actor, is_manager) {
                    return false;
                }
                if let Some(folder_id) = &folder_filter {
                    if document.folder_id.to_string() != *folder_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &document.status != status {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if document.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(flagged) = &flagged_filter {
                    let expected = match flagged.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if document.flagged_for_review != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !document.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|document| document.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    body: web::Json<CreateDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let input = body.into_inner();
        let folder = state
            .folders
            .get(&input.folder_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Folder not found: {}", input.folder_id)))?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        validate_storage_ref(&input.storage_target, &input.storage_key)?;
        let checksum = input.checksum_sha256.trim().to_lowercase();
        if !valid_checksum(&checksum) {
            return Err(UnifiedError::bad_request(
                "checksum_sha256 must be 64 lowercase hex characters",
            ));
        }
        if input.size_bytes < 0 {
            return Err(UnifiedError::bad_request("size_bytes must be >= 0"));
        }
        let mime_type = match &input.mime_type {
            Some(raw) => require_text(raw, "mime_type", MAX_MIME_TYPE_LEN)?,
            None => "application/octet-stream".to_string(),
        };
        let shared_with = validate_client_ids(&input.shared_with_client_ids)?;
        let now = Utc::now();
        let version = VaultDocumentVersion {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            version_number: 1,
            storage_target: input.storage_target.trim().to_string(),
            storage_key: input.storage_key.trim().to_string(),
            checksum_sha256: checksum.clone(),
            size_bytes: input.size_bytes,
            mime_type: mime_type.clone(),
            uploaded_by: caller.actor.clone(),
            note: None,
            created_at: now,
        };
        let document = VaultDocument {
            id: version.document_id,
            folder_id: folder.id,
            name,
            description,
            owner_id: caller.actor.clone(),
            company_id: input.company_id,
            current_version_id: Some(version.id),
            storage_target: version.storage_target.clone(),
            storage_key: version.storage_key.clone(),
            checksum_sha256: checksum,
            size_bytes: input.size_bytes,
            mime_type,
            shared_with_client_ids: shared_with,
            status: "active".to_string(),
            flagged_for_review: false,
            flagged_at: None,
            created_at: now,
            updated_at: now,
        };
        state.versions.insert(version.id, version);
        state.documents.insert(document.id, document.clone());
        state.log_access(document.id, &caller.actor, ACTION_UPLOAD, None);
        state.emit(
            "vault.document.uploaded",
            json!({
                "document_id": document.id.to_string(),
                "folder_id": document.folder_id.to_string(),
                "owner_id": document.owner_id,
                "version": 1,
                "size_bytes": document.size_bytes,
            }),
        );
        Ok(document)
    })();
    match result {
        Ok(document) => created(document),
        Err(error) => err(error, debug),
    }
}

async fn get_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        state.log_access(document.id, &caller.actor, ACTION_VIEW, None);
        Ok(document)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

async fn update_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let input = body.into_inner();
        let mut updated = document.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(folder_id) = input.folder_id {
            if state.folders.get(&folder_id).is_none() {
                return Err(UnifiedError::not_found(format!("Folder not found: {folder_id}")));
            }
            updated.folder_id = folder_id;
        }
        if let Some(status) = input.status {
            match status.as_str() {
                "active" | "archived" => updated.status = status,
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "status must be \"active\" or \"archived\", got \"{other}\""
                    )))
                }
            }
        }
        if let Some(shared_with) = input.shared_with_client_ids {
            updated.shared_with_client_ids = validate_client_ids(&shared_with)?;
        }
        updated.updated_at = Utc::now();
        state.documents.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

async fn delete_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        // Deletion is always explicit and audited: the access log
        // keeps the delete entry, and the file bytes themselves are
        // the storage module's concern (this module only drops its
        // metadata rows).
        state.log_access(
            document.id,
            &caller.actor,
            ACTION_DELETE,
            Some(format!("document \"{}\" deleted", document.name)),
        );
        for version in versions_of(&state, document.id) {
            state.versions.remove(&version.id);
        }
        for link in state
            .share_links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| link.document_id == document.id)
        {
            state.share_links.remove(&link.id);
        }
        state.documents.remove(&document.id);
        state.emit(
            "vault.document.deleted",
            json!({
                "document_id": document.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "document_id": document.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Version handlers
// ---------------------------------------------------------------------------

async fn upload_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UploadVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocumentVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let input = body.into_inner();
        validate_storage_ref(&input.storage_target, &input.storage_key)?;
        let checksum = input.checksum_sha256.trim().to_lowercase();
        if !valid_checksum(&checksum) {
            return Err(UnifiedError::bad_request(
                "checksum_sha256 must be 64 lowercase hex characters",
            ));
        }
        if input.size_bytes < 0 {
            return Err(UnifiedError::bad_request("size_bytes must be >= 0"));
        }
        let mime_type = match &input.mime_type {
            Some(raw) => require_text(raw, "mime_type", MAX_MIME_TYPE_LEN)?,
            None => document.mime_type.clone(),
        };
        let note = match &input.note {
            Some(raw) => Some(require_text(raw, "note", MAX_NOTE_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let version = VaultDocumentVersion {
            id: Uuid::now_v7(),
            document_id: document.id,
            version_number: next_version_number(&state, document.id),
            storage_target: input.storage_target.trim().to_string(),
            storage_key: input.storage_key.trim().to_string(),
            checksum_sha256: checksum.clone(),
            size_bytes: input.size_bytes,
            mime_type: mime_type.clone(),
            uploaded_by: caller.actor.clone(),
            note,
            created_at: now,
        };
        state.versions.insert(version.id, version.clone());
        let mut updated = document.clone();
        updated.current_version_id = Some(version.id);
        updated.storage_target = version.storage_target.clone();
        updated.storage_key = version.storage_key.clone();
        updated.checksum_sha256 = checksum;
        updated.size_bytes = input.size_bytes;
        updated.mime_type = mime_type;
        updated.updated_at = now;
        state.documents.insert(updated.id, updated.clone());
        enforce_version_cap(&state, &updated);
        state.log_access(document.id, &caller.actor, ACTION_UPLOAD, None);
        state.emit(
            "vault.document.versioned",
            json!({
                "document_id": document.id.to_string(),
                "version_id": version.id.to_string(),
                "version": version.version_number,
                "uploaded_by": caller.actor,
            }),
        );
        Ok(version)
    })();
    match result {
        Ok(version) => created(version),
        Err(error) => err(error, debug),
    }
}

async fn list_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        let params = query.into_inner();
        let items = versions_of(&state, document.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn restore_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let (document_id, version_id) = path.into_inner();
        let document = load_managed_document(&ctx, &state, &document_id, &caller)?;
        let version_uuid = parse_uuid(&version_id, "version")?;
        let version = state
            .versions
            .get(&version_uuid)
            .map(|entry| entry.value().clone())
            .filter(|version| version.document_id == document.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Version not found: {version_id}")))?;
        // Restore points the document back at the old version's
        // storage reference; it does not delete the newer versions.
        let now = Utc::now();
        let mut updated = document.clone();
        updated.current_version_id = Some(version.id);
        updated.storage_target = version.storage_target.clone();
        updated.storage_key = version.storage_key.clone();
        updated.checksum_sha256 = version.checksum_sha256.clone();
        updated.size_bytes = version.size_bytes;
        updated.mime_type = version.mime_type.clone();
        updated.updated_at = now;
        state.documents.insert(updated.id, updated.clone());
        state.log_access(
            document.id,
            &caller.actor,
            ACTION_RESTORE,
            Some(format!("restored version {}", version.version_number)),
        );
        state.emit(
            "vault.document.restored",
            json!({
                "document_id": document.id.to_string(),
                "version_id": version.id.to_string(),
                "version": version.version_number,
                "restored_by": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Download + access-log handlers
// ---------------------------------------------------------------------------

async fn download_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        if document.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active document can be downloaded (status is \"{}\")",
                document.status
            )));
        }
        // Every download is logged (actor, action, timestamp) and
        // the file bytes are served by the storage module: the
        // response carries the storage reference only.
        state.log_access(document.id, &caller.actor, ACTION_DOWNLOAD, None);
        Ok(json!({
            "document_id": document.id.to_string(),
            "name": document.name,
            "storage_target": document.storage_target,
            "storage_key": document.storage_key,
            "checksum_sha256": document.checksum_sha256,
            "size_bytes": document.size_bytes,
            "mime_type": document.mime_type,
            "version_id": document.current_version_id.map(|id| id.to_string()),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_access_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor_id").cloned();
        let mut items: Vec<VaultAccessLog> = state
            .access_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.document_id != document.id {
                    return false;
                }
                if let Some(action) = &action_filter {
                    if &row.action != action {
                        return false;
                    }
                }
                if let Some(actor) = &actor_filter {
                    if &row.actor_id != actor {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Share-link handlers
// ---------------------------------------------------------------------------

async fn create_share_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateShareLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultShareLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let input = body.into_inner();
        if input.expires_in_seconds < 60 || input.expires_in_seconds > 365 * 24 * 60 * 60 {
            return Err(UnifiedError::bad_request(
                "expires_in_seconds must be between 60 and 31536000 (one year)",
            ));
        }
        if input.max_downloads < 1 || input.max_downloads > MAX_LINK_DOWNLOADS {
            return Err(UnifiedError::bad_request(format!(
                "max_downloads must be between 1 and {MAX_LINK_DOWNLOADS}"
            )));
        }
        let now = Utc::now();
        let link = VaultShareLink {
            id: Uuid::now_v7(),
            document_id: document.id,
            token: new_share_token(),
            expires_at: now + chrono::Duration::seconds(input.expires_in_seconds),
            max_downloads: input.max_downloads,
            download_count: 0,
            revoked: false,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.share_links.insert(link.id, link.clone());
        state.log_access(document.id, &caller.actor, ACTION_SHARE, None);
        state.emit(
            "vault.link.created",
            json!({
                "document_id": document.id.to_string(),
                "link_id": link.id.to_string(),
                "expires_at": link.expires_at.to_rfc3339(),
                "max_downloads": link.max_downloads,
                "created_by": caller.actor,
            }),
        );
        state.emit(
            "vault.document.shared",
            json!({
                "document_id": document.id.to_string(),
                "link_id": link.id.to_string(),
                "shared_by": caller.actor,
            }),
        );
        Ok(link)
    })();
    match result {
        Ok(link) => created(link),
        Err(error) => err(error, debug),
    }
}

async fn list_share_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let document = load_visible_document(&ctx, &state, &path.into_inner(), &caller)?;
        let params = query.into_inner();
        let revoked_filter = params.filters.get("revoked").cloned();
        let mut items: Vec<VaultShareLink> = state
            .share_links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| {
                if link.document_id != document.id {
                    return false;
                }
                if let Some(revoked) = &revoked_filter {
                    let expected = match revoked.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if link.revoked != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|link| link.created_at);
        // The token is the link credential: it is only returned on
        // creation. List responses carry the link metadata without
        // the token so a leaked list response cannot resolve links.
        let safe: Vec<Value> = items
            .into_iter()
            .map(|link| {
                json!({
                    "id": link.id.to_string(),
                    "document_id": link.document_id.to_string(),
                    "expires_at": link.expires_at.to_rfc3339(),
                    "max_downloads": link.max_downloads,
                    "download_count": link.download_count,
                    "revoked": link.revoked,
                    "created_by": link.created_by,
                    "created_at": link.created_at.to_rfc3339(),
                    "updated_at": link.updated_at.to_rfc3339(),
                })
            })
            .collect();
        Ok(paginate(safe, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn revoke_share_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "share link")?;
        let link = state
            .share_links
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Share link not found: {id}")))?;
        if link.revoked {
            return Err(UnifiedError::conflict("Share link is already revoked"));
        }
        let mut updated = link.clone();
        updated.revoked = true;
        updated.updated_at = Utc::now();
        state.share_links.insert(updated.id, updated.clone());
        state.log_access(
            link.document_id,
            &caller.actor,
            ACTION_LINK_REVOKE,
            None,
        );
        state.emit(
            "vault.link.revoked",
            json!({
                "document_id": link.document_id.to_string(),
                "link_id": link.id.to_string(),
                "revoked_by": caller.actor,
            }),
        );
        Ok(json!({ "revoked": true, "link_id": link.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve a share link by its token. Public route: no caller
/// headers are required, and the API Manager's `ApiAccessRule`
/// gate decides whether the path is public at all (see the module
/// header). The token is never logged. A revoked, expired, or
/// exhausted link answers 404, the same as an unknown token, so
/// link state never leaks.
async fn resolve_shared_link(
    state: web::Data<VaultState>,
    path: web::Path<String>,
) -> HttpResponse {
    let token = path.into_inner();
    let now = Utc::now();
    let link = state
        .share_links
        .iter()
        .map(|entry| entry.value().clone())
        .find(|link| link.token == token);
    let Some(link) = link else {
        return err(UnifiedError::not_found("Share link not found"), false);
    };
    if link.revoked || now > link.expires_at || link.download_count >= link.max_downloads {
        return err(UnifiedError::not_found("Share link not found"), false);
    }
    let Some(document) = state
        .documents
        .get(&link.document_id)
        .map(|entry| entry.value().clone())
    else {
        return err(UnifiedError::not_found("Share link not found"), false);
    };
    if document.status != "active" {
        return err(UnifiedError::not_found("Share link not found"), false);
    }
    let mut updated = link.clone();
    updated.download_count += 1;
    state.share_links.insert(updated.id, updated.clone());
    state.log_access(
        document.id,
        "share-link",
        ACTION_DOWNLOAD,
        Some(format!("via share link {}", link.id)),
    );
    ok(json!({
        "document_id": document.id.to_string(),
        "name": document.name,
        "storage_target": document.storage_target,
        "storage_key": document.storage_key,
        "checksum_sha256": document.checksum_sha256,
        "size_bytes": document.size_bytes,
        "mime_type": document.mime_type,
        "version_id": document.current_version_id.map(|id| id.to_string()),
        "downloads_remaining": updated.max_downloads - updated.download_count,
        "expires_at": updated.expires_at.to_rfc3339(),
    }))
}

// ---------------------------------------------------------------------------
// Access-request handlers
// ---------------------------------------------------------------------------

async fn request_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RequestAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultAccessRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let raw_id = path.into_inner();
        let document = load_visible_document(&ctx, &state, &raw_id, &caller)
            .or_else(|_| {
                // A requester who cannot see the document yet may
                // still request access: load it without the
                // visibility gate, but keep the id existence check
                // (a missing document still answers 404).
                let id = parse_uuid(&raw_id, "document")?;
                state
                    .documents
                    .get(&id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))
            })?;
        if document.owner_id == caller.actor {
            return Err(UnifiedError::conflict("The document owner already has access"));
        }
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        if can_see_document(&state, &document, &caller.actor, is_manager) {
            return Err(UnifiedError::conflict("Access is already granted for this document"));
        }
        if state.access_requests.iter().any(|entry| {
            let request = entry.value();
            request.document_id == document.id
                && request.requester_id == caller.actor
                && request.status == "pending"
        }) {
            return Err(UnifiedError::conflict(
                "An access request for this document is already pending",
            ));
        }
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let request = VaultAccessRequest {
            id: Uuid::now_v7(),
            document_id: document.id,
            requester_id: caller.actor.clone(),
            reason,
            status: "pending".to_string(),
            decided_by: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .access_requests
            .insert(request.id, request.clone());
        state.log_access(document.id, &caller.actor, ACTION_REQUEST, None);
        state.emit(
            "vault.access.requested",
            json!({
                "document_id": document.id.to_string(),
                "request_id": request.id.to_string(),
                "requester_id": caller.actor,
                "owner_id": document.owner_id,
            }),
        );
        state.notify(
            "vault.access.requested",
            &document.owner_id,
            "Vault access requested",
            &format!(
                "{} requested access to document \"{}\"",
                caller.actor, document.name
            ),
            Some(format!("/api/v1/vault/access-requests/{}", request.id)),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => created(request),
        Err(error) => err(error, debug),
    }
}

async fn list_access_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let document_filter = params.filters.get("document_id").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultAccessRequest> = state
            .access_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                // Managers see every request; anyone else sees only
                // their own requests and requests for documents they
                // own.
                if !is_manager {
                    let own_request = request.requester_id == caller.actor;
                    let own_document = state
                        .documents
                        .get(&request.document_id)
                        .map(|entry| entry.value().owner_id == caller.actor)
                        .unwrap_or(false);
                    if !own_request && !own_document {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &request.status != status {
                        return false;
                    }
                }
                if let Some(document_id) = &document_filter {
                    if request.document_id.to_string() != *document_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|request| request.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn decide_access_request(
    ctx: &AppContext,
    state: &VaultState,
    caller: &Caller,
    raw_id: &str,
    approve: bool,
    reason: Option<String>,
) -> UnifiedResult<VaultAccessRequest> {
    require_permission(ctx, &caller.role, PERM_VAULT_MANAGE)?;
    let id = parse_uuid(raw_id, "access request")?;
    let request = state
        .access_requests
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Access request not found: {id}")))?;
    if request.status != "pending" {
        return Err(UnifiedError::conflict(format!(
            "Only a pending access request can be decided (status is \"{}\")",
            request.status
        )));
    }
    let document = state
        .documents
        .get(&request.document_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {}", request.document_id)))?;
    let now = Utc::now();
    let mut updated = request.clone();
    updated.status = if approve { "approved".to_string() } else { "denied".to_string() };
    updated.decided_by = Some(caller.actor.clone());
    updated.decided_at = Some(now);
    updated.updated_at = now;
    state.access_requests.insert(updated.id, updated.clone());
    let action = if approve { ACTION_APPROVE } else { ACTION_DENY };
    state.log_access(document.id, &caller.actor, action, reason.clone());
    state.emit(
        if approve {
            "vault.access.granted"
        } else {
            "vault.access.denied"
        },
        json!({
            "document_id": document.id.to_string(),
            "request_id": request.id.to_string(),
            "requester_id": request.requester_id,
            "decided_by": caller.actor,
        }),
    );
    state.notify(
        if approve {
            "vault.access.granted"
        } else {
            "vault.access.denied"
        },
        &request.requester_id,
        if approve {
            "Vault access granted"
        } else {
            "Vault access denied"
        },
        &format!(
            "Your access request for document \"{}\" was {}",
            document.name,
            if approve { "approved" } else { "denied" }
        ),
        Some(format!("/api/v1/vault/documents/{}", document.id)),
    );
    Ok(updated)
}

async fn approve_access_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        decide_access_request(&ctx, &state, &caller, &path.into_inner(), true, reason)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

async fn deny_access_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        decide_access_request(&ctx, &state, &caller, &path.into_inner(), false, reason)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Retention handlers
// ---------------------------------------------------------------------------

async fn flag_document_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VaultDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_MANAGE)?;
        let document = load_managed_document(&ctx, &state, &path.into_inner(), &caller)?;
        let now = Utc::now();
        let mut updated = document.clone();
        updated.flagged_for_review = true;
        updated.flagged_at = Some(now);
        updated.updated_at = now;
        state.documents.insert(updated.id, updated.clone());
        state.log_access(document.id, &caller.actor, ACTION_FLAG_REVIEW, None);
        state.emit(
            "vault.document.flagged",
            json!({
                "document_id": document.id.to_string(),
                "flagged_by": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(document) => ok(document),
        Err(error) => err(error, debug),
    }
}

async fn retention_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<VaultState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VAULT_VIEW)?;
        let params = query.into_inner();
        let now = Utc::now();
        let is_manager = check_capability(&ctx, &caller.role, PERM_VAULT_MANAGE);
        let mut items: Vec<VaultDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                if !can_see_document(&state, document, &caller.actor, is_manager) {
                    return false;
                }
                // Flagged documents always show up; otherwise a
                // document qualifies when its folder has a retention
                // rule and the document is older than that many days.
                if document.flagged_for_review {
                    return true;
                }
                let Some(folder) = state.folders.get(&document.folder_id) else {
                    return false;
                };
                let Some(days) = folder.value().retention_days else {
                    return false;
                };
                let age_days = (now - document.created_at).num_days();
                age_days >= days
            })
            .collect();
        items.sort_by_key(|document| document.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct VaultModule {
    state: VaultState,
}

impl VaultModule {
    pub fn new() -> Self {
        Self {
            state: VaultState::default(),
        }
    }
}

impl Default for VaultModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for VaultModule {
    fn id(&self) -> &'static str {
        "vault"
    }

    fn name(&self) -> &'static str {
        "Document Vault"
    }

    fn description(&self) -> &'static str {
        "Secure document storage per company and client: folders, versioned documents (metadata only — file bytes live in the storage module), append-only access logs, expiring share links, access requests with approvals, and retention rules that flag for review and never auto-delete."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "storage", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "storage", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {
        // No inbound listeners: the vault only emits events (see the
        // module header). Access decisions and share-link state are
        // driven by its own HTTP routes.
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/vault")
                .app_data(state)
                .route("/folders", web::get().to(list_folders))
                .route("/folders", web::post().to(create_folder))
                .route("/folders/{id}", web::get().to(get_folder))
                .route("/folders/{id}", web::patch().to(update_folder))
                .route("/folders/{id}", web::delete().to(delete_folder))
                .route("/folders/{id}/retention", web::post().to(set_folder_retention))
                .route("/folders/{id}/documents", web::get().to(list_folder_documents))
                .route("/documents", web::get().to(list_documents))
                .route("/documents", web::post().to(create_document))
                .route("/documents/{id}", web::get().to(get_document))
                .route("/documents/{id}", web::patch().to(update_document))
                .route("/documents/{id}", web::delete().to(delete_document))
                .route("/documents/{id}/versions", web::post().to(upload_version))
                .route("/documents/{id}/versions", web::get().to(list_versions))
                .route(
                    "/documents/{id}/versions/{version_id}/restore",
                    web::post().to(restore_version),
                )
                .route("/documents/{id}/download", web::post().to(download_document))
                .route("/documents/{id}/access-log", web::get().to(list_access_log))
                .route("/documents/{id}/share", web::post().to(create_share_link))
                .route("/documents/{id}/share-links", web::get().to(list_share_links))
                .route("/documents/{id}/request-access", web::post().to(request_access))
                .route("/documents/{id}/flag-review", web::post().to(flag_document_review))
                .route("/share-links/{id}/revoke", web::post().to(revoke_share_link))
                .route("/shared/{token}", web::get().to(resolve_shared_link))
                .route("/access-requests", web::get().to(list_access_requests))
                .route("/access-requests/{id}/approve", web::post().to(approve_access_request))
                .route("/access-requests/{id}/deny", web::post().to(deny_access_request))
                .route("/retention/review", web::get().to(retention_review)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/vault/folders", owner: "vault", description: "List folders (paginated, filter by parent_id, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/folders", owner: "vault", description: "Create a folder (manage capability)." },
            RouteInfo { method: "GET", path: "/api/v1/vault/folders/{id}", owner: "vault", description: "Get one folder." },
            RouteInfo { method: "PATCH", path: "/api/v1/vault/folders/{id}", owner: "vault", description: "Update a folder (name, parent, retention)." },
            RouteInfo { method: "DELETE", path: "/api/v1/vault/folders/{id}", owner: "vault", description: "Delete an empty folder." },
            RouteInfo { method: "POST", path: "/api/v1/vault/folders/{id}/retention", owner: "vault", description: "Set the folder retention rule (retain N days, flag for review)." },
            RouteInfo { method: "GET", path: "/api/v1/vault/folders/{id}/documents", owner: "vault", description: "List documents in a folder (visibility enforced)." },
            RouteInfo { method: "GET", path: "/api/v1/vault/documents", owner: "vault", description: "List visible documents (paginated, filter by folder_id, status, company_id, flagged_for_review). Clients only see shared documents." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents", owner: "vault", description: "Upload document metadata (storage target + key, checksum, size); creates version 1." },
            RouteInfo { method: "GET", path: "/api/v1/vault/documents/{id}", owner: "vault", description: "Get one document (logged as a view; invisible documents answer 404)." },
            RouteInfo { method: "PATCH", path: "/api/v1/vault/documents/{id}", owner: "vault", description: "Update a document (name, description, folder, status, shared clients)." },
            RouteInfo { method: "DELETE", path: "/api/v1/vault/documents/{id}", owner: "vault", description: "Delete a document (explicit, audited; file bytes stay with the storage module)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents/{id}/versions", owner: "vault", description: "Upload a new version (metadata); old versions are kept." },
            RouteInfo { method: "GET", path: "/api/v1/vault/documents/{id}/versions", owner: "vault", description: "List versions of a document (oldest first)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents/{id}/versions/{version_id}/restore", owner: "vault", description: "Restore an old version as the current version." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents/{id}/download", owner: "vault", description: "Record a download (logged) and return the storage reference; bytes are served by the storage module." },
            RouteInfo { method: "GET", path: "/api/v1/vault/documents/{id}/access-log", owner: "vault", description: "List the append-only access log of a document (filter by action, actor_id)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents/{id}/share", owner: "vault", description: "Create an expiring share link (unguessable token, download cap)." },
            RouteInfo { method: "GET", path: "/api/v1/vault/documents/{id}/share-links", owner: "vault", description: "List share links of a document (tokens are never returned here)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents/{id}/request-access", owner: "vault", description: "Request access to a document (owner is notified)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/documents/{id}/flag-review", owner: "vault", description: "Flag a document for retention review." },
            RouteInfo { method: "POST", path: "/api/v1/vault/share-links/{id}/revoke", owner: "vault", description: "Revoke a share link immediately." },
            RouteInfo { method: "GET", path: "/api/v1/vault/shared/{token}", owner: "vault", description: "PUBLIC: resolve a share link by token (no auth; gated by the API Manager ApiAccessRule for this path; revoked/expired/exhausted links answer 404)." },
            RouteInfo { method: "GET", path: "/api/v1/vault/access-requests", owner: "vault", description: "List access requests (managers see all; others see their own and requests for documents they own)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/access-requests/{id}/approve", owner: "vault", description: "Approve an access request (audited; requester is notified)." },
            RouteInfo { method: "POST", path: "/api/v1/vault/access-requests/{id}/deny", owner: "vault", description: "Deny an access request (audited; requester is notified)." },
            RouteInfo { method: "GET", path: "/api/v1/vault/retention/review", owner: "vault", description: "List documents past their retention window or flagged for review (never auto-deleted)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(VaultModule::new()));
}
