// Documents module — chunked / resumable uploads, multi-file upload,
// per-module allowlists, pluggable virus scanning, versioning,
// per-file access control with signed expiring download URLs,
// content deduplication, quotas, and preview metadata.
//
// Bytes live in the core storage abstraction (`crate::core::storage`):
// the default provider is `LocalProvider` under `data/documents/`.
// This module builds no second storage layer; stored objects are
// addressed by validated storage keys (`objects/<prefix>/<sha256>`).
// The in-memory DashMap store is authoritative at runtime; the
// tables in `migrations/2026-11-22-000064_documents` are persistence
// targets only (same convention as the other batch modules).
//
// Routes live under `/api/v1/documents`:
//
//   Documents:
//     GET    /                              list documents (paginated, filtered)
//     POST   /upload                        multipart multi-file upload
//     GET    /{id}                          one document
//     DELETE /{id}                          delete document + all versions
//     GET    /{id}/preview                  preview metadata (mime, size,
//                                            image dimensions, checksum,
//                                            version count, scan status)
//     GET    /{id}/download                 download current version
//     POST   /{id}/signed-url               issue a signed expiring URL
//     GET    /download/{token}              redeem a signed URL
//     POST   /{id}/versions                 re-upload (new version)
//     GET    /{id}/versions                 list versions
//     GET    /{id}/versions/{version}       one version
//     POST   /{id}/versions/{version}/restore
//                                            restore a version (new version)
//     POST   /{id}/rescan                   re-run the virus scan (admin)
//   Chunked / resumable uploads:
//     POST   /uploads/sessions              initiate a session
//     GET    /uploads/sessions              list sessions
//     GET    /uploads/sessions/{id}         session status (missing parts)
//     POST   /uploads/sessions/{id}/parts   upload one part
//     POST   /uploads/sessions/{id}/complete
//                                            assemble + verify + finalize
//     DELETE /uploads/sessions/{id}         abort (delete partial parts)
//     POST   /uploads/sessions/purge-expired
//                                            delete expired sessions (admin)
//   Allowlist (admin):
//     GET    /allowlist                     list rules
//     POST   /allowlist                     create a rule
//     GET    /allowlist/{id}                one rule
//     PATCH  /allowlist/{id}                update a rule
//     DELETE /allowlist/{id}                delete a rule
//   Quotas:
//     GET    /quotas/usage                  quota usage (company / user)
//     GET    /quotas                        list local quotas
//     POST   /quotas                        create / replace a local quota (admin)
//   Scanners / settings / downloads:
//     GET    /scanners                      registered scanners
//     GET    /settings                      document settings
//     PATCH  /settings                      update settings (admin)
//     GET    /downloads                     download records (paginated)
//
// Events emitted on the hook bus (never direct calls to other modules):
//   - `document.uploaded` (new document finalized, incl. via a
//     completed chunked session), `document.version.created`
//     (re-upload or restore), `document.downloaded` (every download,
//     including unscanned ones), `document.deleted`,
//     `document.scan.completed` (every scan verdict),
//     `document.quota.exceeded` (upload rejected by quota).
// Quota checks ask the Storage Management module through its
// `storage.object.upload_requested` event (payload
// `{"module": "documents", "owner", "company_id", "key_hint",
// "bytes", "mime"}`); the answer carries `quota_ok` /
// `quota_exceeded`. When no Storage Management handler answers,
// the module falls back to its own local per-company / per-user
// quotas, and an upload with no quota row at all is unlimited
// locally. A quota rejection is always fail-closed: the upload is
// refused with 409 and `document.quota.exceeded` is emitted.
//
// Virus scanning — pluggable, nothing bundled:
//   Implement `DocumentScanner` (`scan(bytes) -> ScanVerdict`) and
//   register it with `DocumentsState::register_scanner` (or
//   `DocumentsModule::register_scanner`). Scanners are listed at
//   `GET /api/v1/documents/scanners`. A scan runs synchronously
//   before a file becomes downloadable and the verdict is recorded
//   on the document and on every version. With no scanner
//   configured the verdict is `Skipped`. Download rules:
//   `Clean` -> allowed; `Infected` -> blocked, always;
//   `Pending` -> blocked; `Error` -> blocked; `Skipped` -> allowed
//   ONLY when the admin setting `allow_unscanned_downloads` is true
//   (default false — fail closed), and every skipped download is
//   audit-logged through `document.downloaded` with
//   `"scan_status": "skipped"`.
//
// Signed download URLs: HMAC-SHA-256 over
// `"<document-id>:<expiry-unix>"` with a per-process random key
// generated at startup (the key is never persisted, never logged,
// and never returned; links stop working after a restart, by
// design). Verification uses a constant-time comparison and
// rejects expired or tampered tokens. The token itself is the
// capability for `GET /download/{token}`; every other route
// requires the `X-User-Id` / `X-Role` headers and the matching
// `documents:read` / `documents:write` / `documents:admin`
// capability through the `role.check` hook (fail closed: no
// answer means not allowed).
//
// Follow-up, deliberately out of this round: no thumbnail
// rendering. `GET /{id}/preview` returns metadata only (including
// image dimensions parsed from the image header bytes); generating
// thumbnail images is follow-up work.
//
// Actor identity and capability checks follow the Storage Manager /
// AI-module shape: the caller is identified by the `X-User-Id`
// header and the role by the `X-Role` header, and every route asks
// the roles module through the `role.check` hook.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::storage::{sha256_hex, validate_key, LocalProvider, StorageProvider};
use actix_multipart::Multipart;
use actix_web::{web, HttpRequest, HttpResponse};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum single upload through any endpoint (100 MiB), matching
/// the file manager and the storage hub.
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
/// Default chunk size for resumable upload sessions (1 MiB).
const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024;
/// Smallest accepted chunk size (64 KiB).
const MIN_CHUNK_SIZE: u64 = 64 * 1024;
/// Largest accepted chunk size (8 MiB).
const MAX_CHUNK_SIZE: u64 = 8 * 1024 * 1024;
/// Default lifetime of an upload session (24 hours).
const DEFAULT_SESSION_TTL_SECS: i64 = 86_400;
/// Default lifetime of a signed download URL (1 hour).
const DEFAULT_SIGNED_URL_SECS: u64 = 3_600;
/// Maximum lifetime of a signed download URL (7 days).
const MAX_SIGNED_URL_SECS: u64 = 604_800;

// ---------------------------------------------------------------------------
// Permissions (capability names checked through the `role.check` hook).
// ---------------------------------------------------------------------------

const PERM_READ: &str = "documents:read";
const PERM_WRITE: &str = "documents:write";
const PERM_ADMIN: &str = "documents:admin";

// ---------------------------------------------------------------------------
// Hook event names.
// ---------------------------------------------------------------------------

pub mod events {
    /// Emitted when a new document is finalized. Payload is the
    /// document record.
    pub const DOCUMENT_UPLOADED: &str = "document.uploaded";
    /// Emitted when a new version is created (re-upload or restore).
    /// Payload is the version record.
    pub const VERSION_CREATED: &str = "document.version.created";
    /// Emitted on every download. Payload:
    /// `{"document_id", "version", "user_id", "via",
    /// "scan_status", "bytes"}`.
    pub const DOCUMENT_DOWNLOADED: &str = "document.downloaded";
    /// Emitted when a document is deleted. Payload:
    /// `{"id", "name", "owner_id", "company_id"}`.
    pub const DOCUMENT_DELETED: &str = "document.deleted";
    /// Emitted when a scan finishes. Payload:
    /// `{"document_id", "version", "verdict", "threat",
    /// "scanner"}`.
    pub const SCAN_COMPLETED: &str = "document.scan.completed";
    /// Emitted when a quota check rejects an upload. Payload:
    /// `{"scope", "scope_id", "module", "user_id", "company_id",
    /// "bytes", "detail"}`.
    pub const QUOTA_EXCEEDED: &str = "document.quota.exceeded";
}

// ---------------------------------------------------------------------------
// Virus scanner — pluggable interface. No scanner is bundled.
// ---------------------------------------------------------------------------

/// Verdict of one virus scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanVerdict {
    /// No threat found.
    Clean,
    /// A threat was found; `threat` names it (scanner-supplied
    /// label, never raw file content).
    Infected { threat: String },
    /// The scanner failed; `reason` explains why. Treated as
    /// blocking: a file whose scan errored is never downloadable
    /// until a later scan returns `Clean`.
    Error { reason: String },
}

impl ScanVerdict {
    /// Stored status string for the verdict.
    pub fn status(&self) -> &'static str {
        match self {
            ScanVerdict::Clean => "clean",
            ScanVerdict::Infected { .. } => "infected",
            ScanVerdict::Error { .. } => "error",
        }
    }

    /// Threat label for `Infected`, `None` otherwise.
    pub fn threat(&self) -> Option<String> {
        match self {
            ScanVerdict::Infected { threat } => Some(threat.clone()),
            _ => None,
        }
    }
}

/// A pluggable virus scanner. Implement this trait and register the
/// implementation with `DocumentsState::register_scanner` (or
/// `DocumentsModule::register_scanner`); the module calls `scan`
/// synchronously for every finalized upload and for every explicit
/// rescan, before the file becomes downloadable. The scanner must
/// be fast and must never log or retain the bytes it scans. The
/// module runs every registered scanner and combines the verdicts:
/// any `Infected` wins (first threat reported), otherwise any
/// `Error` wins, otherwise `Clean`. With no scanner registered the
/// verdict is `Skipped` (see the module docs for the download rule).
pub trait DocumentScanner: Send + Sync {
    /// Stable name of this scanner, shown by `GET /scanners`.
    fn name(&self) -> &str;
    /// Scan `bytes` and return the verdict.
    fn scan(&self, bytes: &[u8]) -> ScanVerdict;
}

/// Scan status recorded on a document and on each version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Pending,
    Clean,
    Infected,
    Error,
    Skipped,
}

impl ScanStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScanStatus::Pending => "pending",
            ScanStatus::Clean => "clean",
            ScanStatus::Infected => "infected",
            ScanStatus::Error => "error",
            ScanStatus::Skipped => "skipped",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "clean" => ScanStatus::Clean,
            "infected" => ScanStatus::Infected,
            "error" => ScanStatus::Error,
            "skipped" => ScanStatus::Skipped,
            _ => ScanStatus::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One stored content object, shared by every document version with
/// the same bytes (content-addressed deduplication).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredObject {
    /// SHA-256 hex of the content; also the identity of the object.
    pub checksum: String,
    /// Validated storage key, e.g. `objects/ab/<sha256>`.
    pub storage_key: String,
    pub size: u64,
    /// How many document versions point at this object.
    pub ref_count: u64,
    pub created_at: DateTime<Utc>,
}

/// One document: the current version plus its access scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRecord {
    pub id: String,
    pub name: String,
    /// Owning module id (the module the file belongs to).
    pub module_id: String,
    /// Company the document belongs to; empty when unscoped.
    pub company_id: String,
    /// Opaque actor id of the owner (`X-User-Id` value).
    pub owner_id: String,
    /// Access scope: `owner`, `company`, or `module`.
    pub scope: String,
    pub mime: String,
    pub size: u64,
    /// SHA-256 hex of the current version's bytes.
    pub checksum: String,
    /// Storage key of the current version's bytes.
    pub storage_key: String,
    pub current_version: u32,
    pub version_count: u32,
    pub scan_status: String,
    /// Threat label when `scan_status` is `infected`, else null.
    pub scan_threat: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One version of a document. History is append-only: restoring an
/// old version creates a new version with the old content and never
/// mutates an existing version row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVersion {
    pub id: String,
    pub document_id: String,
    pub version: u32,
    pub checksum: String,
    pub storage_key: String,
    pub size: u64,
    pub mime: String,
    /// Opaque actor id of the uploader.
    pub uploader_id: String,
    pub scan_status: String,
    pub scan_threat: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One resumable upload session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSession {
    pub id: String,
    /// Document the session completes into, when this session is a
    /// re-upload of an existing document.
    pub document_id: Option<String>,
    pub name: String,
    pub module_id: String,
    pub company_id: String,
    pub owner_id: String,
    pub total_size: u64,
    pub expected_sha256: String,
    pub chunk_size: u64,
    /// Number of parts the total size splits into.
    pub part_count: u32,
    /// Received part indexes, sorted.
    pub received_parts: Vec<u32>,
    /// Missing part indexes, sorted (for resume).
    pub missing_parts: Vec<u32>,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One received chunk part, held in memory until the session is
/// completed or aborted.
#[derive(Debug, Clone)]
pub struct UploadPart {
    pub index: u32,
    pub size: u64,
    pub sha256: String,
    pub bytes: Vec<u8>,
    pub received_at: DateTime<Utc>,
}

/// One per-module allowlist rule (admin-managed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowlistRule {
    pub id: String,
    /// Module id this rule applies to.
    pub module_id: String,
    /// Allowed file extensions, lowercase, without the dot.
    pub extensions: Vec<String>,
    /// Allowed MIME types (empty means any MIME for an allowed
    /// extension).
    pub mime_types: Vec<String>,
    /// Maximum file size in bytes for this module.
    pub max_bytes: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One download record (audit trail, one row per download).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: String,
    pub document_id: String,
    pub version: u32,
    /// Opaque actor id, or `"signed-url"` for token redemptions.
    pub user_id: String,
    /// `direct` or `signed-url`.
    pub via: String,
    pub scan_status: String,
    pub bytes: u64,
    pub downloaded_at: DateTime<Utc>,
}

/// One local quota row (per-company or per-user). Storage
/// Management quotas are consulted first through the hook bus;
/// these rows are the fallback when that module does not answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentQuota {
    pub id: String,
    /// `company` or `user`.
    pub scope: String,
    pub scope_id: String,
    /// Maximum total bytes, or null for unlimited.
    pub bytes_limit: Option<u64>,
    pub used_bytes: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Admin-managed document settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSettings {
    /// When false (default), files whose scan was skipped because
    /// no scanner is configured are NOT downloadable (fail closed).
    pub allow_unscanned_downloads: bool,
    pub default_chunk_size: u64,
    pub session_ttl_secs: i64,
    pub max_upload_bytes: u64,
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            allow_unscanned_downloads: false,
            default_chunk_size: DEFAULT_CHUNK_SIZE,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            max_upload_bytes: MAX_UPLOAD_BYTES,
        }
    }
}

/// Preview metadata for one document. No thumbnail is rendered in
/// this round; image dimensions come from the image header bytes
/// only (see `image_dimensions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentPreview {
    pub document_id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub checksum: String,
    pub version_count: u32,
    pub current_version: u32,
    pub scan_status: String,
    pub scan_threat: Option<String>,
    /// Image width in pixels, when the bytes identify an image.
    pub width: Option<u32>,
    /// Image height in pixels, when the bytes identify an image.
    pub height: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InitiateSessionRequest {
    pub name: String,
    pub module_id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    /// Target document for a re-upload session, if any.
    #[serde(default)]
    pub document_id: Option<String>,
    pub total_size: u64,
    /// Expected SHA-256 hex of the complete file.
    pub sha256: String,
    #[serde(default)]
    pub chunk_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAllowlistRequest {
    pub module_id: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub mime_types: Vec<String>,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAllowlistRequest {
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
    #[serde(default)]
    pub mime_types: Option<Vec<String>>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateQuotaRequest {
    /// `company` or `user`.
    pub scope: String,
    pub scope_id: String,
    /// Maximum total bytes, or null for unlimited.
    pub bytes_limit: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignedUrlRequest {
    #[serde(default)]
    pub expires_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub allow_unscanned_downloads: Option<bool>,
    #[serde(default)]
    pub default_chunk_size: Option<u64>,
    #[serde(default)]
    pub session_ttl_secs: Option<i64>,
    #[serde(default)]
    pub max_upload_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedUrlResponse {
    pub document_id: String,
    pub url: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// One per-file outcome of a multi-file upload.
#[derive(Debug, Clone, Serialize)]
pub struct MultiUploadResult {
    pub uploaded: Vec<DocumentRecord>,
    pub failed: Vec<Value>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct DocumentsState {
    hooks: RwLock<Arc<HookBus>>,
    documents: DashMap<String, DocumentRecord>,
    versions: DashMap<String, DocumentVersion>,
    /// Objects keyed by content SHA-256 hex.
    objects: DashMap<String, StoredObject>,
    sessions: DashMap<String, UploadSession>,
    /// Parts keyed by session id, then part index.
    parts: DashMap<String, DashMap<u32, UploadPart>>,
    allowlist: DashMap<String, AllowlistRule>,
    downloads: DashMap<String, DownloadRecord>,
    /// Local quotas keyed by `"<scope>:<scope_id>"`.
    quotas: DashMap<String, DocumentQuota>,
    /// Registered virus scanners, keyed by scanner name.
    scanners: DashMap<String, Arc<dyn DocumentScanner>>,
    settings: RwLock<DocumentSettings>,
    /// Per-process random HMAC key for signed download URLs.
    /// Generated at startup, never persisted or logged.
    signing_key: RwLock<[u8; 32]>,
    provider: Option<Arc<dyn StorageProvider>>,
}

impl DocumentsState {
    pub fn new() -> Self {
        let provider = LocalProvider::new(PathBuf::from("data/documents"))
            .ok()
            .map(|provider| Arc::new(provider) as Arc<dyn StorageProvider>);
        // Per-process signing key from two random UUIDs (32 bytes).
        let first = *Uuid::new_v4().as_bytes();
        let second = *Uuid::new_v4().as_bytes();
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&first);
        key[16..].copy_from_slice(&second);
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            documents: DashMap::new(),
            versions: DashMap::new(),
            objects: DashMap::new(),
            sessions: DashMap::new(),
            parts: DashMap::new(),
            allowlist: DashMap::new(),
            downloads: DashMap::new(),
            quotas: DashMap::new(),
            scanners: DashMap::new(),
            settings: RwLock::new(DocumentSettings::default()),
            signing_key: RwLock::new(key),
            provider,
        };
        state.seed_default_allowlist();
        state
    }

    /// Seed the default allowlist rule for this module's own files.
    /// Other modules have no rule until an admin creates one, and
    /// uploads for a module without a rule are rejected (fail
    /// closed). The default set is common office / image / archive
    /// types; nothing executable is ever allowed.
    fn seed_default_allowlist() {
        let rule = AllowlistRule {
            id: new_id(),
            module_id: "documents".to_string(),
            extensions: vec![
                "pdf".to_string(),
                "doc".to_string(),
                "docx".to_string(),
                "xls".to_string(),
                "xlsx".to_string(),
                "ppt".to_string(),
                "pptx".to_string(),
                "txt".to_string(),
                "md".to_string(),
                "csv".to_string(),
                "png".to_string(),
                "jpg".to_string(),
                "jpeg".to_string(),
                "gif".to_string(),
                "webp".to_string(),
                "svg".to_string(),
                "zip".to_string(),
            ],
            mime_types: vec![
                "application/pdf".to_string(),
                "application/msword".to_string(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string(),
                "application/vnd.ms-excel".to_string(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                    .to_string(),
                "application/vnd.ms-powerpoint".to_string(),
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    .to_string(),
                "text/plain".to_string(),
                "text/markdown".to_string(),
                "text/csv".to_string(),
                "image/png".to_string(),
                "image/jpeg".to_string(),
                "image/gif".to_string(),
                "image/webp".to_string(),
                "image/svg+xml".to_string(),
                "application/zip".to_string(),
            ],
            max_bytes: MAX_UPLOAD_BYTES,
            created_at: now(),
            updated_at: now(),
        };
        self.allowlist.insert(rule.id.clone(), rule);
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn settings(&self) -> DocumentSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    fn provider(&self) -> UnifiedResult<Arc<dyn StorageProvider>> {
        self.provider.clone().ok_or_else(|| {
            UnifiedError::internal(
                "Document storage is unavailable (local provider could not be created)",
            )
        })
    }

    /// Register a virus scanner. Re-registering a name replaces the
    /// previous scanner with that name.
    pub fn register_scanner(&self, scanner: Arc<dyn DocumentScanner>) {
        self.scanners.insert(scanner.name().to_string(), scanner);
    }

    /// Remove a scanner by name. Returns true when one was removed.
    pub fn unregister_scanner(&self, name: &str) -> bool {
        self.scanners.remove(name).is_some()
    }

    /// Names of the registered scanners, sorted.
    pub fn scanner_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .scanners
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        names.sort();
        names
    }

    /// Run every registered scanner over `bytes` and combine the
    /// verdicts: any `Infected` wins, otherwise any `Error` wins,
    /// otherwise `Clean`. With no scanner registered the caller
    /// records `Skipped` (see `scan_document_bytes`).
    fn run_scanners(&self, bytes: &[u8]) -> Option<ScanVerdict> {
        if self.scanners.is_empty() {
            return None;
        }
        let mut first_error: Option<ScanVerdict> = None;
        for entry in self.scanners.iter() {
            match entry.value().scan(bytes) {
                verdict @ ScanVerdict::Infected { .. } => return Some(verdict),
                verdict @ ScanVerdict::Error { .. } => {
                    if first_error.is_none() {
                        first_error = Some(verdict);
                    }
                }
                ScanVerdict::Clean => {}
            }
        }
        Some(first_error.unwrap_or(ScanVerdict::Clean))
    }

    /// Scan `bytes` for a document version, emit
    /// `document.scan.completed`, and return the recorded status
    /// plus threat label. `None` from the scanner registry means
    /// no scanner is configured, recorded as `Skipped`.
    fn scan_document_bytes(
        &self,
        document_id: &str,
        version: u32,
        bytes: &[u8],
    ) -> (ScanStatus, Option<String>) {
        let (status, threat, scanner) = match self.run_scanners(bytes) {
            None => (ScanStatus::Skipped, None, None),
            Some(ScanVerdict::Clean) => (ScanStatus::Clean, None, Some("registered".to_string())),
            Some(ScanVerdict::Infected { threat }) => {
                (ScanStatus::Infected, Some(threat), Some("registered".to_string()))
            }
            Some(ScanVerdict::Error { reason }) => {
                (ScanStatus::Error, Some(reason), Some("registered".to_string()))
            }
        };
        let verdict = match &status {
            ScanStatus::Clean => "clean",
            ScanStatus::Infected => "infected",
            ScanStatus::Error => "error",
            ScanStatus::Skipped => "skipped",
            ScanStatus::Pending => "pending",
        };
        self.emit(
            events::SCAN_COMPLETED,
            json!({
                "document_id": document_id,
                "version": version,
                "verdict": verdict,
                "threat": threat,
                "scanner": scanner,
            }),
        );
        (status, threat)
    }

    /// Storage key for one content object (validated before use).
    fn object_key(checksum: &str) -> UnifiedResult<String> {
        let prefix = checksum.get(..2).unwrap_or("00");
        let key = format!("objects/{prefix}/{checksum}");
        validate_key(&key)?;
        Ok(key)
    }

    /// Store `bytes` under their content hash, reusing the existing
    /// object when identical content is already stored. Returns the
    /// storage key and whether a new object was written.
    async fn store_content(&self, bytes: &[u8]) -> UnifiedResult<(String, bool)> {
        let checksum = sha256_hex(bytes);
        if let Some(object) = self.objects.get(&checksum) {
            return Ok((object.storage_key.clone(), false));
        }
        let key = Self::object_key(&checksum)?;
        let provider = self.provider()?;
        provider.upload(&key, bytes.to_vec()).await?;
        self.objects.insert(
            checksum.clone(),
            StoredObject {
                checksum,
                storage_key: key.clone(),
                size: bytes.len() as u64,
                ref_count: 0,
                created_at: now(),
            },
        );
        Ok((key, true))
    }

    /// Add one reference to the object with `checksum` (called once
    /// per version that points at it).
    fn retain_object(&self, checksum: &str) {
        if let Some(mut object) = self.objects.get_mut(checksum) {
            object.ref_count = object.ref_count.saturating_add(1);
        }
    }

    /// Remove one reference to the object with `checksum`, deleting
    /// the stored bytes when the last reference goes away. The
    /// delete is best-effort: a failed provider delete keeps the
    /// object row so the bytes are not lost silently.
    async fn release_object(&self, checksum: &str) {
        let should_delete = match self.objects.get_mut(checksum) {
            Some(mut object) => {
                object.ref_count = object.ref_count.saturating_sub(1);
                object.ref_count == 0
            }
            None => false,
        };
        if !should_delete {
            return;
        }
        let key = match self.objects.get(checksum) {
            Some(object) => object.storage_key.clone(),
            None => return,
        };
        let deleted = match self.provider() {
            Ok(provider) => provider.delete(&key).await.is_ok(),
            Err(_) => false,
        };
        if deleted {
            self.objects.remove(checksum);
        }
    }

    /// Ask the Storage Management module whether `bytes` may be
    /// uploaded for `owner` / `company_id`, through the
    /// `storage.object.upload_requested` event. Returns `Ok(())`
    /// when the module answers that the quota is fine, or when no
    /// handler answers at all (then the local quotas below decide).
    /// A `quota_exceeded` answer rejects the upload with 409 and
    /// emits `document.quota.exceeded`.
    fn check_storage_quota(
        &self,
        owner: &str,
        company_id: &str,
        bytes: u64,
        mime: &str,
    ) -> UnifiedResult<()> {
        let context = self.bus().emit(
            "storage.object.upload_requested",
            json!({
                "module": "documents",
                "owner": owner,
                "company_id": if company_id.is_empty() { Value::Null } else { Value::String(company_id.to_string()) },
                "key_hint": "documents",
                "bytes": bytes,
                "mime": mime,
            }),
        );
        let Some(answer) = context.results.into_iter().next() else {
            // No Storage Management handler answered: fall through
            // to the local quota check.
            return self.check_local_quota(owner, company_id, bytes);
        };
        if answer.get("quota_exceeded").and_then(|v| v.as_bool()).unwrap_or(false) {
            let detail = answer
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Storage quota exceeded")
                .to_string();
            self.emit(
                events::QUOTA_EXCEEDED,
                json!({
                    "scope": answer.get("scope").cloned().unwrap_or(Value::Null),
                    "scope_id": answer.get("scope_id").cloned().unwrap_or(Value::Null),
                    "module": "documents",
                    "user_id": owner,
                    "company_id": company_id,
                    "bytes": bytes,
                    "detail": detail,
                }),
            );
            return Err(UnifiedError::new(
                format!("Upload rejected by storage quota: {detail}"),
                409,
            ));
        }
        if answer.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Ok(());
        }
        // The module answered but did not resolve (no binding, or an
        // error): do not treat that as a quota rejection; the local
        // quotas still apply.
        self.check_local_quota(owner, company_id, bytes)
    }

    /// Local per-company / per-user quota check (fallback). A scope
    /// with no quota row is unlimited locally.
    fn check_local_quota(
        &self,
        owner: &str,
        company_id: &str,
        bytes: u64,
    ) -> UnifiedResult<()> {
        for (scope, scope_id) in [("company", company_id), ("user", owner)] {
            if scope_id.is_empty() {
                continue;
            }
            let key = format!("{scope}:{scope_id}");
            if let Some(quota) = self.quotas.get(&key) {
                if let Some(limit) = quota.bytes_limit {
                    if quota.used_bytes.saturating_add(bytes) > limit {
                        let detail = format!(
                            "Local {scope} quota exceeded for \"{scope_id}\": used {} + {bytes} exceeds the limit of {limit} bytes",
                            quota.used_bytes
                        );
                        self.emit(
                            events::QUOTA_EXCEEDED,
                            json!({
                                "scope": scope,
                                "scope_id": scope_id,
                                "module": "documents",
                                "user_id": owner,
                                "company_id": company_id,
                                "bytes": bytes,
                                "detail": detail,
                            }),
                        );
                        return Err(UnifiedError::new(
                            format!("Upload rejected by quota: {detail}"),
                            409,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Add `bytes` to the local quota counters for the company and
    /// the user (rows are created lazily with no limit).
    fn add_local_usage(&self, owner: &str, company_id: &str, bytes: u64) {
        for (scope, scope_id) in [("company", company_id), ("user", owner)] {
            if scope_id.is_empty() {
                continue;
            }
            let key = format!("{scope}:{scope_id}");
            if let Some(mut quota) = self.quotas.get_mut(&key) {
                quota.used_bytes = quota.used_bytes.saturating_add(bytes);
                quota.updated_at = now();
            } else {
                let quota = DocumentQuota {
                    id: new_id(),
                    scope: scope.to_string(),
                    scope_id: scope_id.to_string(),
                    bytes_limit: None,
                    used_bytes: bytes,
                    created_at: now(),
                    updated_at: now(),
                };
                self.quotas.insert(key, quota);
            }
        }
    }

    /// Subtract `bytes` from the local quota counters (never below 0).
    fn remove_local_usage(&self, owner: &str, company_id: &str, bytes: u64) {
        for (scope, scope_id) in [("company", company_id), ("user", owner)] {
            if scope_id.is_empty() {
                continue;
            }
            let key = format!("{scope}:{scope_id}");
            if let Some(mut quota) = self.quotas.get_mut(&key) {
                quota.used_bytes = quota.used_bytes.saturating_sub(bytes);
                quota.updated_at = now();
            }
        }
    }

    /// Allowlist rule for `module_id`, if one exists.
    fn allowlist_rule(&self, module_id: &str) -> Option<AllowlistRule> {
        self.allowlist
            .iter()
            .find(|entry| entry.value().module_id == module_id)
            .map(|entry| entry.value().clone())
    }

    /// Validate one file against the per-module allowlist: extension
    /// and MIME must be allowed and the size must fit the rule.
    /// A module with no rule is rejected (fail closed).
    fn check_allowlist(
        &self,
        module_id: &str,
        name: &str,
        mime: &str,
        size: u64,
    ) -> UnifiedResult<()> {
        let rule = self.allowlist_rule(module_id).ok_or_else(|| {
            UnifiedError::unprocessable(format!(
                "No allowlist rule exists for module \"{module_id}\"; an admin must create one before files can be uploaded for that module"
            ))
        })?;
        let extension = Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();
        if extension.is_empty() || !rule.extensions.iter().any(|allowed| allowed == &extension) {
            return Err(UnifiedError::unprocessable(format!(
                "File extension \".{extension}\" is not allowed for module \"{module_id}\""
            )));
        }
        if !rule.mime_types.is_empty()
            && !rule.mime_types.iter().any(|allowed| allowed == mime)
        {
            return Err(UnifiedError::unprocessable(format!(
                "MIME type \"{mime}\" is not allowed for module \"{module_id}\""
            )));
        }
        if size > rule.max_bytes {
            return Err(UnifiedError::new(
                format!(
                    "File size {size} bytes exceeds the allowlist limit of {} bytes for module \"{module_id}\"",
                    rule.max_bytes
                ),
                413,
            ));
        }
        Ok(())
    }

    /// True when `caller` may read `document`: the owner, or a
    /// caller holding `documents:admin`, or a company/module scope
    /// match. Company and module scope matches need the caller's
    /// company, which the request headers do not carry, so a
    /// non-owner read of a company/module-scoped document requires
    /// the admin capability; the scope is still recorded so a future
    /// company-aware caller can enforce it precisely.
    fn can_read(&self, ctx: &AppContext, caller: &Caller, document: &DocumentRecord) -> bool {
        if document.owner_id == caller.actor {
            return true;
        }
        if check_capability(ctx, &caller.role, PERM_ADMIN) {
            return true;
        }
        false
    }

    /// Validate a signed download token. Returns the document id on
    /// success; rejects expired or tampered tokens. The token form
    /// is `"<document-id>.<expiry-unix>.<hmac-hex>"`; the version
    /// is resolved from the document at redemption time, so the
    /// token always downloads the current version.
    fn verify_signed_token(&self, token: &str) -> UnifiedResult<String> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(UnifiedError::forbidden("Invalid download token"));
        }
        let document_id = parts[0].to_string();
        let expiry: i64 = parts[1]
            .parse()
            .map_err(|_| UnifiedError::forbidden("Invalid download token"))?;
        if expiry <= Utc::now().timestamp() {
            return Err(UnifiedError::forbidden("Download token has expired"));
        }
        let expected = self.sign_token(&document_id, expiry);
        if !constant_time_eq(expected.as_bytes(), parts[2].as_bytes()) {
            return Err(UnifiedError::forbidden("Download token is invalid"));
        }
        Ok(document_id)
    }

    /// HMAC-SHA-256 signature hex for a document id + expiry.
    fn sign_token(&self, document_id: &str, expiry: i64) -> String {
        let key = *self.signing_key.read().expect("signing key lock poisoned");
        let payload = format!("{document_id}:{expiry}");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .expect("HMAC key is always a valid length");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        tag.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Issue a signed download token for `document_id`.
    fn issue_signed_token(&self, document_id: &str, expires_secs: u64) -> (String, DateTime<Utc>) {
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs as i64);
        let expiry = expires_at.timestamp();
        let signature = self.sign_token(document_id, expiry);
        (
            format!("{document_id}.{expiry}.{signature}"),
            expires_at,
        )
    }

    /// Enforce the download scan gate. Returns `Ok(())` when the
    /// download may proceed; otherwise a 403 that names the reason.
    /// `Skipped` is allowed only when the admin setting explicitly
    /// permits unscanned downloads (default: not permitted).
    fn download_gate(&self, document: &DocumentRecord) -> UnifiedResult<()> {
        match ScanStatus::from_str(&document.scan_status) {
            ScanStatus::Clean => Ok(()),
            ScanStatus::Infected => Err(UnifiedError::forbidden(
                "Download blocked: the file failed the virus scan",
            )),
            ScanStatus::Pending => Err(UnifiedError::forbidden(
                "Download blocked: the file has not finished scanning yet",
            )),
            ScanStatus::Error => Err(UnifiedError::forbidden(
                "Download blocked: the virus scan failed; rescan the file first",
            )),
            ScanStatus::Skipped => {
                if self.settings().allow_unscanned_downloads {
                    Ok(())
                } else {
                    Err(UnifiedError::forbidden(
                        "Download blocked: the file has not been virus-scanned and unscanned downloads are not permitted",
                    ))
                }
            }
        }
    }

    /// Record one download (audit trail + event).
    fn log_download(&self, document: &DocumentRecord, user_id: &str, via: &str) {
        let record = DownloadRecord {
            id: new_id(),
            document_id: document.id.clone(),
            version: document.current_version,
            user_id: user_id.to_string(),
            via: via.to_string(),
            scan_status: document.scan_status.clone(),
            bytes: document.size,
            downloaded_at: now(),
        };
        self.downloads.insert(record.id.clone(), record);
        self.emit(
            events::DOCUMENT_DOWNLOADED,
            json!({
                "document_id": document.id,
                "version": document.current_version,
                "user_id": user_id,
                "via": via,
                "scan_status": document.scan_status,
                "bytes": document.size,
            }),
        );
    }

    /// Shared finalize path for every upload route: validate the
    /// allowlist and quota, scan, deduplicate + store the bytes,
    /// then create the document (or a new version of
    /// `existing_document_id`). Emits `document.uploaded` or
    /// `document.version.created`.
    async fn finalize_upload(
        &self,
        name: String,
        module_id: String,
        company_id: String,
        owner_id: String,
        scope: String,
        bytes: Vec<u8>,
        existing_document_id: Option<String>,
    ) -> UnifiedResult<DocumentRecord> {
        // Fail before storing anything when the target document of a
        // re-upload does not exist, so no orphan object is left.
        if let Some(id) = &existing_document_id {
            self.load_document(id)?;
        }
        let mime = mime_for(&name, &bytes);
        let size = bytes.len() as u64;
        if size == 0 {
            return Err(UnifiedError::bad_request("Uploaded file is empty"));
        }
        if size > self.settings().max_upload_bytes {
            return Err(UnifiedError::new(
                format!(
                    "Upload exceeds the {} byte size limit",
                    self.settings().max_upload_bytes
                ),
                413,
            ));
        }
        self.check_allowlist(&module_id, &name, &mime, size)?;
        self.check_storage_quota(&owner_id, &company_id, size, &mime)?;
        let checksum = sha256_hex(&bytes);
        let (storage_key, _is_new) = self.store_content(&bytes).await?;
        // Scan the bytes before the file becomes downloadable. The
        // version number for the scan event is known once the
        // document row exists, so scan first and record the verdict
        // on both rows below.
        let target_id = existing_document_id
            .clone()
            .unwrap_or_else(new_id);
        let version_number = match &existing_document_id {
            Some(id) => self
                .documents
                .get(id)
                .map(|entry| entry.current_version + 1)
                .unwrap_or(1),
            None => 1,
        };
        let (scan_status, scan_threat) =
            self.scan_document_bytes(&target_id, version_number, &bytes);
        self.retain_object(&checksum);
        let version = DocumentVersion {
            id: new_id(),
            document_id: target_id.clone(),
            version: version_number,
            checksum: checksum.clone(),
            storage_key: storage_key.clone(),
            size,
            mime: mime.clone(),
            uploader_id: owner_id.clone(),
            scan_status: scan_status.as_str().to_string(),
            scan_threat: scan_threat.clone(),
            created_at: now(),
        };
        self.versions.insert(version.id.clone(), version.clone());
        let record = match &existing_document_id {
            Some(id) => {
                let mut document = self
                    .documents
                    .get(id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Document not found"))?;
                document.name = name.clone();
                document.mime = mime.clone();
                document.size = size;
                document.checksum = checksum.clone();
                document.storage_key = storage_key.clone();
                document.current_version = version_number;
                document.version_count += 1;
                document.scan_status = scan_status.as_str().to_string();
                document.scan_threat = scan_threat.clone();
                document.updated_at = now();
                self.documents.insert(document.id.clone(), document.clone());
                self.emit(
                    events::VERSION_CREATED,
                    serde_json::to_value(&version).unwrap_or(Value::Null),
                );
                document
            }
            None => {
                let document = DocumentRecord {
                    id: target_id.clone(),
                    name: name.clone(),
                    module_id: module_id.clone(),
                    company_id: company_id.clone(),
                    owner_id: owner_id.clone(),
                    scope,
                    mime: mime.clone(),
                    size,
                    checksum: checksum.clone(),
                    storage_key: storage_key.clone(),
                    current_version: 1,
                    version_count: 1,
                    scan_status: scan_status.as_str().to_string(),
                    scan_threat: scan_threat.clone(),
                    created_at: now(),
                    updated_at: now(),
                };
                self.documents.insert(document.id.clone(), document.clone());
                self.emit(
                    events::DOCUMENT_UPLOADED,
                    serde_json::to_value(&document).unwrap_or(Value::Null),
                );
                document
            }
        };
        self.add_local_usage(&owner_id, &company_id, size);
        Ok(record)
    }

    /// Load a document or return a 404.
    fn load_document(&self, id: &str) -> UnifiedResult<DocumentRecord> {
        self.documents
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Document not found"))
    }

    /// Delete a document and all of its versions, releasing the
    /// deduplicated objects and the local quota usage.
    async fn delete_document(&self, id: &str) -> UnifiedResult<DocumentRecord> {
        let document = self.load_document(id)?;
        let version_ids: Vec<String> = self
            .versions
            .iter()
            .filter(|entry| entry.value().document_id == id)
            .map(|entry| entry.key().clone())
            .collect();
        for version_id in version_ids {
            if let Some((_, version)) = self.versions.remove(&version_id) {
                self.release_object(&version.checksum).await;
            }
        }
        self.remove_local_usage(&document.owner_id, &document.company_id, document.size);
        self.documents.remove(id);
        self.emit(
            events::DOCUMENT_DELETED,
            json!({
                "id": document.id,
                "name": document.name,
                "owner_id": document.owner_id,
                "company_id": document.company_id,
            }),
        );
        Ok(document)
    }

    /// Expired sessions (and their parts) removed by the purge.
    /// Returns the number of sessions removed.
    fn purge_expired_sessions(&self) -> usize {
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().expires_at <= Utc::now())
            .map(|entry| entry.key().clone())
            .collect();
        let count = expired.len();
        for id in expired {
            self.sessions.remove(&id);
            self.parts.remove(&id);
        }
        count
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

/// Strip any directory components from an uploaded name. Returns an
/// error when nothing usable remains (for example `..` or `/`).
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

/// Image dimensions from the image header bytes only — no rendering.
/// Supports PNG, GIF, JPEG (first SOF marker), and WebP (VP8X / VP8
/// lossy / VP8L). Returns `None` for anything else.
fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png" => {
            if bytes.len() >= 24 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
                let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
                let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
                Some((width, height))
            } else {
                None
            }
        }
        "image/gif" => {
            if bytes.len() >= 10 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
                let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
                let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
                Some((width, height))
            } else {
                None
            }
        }
        "image/jpeg" => jpeg_dimensions(bytes),
        "image/webp" => {
            if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
                webp_dimensions(bytes)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut index = 2usize;
    while index + 9 < bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        let marker = bytes[index + 1];
        // SOF0 / SOF1 / SOF2 (skip DHT, JPGn, and restart markers).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let height = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]) as u32;
            return Some((width, height));
        }
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            index += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
        if length < 2 {
            return None;
        }
        index += 2 + length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" => {
            let width = (u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]) + 1) as u32;
            let height = (u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]) + 1) as u32;
            Some((width, height))
        }
        b"VP8 " => {
            // Lossy VP8: 14-bit width / height at offset 26 / 28.
            let width = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF) as u32;
            let height = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF) as u32;
            Some((width, height))
        }
        b"VP8L" => {
            let packed = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (packed & 0x3FFF) + 1;
            let height = ((packed >> 14) & 0x3FFF) + 1;
            Some((width, height))
        }
        _ => None,
    }
}

/// Constant-time equality for HMAC tag comparison (lengths must
/// match; the fold runs over every byte either way).
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

fn is_valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_module_id(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Module id must be 1..=128 characters",
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "Module id must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    Ok(trimmed.to_string())
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

fn valid_scope(value: &str) -> UnifiedResult<String> {
    match value.trim() {
        "owner" | "company" | "module" => Ok(value.trim().to_string()),
        _ => Err(UnifiedError::bad_request(
            "Scope must be one of: owner, company, module",
        )),
    }
}

/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against.
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
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

/// Authorize one request: caller identity + one capability. Returns
/// the caller on success so handlers can use the actor id.
fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// Escape a file name for the `Content-Disposition` header: strip
/// quotes, backslashes, and control characters.
fn safe_download_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect()
}

// ---------------------------------------------------------------------------
// Handlers — documents
// ---------------------------------------------------------------------------

async fn list_documents(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let is_admin = check_capability(&ctx, &found.role, PERM_ADMIN);
    let mut items: Vec<DocumentRecord> = state
        .documents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|document| {
            // Non-admin callers only see their own documents in the
            // list; admins see everything (filters still apply).
            (is_admin || document.owner_id == found.actor)
                && matches_search(&document.name, &params)
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
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn get_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<DocumentRecord> = (|| {
        let document = state.load_document(path.as_str())?;
        if !state.can_read(&ctx, &found, &document) {
            return Err(UnifiedError::forbidden(
                "You do not have access to this document",
            ));
        }
        Ok(document)
    })();
    json::respond(result, false)
}

/// Multipart multi-file upload (drag-drop): text fields
/// `module_id`, `company_id` (optional), `scope` (optional,
/// default `owner`) plus one `file` field per file. Each file is
/// validated against the per-module allowlist before acceptance;
/// per-file outcomes are reported so one bad file does not lose
/// the rest of the batch.
async fn upload_documents(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    mut payload: Multipart,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<MultiUploadResult> = (|| async {
        let mut module_id = "documents".to_string();
        let mut company_id = String::new();
        let mut scope = "owner".to_string();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
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
                "module_id" | "company_id" | "scope" => {
                    let mut text = String::new();
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
                    {
                        text.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    match name.as_str() {
                        "module_id" => module_id = text,
                        "company_id" => company_id = text,
                        _ => scope = text,
                    }
                }
                "file" => {
                    let raw_name = field
                        .content_disposition()
                        .get_filename()
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
        let scope = valid_scope(&scope)?;
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
                    bytes,
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
    })()
    .await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn delete_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Value> = (|| async {
        let document = state.load_document(path.as_str())?;
        // Only the owner or an admin may delete a document.
        if document.owner_id != found.actor
            && !check_capability(&ctx, &found.role, PERM_ADMIN)
        {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin may delete this document",
            ));
        }
        state.delete_document(path.as_str()).await?;
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })()
    .await;
    json::respond(result, false)
}

async fn preview_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<DocumentPreview> = (|| async {
        let document = state.load_document(path.as_str())?;
        if !state.can_read(&ctx, &found, &document) {
            return Err(UnifiedError::forbidden(
                "You do not have access to this document",
            ));
        }
        let (width, height) = match state.provider()?.download(&document.storage_key).await {
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
    json::respond(result, false)
}

/// Stream the current version's bytes. The scan gate runs before
/// any byte is sent.
async fn download_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let document = match state.load_document(path.as_str()) {
        Ok(document) => document,
        Err(error) => return json::error_response(&error, false),
    };
    if !state.can_read(&ctx, &found, &document) {
        return json::error_response(
            &UnifiedError::forbidden("You do not have access to this document"),
            false,
        );
    }
    if let Err(error) = state.download_gate(&document) {
        return json::error_response(&error, false);
    }
    let provider = match state.provider() {
        Ok(provider) => provider,
        Err(error) => return json::error_response(&error, false),
    };
    let bytes = match provider.download(&document.storage_key).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return json::error_response(
                &UnifiedError::not_found("Stored document is missing"),
                false,
            );
        }
    };
    state.log_download(&document, &found.actor, "direct");
    let mime = document.mime.clone();
    let name = safe_download_name(&document.name);
    HttpResponse::Ok()
        .content_type(mime)
        .append_header((
            "Content-Disposition",
            format!("attachment; filename=\"{name}\""),
        ))
        .body(Bytes::from(bytes))
}

/// Redeem a signed download URL. The token is the capability here:
/// no `X-User-Id` / `X-Role` headers are required. The scan gate
/// still applies, and the download is logged with user
/// `"signed-url"`.
async fn download_signed(
    state: web::Data<Arc<DocumentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let document_id = match state.verify_signed_token(path.as_str()) {
        Ok(document_id) => document_id,
        Err(error) => return json::error_response(&error, false),
    };
    let document = match state.load_document(&document_id) {
        Ok(document) => document,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = state.download_gate(&document) {
        return json::error_response(&error, false);
    }
    let provider = match state.provider() {
        Ok(provider) => provider,
        Err(error) => return json::error_response(&error, false),
    };
    let bytes = match provider.download(&document.storage_key).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return json::error_response(
                &UnifiedError::not_found("Stored document is missing"),
                false,
            );
        }
    };
    state.log_download(&document, "signed-url", "signed-url");
    let mime = document.mime.clone();
    let name = safe_download_name(&document.name);
    HttpResponse::Ok()
        .content_type(mime)
        .append_header((
            "Content-Disposition",
            format!("attachment; filename=\"{name}\""),
        ))
        .body(Bytes::from(bytes))
}

async fn issue_signed_url(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignedUrlRequest>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<SignedUrlResponse> = (|| {
        let document = state.load_document(path.as_str())?;
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
        let (token, expires_at) = state.issue_signed_token(&document.id, expires_secs);
        Ok(SignedUrlResponse {
            document_id: document.id.clone(),
            url: format!("/api/v1/documents/download/{token}"),
            token,
            expires_at,
        })
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers — versions
// ---------------------------------------------------------------------------

/// Re-upload one file as a new version of an existing document
/// (multipart field `file`).
async fn upload_new_version(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: Multipart,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<DocumentRecord> = (|| async {
        let document = state.load_document(path.as_str())?;
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
                .get_name()
                .unwrap_or("")
                .to_string();
            if name == "file" {
                let raw_name = field
                    .content_disposition()
                    .get_filename()
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
        state
            .finalize_upload(
                file_name,
                document.module_id.clone(),
                document.company_id.clone(),
                found.actor.clone(),
                document.scope.clone(),
                bytes,
                Some(document.id.clone()),
            )
            .await
    })()
    .await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_versions(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let document = match state.load_document(path.as_str()) {
        Ok(document) => document,
        Err(error) => return json::error_response(&error, false),
    };
    if !state.can_read(&ctx, &found, &document) {
        return json::error_response(
            &UnifiedError::forbidden("You do not have access to this document"),
            false,
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

async fn get_version(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let (document_id, version_number) = path.into_inner();
    let result: UnifiedResult<DocumentVersion> = (|| {
        let document = state.load_document(&document_id)?;
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
    json::respond(result, false)
}

/// Restore an old version: creates a NEW version with the old
/// content (the stored object reference count grows by one).
/// History is never mutated.
async fn restore_version(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let (document_id, version_number) = path.into_inner();
    let result: UnifiedResult<DocumentRecord> = (|| async {
        let document = state.load_document(&document_id)?;
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
        let bytes = state.provider()?.download(&old.storage_key).await?;
        let new_version_number = document.current_version + 1;
        let (scan_status, scan_threat) =
            state.scan_document_bytes(&document.id, new_version_number, &bytes);
        state.retain_object(&old.checksum);
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
        let mut updated = document.clone();
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
        state.emit(
            events::VERSION_CREATED,
            serde_json::to_value(&version).unwrap_or(Value::Null),
        );
        Ok(updated)
    })()
    .await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Re-run the virus scan on the current version (admin). The
/// recorded verdict replaces the previous one; the version history
/// is untouched.
async fn rescan_document(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _found = match authorize(&ctx, &req, PERM_ADMIN) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<DocumentRecord> = (|| async {
        let document = state.load_document(path.as_str())?;
        let bytes = state.provider()?.download(&document.storage_key).await?;
        let (scan_status, scan_threat) = state.scan_document_bytes(
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
    })()
    .await;
    json::respond(result, false)
}

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

async fn initiate_session(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<InitiateSessionRequest>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<UploadSession> = (|| {
        let input = body.into_inner();
        let name = sanitize_file_name(&input.name)?;
        let module_id = valid_module_id(&input.module_id)?;
        let company_id = input.company_id.unwrap_or_default();
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
        let session = UploadSession {
            id: new_id(),
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
            expires_at: now() + chrono::Duration::seconds(state.settings().session_ttl_secs),
            created_at: now(),
            updated_at: now(),
        };
        state.sessions.insert(session.id.clone(), session.clone());
        state.parts.insert(session.id.clone(), DashMap::new());
        Ok(session)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_sessions(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let is_admin = check_capability(&ctx, &found.role, PERM_ADMIN);
    let mut items: Vec<UploadSession> = state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
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

async fn get_session(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
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
    json::respond(result, false)
}

/// Upload one part: multipart fields `part_index` (0-based),
/// `checksum` (SHA-256 hex of this part's bytes), and `part`
/// (the bytes). A part whose checksum does not match is rejected
/// and not stored, so the client can retry just that part.
async fn upload_part(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: Multipart,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<UploadSession> = (|| async {
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
                .get_name()
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
                        text.push_str(&String::from_utf8_lossy(&chunk));
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
        let part_index = part_index
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"part_index\" is required"))?;
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
        let part = UploadPart {
            index: part_index,
            size: bytes.len() as u64,
            sha256: checksum,
            bytes,
            received_at: now(),
        };
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
    })()
    .await;
    json::respond(result, false)
}

/// Assemble the parts in order, verify the full SHA-256, then run
/// the virus scan and finalize the document (or a new version).
async fn complete_session(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
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
        let mut bytes: Vec<u8> = Vec::with_capacity(session.total_size as usize);
        let parts_map = state
            .parts
            .get(&session.id)
            .ok_or_else(|| UnifiedError::internal("Upload session parts are missing"))?;
        for index in 0..session.part_count {
            let part = parts_map
                .get(&index)
                .ok_or_else(|| UnifiedError::internal(format!("Part {index} is missing")))?;
            bytes.extend_from_slice(&part.bytes);
        }
        if bytes.len() as u64 != session.total_size {
            return Err(UnifiedError::internal(
                "Assembled size does not match the session total size",
            ));
        }
        let actual = sha256_hex(&bytes);
        if actual != session.expected_sha256 {
            return Err(UnifiedError::bad_request(format!(
                "Assembled file checksum mismatch: expected {}, got {actual}",
                session.expected_sha256
            )));
        }
        let document = state
            .finalize_upload(
                session.name.clone(),
                session.module_id.clone(),
                session.company_id.clone(),
                session.owner_id.clone(),
                "owner".to_string(),
                bytes,
                session.document_id.clone(),
            )
            .await?;
        if let Some(mut entry) = state.sessions.get_mut(&session.id) {
            entry.status = "completed".to_string();
            entry.updated_at = now();
        }
        state.parts.remove(&session.id);
        Ok(document)
    })()
    .await;
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Abort a session and delete its partial parts.
async fn abort_session(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_WRITE) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
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
        state.sessions.remove(path.as_str());
        state.parts.remove(path.as_str());
        Ok(json!({ "aborted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn purge_expired_sessions(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let purged = state.purge_expired_sessions();
    json::success_response(json!({ "purged": purged }), 200)
}

// ---------------------------------------------------------------------------
// Handlers — allowlist (admin)
// ---------------------------------------------------------------------------

async fn list_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let mut items: Vec<AllowlistRule> = state
        .allowlist
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            matches_search(&rule.module_id, &params)
                && params
                    .filters
                    .get("module_id")
                    .is_none_or(|value| rule.module_id == *value)
        })
        .collect();
    items.sort_by(|a, b| a.module_id.cmp(&b.module_id));
    json::success_response(paginate(items, &params), 200)
}

fn validate_extensions(raw: &[String]) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for value in raw {
        let extension = value.trim().trim_start_matches('.').to_lowercase();
        if extension.is_empty() || extension.len() > 16 {
            return Err(UnifiedError::bad_request(
                "Extensions must be 1..=16 characters",
            ));
        }
        if !extension
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UnifiedError::bad_request(
                "Extensions must contain only letters, digits, or '-'",
            ));
        }
        if !cleaned.contains(&extension) {
            cleaned.push(extension);
        }
    }
    if cleaned.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one allowed extension is required",
        ));
    }
    Ok(cleaned)
}

fn validate_mime_types(raw: &[String]) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for value in raw {
        let mime = value.trim().to_lowercase();
        if mime.is_empty() || mime.len() > 128 || !mime.contains('/') {
            return Err(UnifiedError::bad_request(
                "MIME types must look like \"type/subtype\"",
            ));
        }
        if !cleaned.contains(&mime) {
            cleaned.push(mime);
        }
    }
    Ok(cleaned)
}

async fn create_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateAllowlistRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<AllowlistRule> = (|| {
        let input = body.into_inner();
        let module_id = valid_module_id(&input.module_id)?;
        if state.allowlist_rule(&module_id).is_some() {
            return Err(UnifiedError::conflict(format!(
                "An allowlist rule already exists for module \"{module_id}\""
            )));
        }
        if input.max_bytes == 0 || input.max_bytes > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::bad_request(format!(
                "max_bytes must be between 1 and {MAX_UPLOAD_BYTES}"
            )));
        }
        let rule = AllowlistRule {
            id: new_id(),
            module_id,
            extensions: validate_extensions(&input.extensions)?,
            mime_types: validate_mime_types(&input.mime_types)?,
            max_bytes: input.max_bytes,
            created_at: now(),
            updated_at: now(),
        };
        state.allowlist.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<AllowlistRule> = state
        .allowlist
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Allowlist rule not found"));
    json::respond(result, false)
}

async fn update_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAllowlistRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<AllowlistRule> = (|| {
        let input = body.into_inner();
        let mut rule = state
            .allowlist
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Allowlist rule not found"))?;
        if let Some(extensions) = input.extensions {
            rule.extensions = validate_extensions(&extensions)?;
        }
        if let Some(mime_types) = input.mime_types {
            rule.mime_types = validate_mime_types(&mime_types)?;
        }
        if let Some(max_bytes) = input.max_bytes {
            if max_bytes == 0 || max_bytes > MAX_UPLOAD_BYTES {
                return Err(UnifiedError::bad_request(format!(
                    "max_bytes must be between 1 and {MAX_UPLOAD_BYTES}"
                )));
            }
            rule.max_bytes = max_bytes;
        }
        rule.updated_at = now();
        state.allowlist.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    json::respond(result, false)
}

async fn delete_allowlist(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let removed = state.allowlist.remove(path.as_str()).is_some();
    if !removed {
        return json::error_response(
            &UnifiedError::not_found("Allowlist rule not found"),
            false,
        );
    }
    json::success_response(json!({ "deleted": true, "id": path.as_str() }), 200)
}

// ---------------------------------------------------------------------------
// Handlers — quotas, scanners, settings, downloads
// ---------------------------------------------------------------------------

async fn quota_usage(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let filters = query.into_inner();
    let company_id = filters.get("company_id").cloned().unwrap_or_default();
    let user_id = filters
        .get("user_id")
        .cloned()
        .unwrap_or_else(|| found.actor.clone());
    // A caller may only ask about their own usage unless admin.
    if user_id != found.actor && !check_capability(&ctx, &found.role, PERM_ADMIN) {
        return json::error_response(
            &UnifiedError::forbidden("You may only view your own quota usage"),
            false,
        );
    }
    let mut usage: Vec<Value> = Vec::new();
    for (scope, scope_id) in [("company", company_id.as_str()), ("user", user_id.as_str())] {
        if scope_id.is_empty() {
            continue;
        }
        let key = format!("{scope}:{scope_id}");
        let (used, limit) = match state.quotas.get(&key) {
            Some(quota) => (quota.used_bytes, quota.bytes_limit),
            None => (0, None),
        };
        usage.push(json!({
            "scope": scope,
            "scope_id": scope_id,
            "used_bytes": used,
            "bytes_limit": limit,
            "remaining_bytes": limit.map(|value| value.saturating_sub(used)),
        }));
    }
    json::success_response(json!({ "usage": usage }), 200)
}

async fn list_quotas(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let mut items: Vec<DocumentQuota> = state
        .quotas
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|quota| {
            params
                .filters
                .get("scope")
                .is_none_or(|value| quota.scope == *value)
                && params
                    .filters
                    .get("scope_id")
                    .is_none_or(|value| quota.scope_id == *value)
        })
        .collect();
    items.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then(a.scope_id.cmp(&b.scope_id))
    });
    json::success_response(paginate(items, &params), 200)
}

async fn create_quota(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateQuotaRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<DocumentQuota> = (|| {
        let input = body.into_inner();
        let scope = match input.scope.trim() {
            "company" | "user" => input.scope.trim().to_string(),
            _ => {
                return Err(UnifiedError::bad_request(
                    "scope must be \"company\" or \"user\"",
                ))
            }
        };
        let scope_id = require_text(&input.scope_id, "scope_id", 128)?;
        let key = format!("{scope}:{scope_id}");
        let mut quota = state
            .quotas
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or(DocumentQuota {
                id: new_id(),
                scope: scope.clone(),
                scope_id: scope_id.clone(),
                bytes_limit: None,
                used_bytes: 0,
                created_at: now(),
                updated_at: now(),
            });
        quota.bytes_limit = input.bytes_limit;
        quota.updated_at = now();
        state.quotas.insert(key, quota.clone());
        Ok(quota)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_scanners(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, false);
    }
    json::success_response(
        json!({ "scanners": state.scanner_names(), "count": state.scanners.len() }),
        200,
    )
}

async fn get_settings(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, false);
    }
    json::success_response(state.settings(), 200)
}

async fn update_settings(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<DocumentSettings> = (|| {
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(value) = input.allow_unscanned_downloads {
            settings.allow_unscanned_downloads = value;
        }
        if let Some(value) = input.default_chunk_size {
            if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "default_chunk_size must be between {MIN_CHUNK_SIZE} and {MAX_CHUNK_SIZE}"
                )));
            }
            settings.default_chunk_size = value;
        }
        if let Some(value) = input.session_ttl_secs {
            if !(60..=604_800).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "session_ttl_secs must be between 60 and 604800",
                ));
            }
            settings.session_ttl_secs = value;
        }
        if let Some(value) = input.max_upload_bytes {
            if value == 0 || value > MAX_UPLOAD_BYTES {
                return Err(UnifiedError::bad_request(format!(
                    "max_upload_bytes must be between 1 and {MAX_UPLOAD_BYTES}"
                )));
            }
            settings.max_upload_bytes = value;
        }
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    json::respond(result, false)
}

async fn list_downloads(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let is_admin = check_capability(&ctx, &found.role, PERM_ADMIN);
    let mut items: Vec<DownloadRecord> = state
        .downloads
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            (is_admin || record.user_id == found.actor)
                && params
                    .filters
                    .get("document_id")
                    .is_none_or(|value| record.document_id == *value)
                && params
                    .filters
                    .get("via")
                    .is_none_or(|value| record.via == *value)
                && params
                    .filters
                    .get("scan_status")
                    .is_none_or(|value| record.scan_status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.downloaded_at.cmp(&a.downloaded_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct DocumentsModule {
    state: Arc<DocumentsState>,
}

impl DocumentsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(DocumentsState::new()),
        }
    }

    /// Register a virus scanner with this module's state.
    pub fn register_scanner(&self, scanner: Arc<dyn DocumentScanner>) {
        self.state.register_scanner(scanner);
    }

    /// Remove a virus scanner by name.
    pub fn unregister_scanner(&self, name: &str) -> bool {
        self.state.unregister_scanner(name)
    }
}

impl Default for DocumentsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DocumentsModule {
    fn id(&self) -> &'static str {
        "documents"
    }

    fn name(&self) -> &'static str {
        "Documents"
    }

    fn description(&self) -> &'static str {
        "Documents: chunked uploads, per-module allowlists, virus-scan hook, versioning, signed downloads, deduplication, and quotas."
    }

    fn requires(&self) -> &[&'static str] {
        &["storage_manager", "roles"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["infer", "hmac", "sha2", "actix-multipart", "dashmap"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {
        // This module emits document.* events and listens to nothing
        // it does not need.
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/documents", owner: "documents", description: "List documents" },
            RouteInfo { method: "POST", path: "/api/v1/documents/upload", owner: "documents", description: "Multipart multi-file upload" },
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}", owner: "documents", description: "Get document" },
            RouteInfo { method: "DELETE", path: "/api/v1/documents/{id}", owner: "documents", description: "Delete document" },
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/preview", owner: "documents", description: "Preview metadata" },
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/download", owner: "documents", description: "Download current version" },
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/signed-url", owner: "documents", description: "Issue signed download URL" },
            RouteInfo { method: "GET", path: "/api/v1/documents/download/{token}", owner: "documents", description: "Redeem signed download URL" },
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/versions", owner: "documents", description: "Re-upload as new version" },
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/versions", owner: "documents", description: "List versions" },
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/versions/{version}", owner: "documents", description: "Get version" },
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/versions/{version}/restore", owner: "documents", description: "Restore version" },
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/rescan", owner: "documents", description: "Re-run virus scan" },
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions", owner: "documents", description: "Initiate chunked upload session" },
            RouteInfo { method: "GET", path: "/api/v1/documents/uploads/sessions", owner: "documents", description: "List upload sessions" },
            RouteInfo { method: "GET", path: "/api/v1/documents/uploads/sessions/{id}", owner: "documents", description: "Upload session status" },
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions/{id}/parts", owner: "documents", description: "Upload one chunk part" },
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions/{id}/complete", owner: "documents", description: "Complete chunked upload" },
            RouteInfo { method: "DELETE", path: "/api/v1/documents/uploads/sessions/{id}", owner: "documents", description: "Abort upload session" },
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions/purge-expired", owner: "documents", description: "Purge expired upload sessions" },
            RouteInfo { method: "GET", path: "/api/v1/documents/allowlist", owner: "documents", description: "List allowlist rules" },
            RouteInfo { method: "POST", path: "/api/v1/documents/allowlist", owner: "documents", description: "Create allowlist rule" },
            RouteInfo { method: "GET", path: "/api/v1/documents/allowlist/{id}", owner: "documents", description: "Get allowlist rule" },
            RouteInfo { method: "PATCH", path: "/api/v1/documents/allowlist/{id}", owner: "documents", description: "Update allowlist rule" },
            RouteInfo { method: "DELETE", path: "/api/v1/documents/allowlist/{id}", owner: "documents", description: "Delete allowlist rule" },
            RouteInfo { method: "GET", path: "/api/v1/documents/quotas/usage", owner: "documents", description: "Quota usage" },
            RouteInfo { method: "GET", path: "/api/v1/documents/quotas", owner: "documents", description: "List local quotas" },
            RouteInfo { method: "POST", path: "/api/v1/documents/quotas", owner: "documents", description: "Create local quota" },
            RouteInfo { method: "GET", path: "/api/v1/documents/scanners", owner: "documents", description: "List registered scanners" },
            RouteInfo { method: "GET", path: "/api/v1/documents/settings", owner: "documents", description: "Get document settings" },
            RouteInfo { method: "PATCH", path: "/api/v1/documents/settings", owner: "documents", description: "Update document settings" },
            RouteInfo { method: "GET", path: "/api/v1/documents/downloads", owner: "documents", description: "List download records" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/documents")
                // Literal segments first: `/download/{token}` must
                // win over `/{id}` for a token redemption.
                .route("/download/{token}", web::get().to(download_signed))
                .route("", web::get().to(list_documents))
                .route("/upload", web::post().to(upload_documents))
                .route("/{id}", web::get().to(get_document))
                .route("/{id}", web::delete().to(delete_document))
                .route("/{id}/preview", web::get().to(preview_document))
                .route("/{id}/download", web::get().to(download_document))
                .route("/{id}/signed-url", web::post().to(issue_signed_url))
                .route("/{id}/versions", web::post().to(upload_new_version))
                .route("/{id}/versions", web::get().to(list_versions))
                .route("/{id}/versions/{version}", web::get().to(get_version))
                .route("/{id}/versions/{version}/restore", web::post().to(restore_version))
                .route("/{id}/rescan", web::post().to(rescan_document))
                .route("/uploads/sessions", web::post().to(initiate_session))
                .route("/uploads/sessions", web::get().to(list_sessions))
                .route("/uploads/sessions/purge-expired", web::post().to(purge_expired_sessions))
                .route("/uploads/sessions/{id}", web::get().to(get_session))
                .route("/uploads/sessions/{id}/parts", web::post().to(upload_part))
                .route("/uploads/sessions/{id}/complete", web::post().to(complete_session))
                .route("/uploads/sessions/{id}", web::delete().to(abort_session))
                .route("/allowlist", web::get().to(list_allowlist))
                .route("/allowlist", web::post().to(create_allowlist))
                .route("/allowlist/{id}", web::get().to(get_allowlist))
                .route("/allowlist/{id}", web::patch().to(update_allowlist))
                .route("/allowlist/{id}", web::delete().to(delete_allowlist))
                .route("/quotas/usage", web::get().to(quota_usage))
                .route("/quotas", web::get().to(list_quotas))
                .route("/quotas", web::post().to(create_quota))
                .route("/scanners", web::get().to(list_scanners))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/downloads", web::get().to(list_downloads)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DocumentsModule::new()));
}
