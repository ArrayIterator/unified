// documents — Shared types: constants, enums, records, and request/response shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use unified_core::config::DocumentsConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::storage::validate_key;
use uuid::Uuid;

/// Maximum single upload through any endpoint (100 MiB), matching
/// the file manager and the storage hub.
pub(crate) const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// Default chunk size for resumable upload sessions (1 MiB).
const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024;

/// Smallest accepted chunk size (64 KiB).
pub(crate) const MIN_CHUNK_SIZE: u64 = 64 * 1024;

/// Largest accepted chunk size (8 MiB).
pub(crate) const MAX_CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// Default lifetime of an upload session (24 hours).
const DEFAULT_SESSION_TTL_SECS: i64 = 86_400;

/// Default lifetime of a signed download URL (1 hour).
pub(crate) const DEFAULT_SIGNED_URL_SECS: u64 = 3_600;

/// Maximum lifetime of a signed download URL (7 days).
pub(crate) const MAX_SIGNED_URL_SECS: u64 = 604_800;

// ---------------------------------------------------------------------------
// Permissions (capability names checked through the `role.check` hook).
// ---------------------------------------------------------------------------

pub(crate) const PERM_READ: &str = "documents:read";

pub(crate) const PERM_WRITE: &str = "documents:write";

pub(crate) const PERM_ADMIN: &str = "documents:admin";

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
/// module runs every registered scanner in sorted scanner-name
/// order (so the reported threat is deterministic) and combines
/// the verdicts: any `Infected` wins (first threat in sorted-name
/// order reported), otherwise any `Error` wins, otherwise `Clean`. With no scanner registered the
/// verdict is `Skipped` (see the module docs for the download rule).
pub trait DocumentScanner: Send + Sync {
    /// Stable name of this scanner, shown by `GET /scanners`.
    fn name(&self) -> &str;
    /// Scan `bytes` and return the verdict.
    fn scan(&self, bytes: &[u8]) -> ScanVerdict;
    /// Scan from a reader instead of a byte slice. The default
    /// implementation reads the whole stream and calls `scan`, so
    /// existing scanners keep working unchanged; override it to
    /// scan incrementally without holding the whole file in RAM.
    fn scan_reader(&self, reader: &mut dyn std::io::Read) -> ScanVerdict {
        let mut bytes = Vec::new();
        match std::io::Read::read_to_end(reader, &mut bytes) {
            Ok(_) => self.scan(&bytes),
            Err(error) => ScanVerdict::Error {
                reason: format!("Cannot read bytes for scanning: {error}"),
            },
        }
    }
    /// True when `scan_reader` streams the input without holding
    /// the whole file in RAM. Scanners that override `scan_reader`
    /// to work incrementally return true; the default is false
    /// (the default `scan_reader` reads the whole stream).
    fn streams_scan(&self) -> bool {
        false
    }
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

    pub(crate) fn from_str(value: &str) -> Self {
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
// Storage layout — tenant / system / temp paths.
// ---------------------------------------------------------------------------

/// Content kind: the second path segment of every stored object key.
/// Exactly three values exist; a new kind is a code change, never a
/// free-form string, so no caller can smuggle an arbitrary directory
/// into the layout.
///
/// NOTE: this is deliberately NOT named `StorageKind`: that name is
/// already the provider kind (`Local` / `S3` / ...) in
/// `unified_core::storage`, imported by this module's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Documents,
    Media,
    Others,
}

impl ContentKind {
    /// Path segment for this kind (`documents`, `media`, `others`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Documents => "documents",
            Self::Media => "media",
            Self::Others => "others",
        }
    }

    /// Parse a kind name (`documents`, `media`, `others`,
    /// case-insensitive). `None` for anything else.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "documents" => Some(Self::Documents),
            "media" => Some(Self::Media),
            "others" => Some(Self::Others),
            _ => None,
        }
    }
}

/// Extension -> content-kind registry (DATA-DRIVEN, not hard-coded
/// in code). The default list ships as config data in
/// `extension_kinds.yaml` next to this file (embedded at compile
/// time, so it travels with the binary); admin overrides come from
/// `documents.extensions` in config.yaml and merge over the
/// defaults. The registry is the source of truth for KNOWN
/// extensions: it validates + normalizes (lowercases) the extension
/// from an original file name, and derives the storage kind when
/// the caller does not specify one explicitly. Unknown extensions
/// fall back to kind `others`, and keep their suffix only when it
/// passes the strict check (`[a-z0-9]{1,10}` after lowercasing).
#[derive(Debug, Clone)]
pub(crate) struct ExtensionRegistry {
    kinds: HashMap<String, ContentKind>,
}

/// Parsed default registry, loaded once from the embedded data
/// file. A broken shipped file degrades to an empty map (every
/// extension then falls back to `others`); the unit test
/// `extension_registry_default_parses` pins the shipped file, so a
/// bad entry fails tests, not production.
static DEFAULT_KINDS: OnceLock<HashMap<String, ContentKind>> = OnceLock::new();

fn default_kinds() -> &'static HashMap<String, ContentKind> {
    DEFAULT_KINDS.get_or_init(|| {
        serde_yaml::from_str::<HashMap<String, String>>(include_str!("extension_kinds.yaml"))
            .map(|raw| {
                raw.into_iter()
                    .filter_map(|(ext, kind)| {
                        let ext = ext.to_ascii_lowercase();
                        if ext.is_empty()
                            || ext.len() > 10
                            || !ext
                                .bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                        {
                            return None;
                        }
                        ContentKind::parse(&kind).map(|kind| (ext, kind))
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self {
            kinds: default_kinds().clone(),
        }
    }
}

impl ExtensionRegistry {
    /// Registry with admin overrides merged over the defaults.
    /// Override keys are lowercased and must pass the strict check;
    /// values must parse as a kind. Invalid entries are skipped
    /// (config validation rejects them at load time; this is the
    /// defensive second layer).
    pub(crate) fn with_overrides(&self, overrides: &HashMap<String, String>) -> Self {
        let mut kinds = self.kinds.clone();
        for (ext, kind) in overrides {
            let ext = ext.to_ascii_lowercase();
            if ext.is_empty()
                || ext.len() > 10
                || !ext
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            {
                continue;
            }
            if let Some(kind) = ContentKind::parse(kind) {
                kinds.insert(ext, kind);
            }
        }
        Self { kinds }
    }

    /// Known kind for an extension (already lowercased by the
    /// caller, or any case — lookup lowercases). `None` for an
    /// unknown extension.
    pub(crate) fn kind_for(&self, extension: &str) -> Option<ContentKind> {
        self.kinds.get(&extension.to_ascii_lowercase()).copied()
    }

    /// Storage kind for a file name, derived from its extension;
    /// unknown or missing extensions fall back to `Others`.
    pub(crate) fn kind_for_file(&self, file_name: &str) -> ContentKind {
        storage_extension(file_name)
            .and_then(|ext| self.kind_for(&ext))
            .unwrap_or(ContentKind::Others)
    }

    /// Resolve a file name to its (storage kind, validated
    /// extension): the kind is derived from the registry when the
    /// caller does not specify one explicitly, and the extension is
    /// the normalized suffix (or `None` when the name carries no
    /// usable extension).
    pub(crate) fn resolve(&self, file_name: &str) -> (ContentKind, Option<String>) {
        (self.kind_for_file(file_name), storage_extension(file_name))
    }

    /// Number of known extensions (for tests / diagnostics).
    pub(crate) fn len(&self) -> usize {
        self.kinds.len()
    }
}

/// Where a stored object belongs: one tenant's tree, or the
/// control-plane (main-site) tree. Constructed explicitly at the
/// call site — there is no `Default` and no fallthrough, so a
/// tenant upload can never silently land in the system tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageScope {
    /// Content owned by one tenant site, identified by its UUID.
    Tenant(Uuid),
    /// Control-plane (main-site) content. Only explicitly
    /// control-plane handlers may use this.
    System,
}

/// Which storage area an object lives in: the tenant/system
/// public area (served by `/files/public/` with zero checks) or
/// the private area (served by `/files/private/` behind the
/// 4-rule flow). The area is a provider-layer concept only: the
/// stored key is visibility-neutral (`<kind>/YYYY/MM/DD/<uuidv7>
/// [.<ext>]`), and the area is inserted between the scope root and
/// the key at runtime (`provider_key_for_area`). A visibility flip
/// MOVES the object between areas; the stored key never changes,
/// so the file URL never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageArea {
    Public,
    Private,
}

impl StorageArea {
    /// Area segment in a provider key.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    /// Parse an area from a URL segment or a `visibility` value.
    /// Anything else is rejected — fail closed.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            _ => None,
        }
    }

    /// The area a document's `visibility` value maps to.
    pub(crate) fn from_visibility(visibility: &str) -> Self {
        match visibility {
            "public" => Self::Public,
            _ => Self::Private,
        }
    }
}

impl StorageScope {
    /// Tenant id for a tenant scope, `None` for the system scope.
    pub(crate) fn tenant_id(&self) -> Option<Uuid> {
        match self {
            Self::Tenant(id) => Some(*id),
            Self::System => None,
        }
    }

    /// Provider-side root for this scope, resolved at runtime from
    /// the scope — NEVER part of the stored key. The stored key is
    /// scope-free (`<kind>/YYYY/MM/DD/<uuidv7>[.<ext>]`); the
    /// provider only ever sees the rooted (effective) key
    /// (`provider_key`), and the DB only ever stores the scope-free
    /// key. This is the ONE place that maps a scope to a physical
    /// root, so a later change to the tenancy-id logic changes this
    /// function alone and never requires re-keying objects.
    pub(crate) fn root(&self) -> String {
        match self {
            Self::Tenant(id) => format!("tenants/{id}"),
            Self::System => "system".to_string(),
        }
    }

    /// Effective provider key for a stored (scope-free) key: the
    /// scope root joined to the key. Pass THIS to the storage
    /// provider (`upload` / `download` / `delete`); store the
    /// scope-free key in the DB.
    ///
    /// This is the PRIVATE-area key (the default area: every upload
    /// lands private unless the caller asked for public). Callers
    /// that serve a document by its row MUST use
    /// `provider_key_for_area` with the row's visibility instead,
    /// or a flipped-public document would be looked up in the
    /// private area and missed.
    pub(crate) fn provider_key(&self, key: &str) -> String {
        self.provider_key_for_area(StorageArea::Private, key)
    }

    /// Effective provider key for a stored (scope-free) key in one
    /// area: `{root}/{area}/{key}`. The area is the visibility
    /// area of the document that owns the object; the stored key
    /// itself carries no area, so a flip only moves the object.
    pub(crate) fn provider_key_for_area(&self, area: StorageArea, key: &str) -> String {
        format!("{}/{}/{}", self.root(), area.as_str(), key)
    }

    /// Opaque scope token for signed download URLs: the tenant UUID
    /// for a tenant scope, the literal `system` for the system
    /// scope. The token is HMAC-signed inside the download token,
    /// so a caller cannot forge another scope at redemption time.
    pub(crate) fn token(&self) -> String {
        match self {
            Self::Tenant(id) => id.to_string(),
            Self::System => "system".to_string(),
        }
    }

    /// Parse a scope token produced by `token`. `None` for anything
    /// else (including a malformed UUID) — fail closed.
    pub(crate) fn from_token(value: &str) -> Option<Self> {
        if value == "system" {
            return Some(Self::System);
        }
        Uuid::parse_str(value).ok().map(Self::Tenant)
    }
}

/// Derive the storage file-name extension from an original file
/// name: the substring after the last dot, lowercased, kept only
/// when it matches `[a-z0-9]{1,10}`. Anything else (no dot, empty,
/// over-long, non-alphanumeric) yields `None` and the object is
/// stored under the bare SHA-256. The extension is a
/// fallback/browsability aid only (S3/CDN Content-Type inference
/// when metadata is not carried, human-readable keys in the
/// console); the source of truth for MIME stays the metadata stored
/// in the DB / set as object metadata at upload — never derive
/// Content-Type from the extension at serve time.
pub(crate) fn storage_extension(file_name: &str) -> Option<String> {
    let ext = file_name.rsplit('.').next()?;
    if ext.len() == file_name.len() {
        return None; // No dot at all.
    }
    let ext = ext.to_ascii_lowercase();
    if ext.is_empty()
        || ext.len() > 10
        || !ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return None;
    }
    Some(ext)
}

/// Build the validated storage key for one content object.
///
/// Layout (date is the UTC upload date):
/// `<kind>/YYYY/MM/DD/<uuidv7>[.<ext>]` — the SAME shape for tenant
/// and system content alike. The key carries NO scope and NO tenant
/// id: there is no `tenants/<tenancy-id>/` or `system/` prefix.
/// The tenant/system distinction lives in the provider layer
/// (`StorageScope::root`, resolved at runtime from the scope) and
/// in the DB metadata (`documents.scope`, `documents.company_id`),
/// never in the key — so a later change to the tenancy-id logic
/// never requires re-keying objects.
///
/// The file name is a FRESH UUIDv7 (`Uuid::now_v7()`, the tree-wide
/// helper) plus the validated original extension — the SAME scheme
/// for tenant and system content. A fresh name can never collide
/// (the same content uploaded twice is two distinct objects — the
/// "upload freely" semantics), UUIDv7 is time-ordered so listings
/// sort by creation, and the name is known at upload start, before
/// any byte is hashed. The SHA-256 is NOT part of the key: it is
/// still computed at finalize and stored in the DB metadata
/// (checksum column) as the integrity value. The checksum argument
/// is still validated (a 64-character lowercase hex digest) because
/// the caller always passes a real digest. A nil tenant id is
/// rejected, and the extension must match `[a-z0-9]{1,10}`
/// (lowercased), so no raw user input can smuggle a path segment
/// into the key. The key is validated with the core `validate_key`
/// before it is returned.
pub(crate) fn build_object_key(
    tenant: Option<Uuid>,
    kind: ContentKind,
    date: DateTime<Utc>,
    checksum: &str,
    extension: Option<&str>,
) -> UnifiedResult<String> {
    if checksum.len() != 64
        || !checksum
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(UnifiedError::bad_request(
            "Storage checksum must be a 64-character lowercase hex digest",
        ));
    }
    // Validate + normalize the extension once (shared by both
    // branches).
    let extension = match extension {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            if ext.is_empty()
                || ext.len() > 10
                || !ext
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            {
                return Err(UnifiedError::bad_request(
                    "Storage extension must match [a-z0-9]{1,10}",
                ));
            }
            Some(ext)
        }
        None => None,
    };
    // One fresh UUIDv7 per upload, for tenant and system alike.
    // The SHA-256 lives in the DB metadata, not in the key.
    let id = Uuid::now_v7();
    let file_name = match &extension {
        Some(ext) => format!("{id}.{ext}"),
        None => id.to_string(),
    };
    let date = date.format("%Y/%m/%d");
    // Scope-free key: the tenant/system distinction is NOT in the
    // key. The tenant argument is only validated here (a nil tenant
    // id is rejected); the provider layer re-adds the scope root
    // at runtime (`StorageScope::provider_key`).
    if tenant == Some(Uuid::nil()) {
        return Err(UnifiedError::forbidden(
            "Tenant id must not be the nil UUID",
        ));
    }
    let key = format!("{}/{date}/{file_name}", kind.as_str());
    validate_key(&key)?;
    Ok(key)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
    /// SHA-256 hex of the current version's bytes, or `None` while
    /// the lazy finalize has not hashed it yet (NULL checksum =
    /// "not verified yet"; serving is gated on the virus scan,
    /// not on the hash).
    pub checksum: Option<String>,
    /// Storage key of the current version's bytes.
    pub storage_key: String,
    pub current_version: u32,
    pub version_count: u32,
    pub scan_status: String,
    /// Threat label when `scan_status` is `infected`, else null.
    pub scan_threat: Option<String>,
    /// Visibility: `public` or `private` (default `private`). The
    /// source of truth for the `/files/private/` rule 1 check and
    /// for public listings; the stored key carries no visibility.
    pub visibility: String,
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
    /// SHA-256 hex of this version's bytes, or `None` while the
    /// lazy finalize has not hashed it yet.
    pub checksum: Option<String>,
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
    /// Visibility the completed document lands with (`public` or
    /// `private`, default `private`); captured at initiation so a
    /// later change never moves an in-flight session's file.
    pub visibility: String,
    /// Resolved tenant id from `require_tenant` at initiation
    /// (the scope the object is stored under). The lazy-finalize
    /// worker needs it to locate the object; `None` only for
    /// sessions created before this field existed, and a job for
    /// such a session fails closed.
    pub tenant_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Spool directory this session's parts live in, captured at
    /// creation (so a later settings change never moves an
    /// in-flight session's parts). Server-side only, never
    /// serialized to API responses.
    #[serde(skip)]
    pub spool_dir: Option<PathBuf>,
}

/// One lazy-finalize job: the durable record that a received
/// upload still needs its whole-file SHA-256 computed and verified
/// against the client-supplied `expected_sha256`. The row is the
/// unit the worker claims, retries and reports on; the session row
/// alone cannot carry that (no per-job status, attempts or error).
///
/// Runtime authority is the in-memory `finalize_jobs` DashMap
/// (this module writes no Diesel rows yet — see the module header
/// for the C11 limitation); the table is the persistence target
/// and the schema of the future write-through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizeJob {
    pub id: String,
    pub session_id: String,
    /// Document the session completes into, when known (a new
    /// document gets its id only at finalize time).
    pub document_id: Option<String>,
    /// Resolved tenant id the object is stored under (copied from
    /// the session at job creation); `None` fails the job closed.
    pub tenant_id: Option<Uuid>,
    /// `pending` | `finalizing` | `verified` | `failed`.
    pub status: String,
    /// Claim attempts so far (each stale-claim re-enqueue bumps it).
    pub attempts: u32,
    /// Last finalize error, when `status` is `failed`.
    pub last_error: Option<String>,
    /// When the current claim started; a `finalizing` job older
    /// than the stale-claim threshold is re-enqueued.
    pub claimed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One received chunk part, spooled to a file under the session's
/// spool directory; only metadata lives in memory, so a
/// multi-gigabyte upload never holds its parts in RAM.
#[derive(Debug, Clone)]
pub struct UploadPart {
    pub index: u32,
    pub size: u64,
    pub sha256: String,
    /// Spool file holding this part's bytes
    /// (`<storage>/temps/uploads/YYYY/MM/DD/<session-id>/<index:06>.part`).
    pub path: PathBuf,
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
    /// Temp files directory for chunked-upload part spool files.
    /// `None` (default) means `<storage>/temps/uploads` under
    /// the local storage root; set a path to override it. Applies
    /// to sessions created after the change; in-flight sessions
    /// keep the directory they started in.
    pub spool_dir: Option<String>,
    /// Maximum chunk data held in RAM in bytes (100 MiB..=8 GiB,
    /// default 100 MiB). Bounds the in-RAM buffering of the
    /// chunked-upload path: one part while it is read, the
    /// non-local object-store fallback, and in-memory virus
    /// scanning. Parts always spill to disk; the cap is the safety
    /// ceiling.
    pub max_chunk_ram_bytes: u64,
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            allow_unscanned_downloads: false,
            default_chunk_size: DEFAULT_CHUNK_SIZE,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            max_upload_bytes: MAX_UPLOAD_BYTES,
            spool_dir: None,
            max_chunk_ram_bytes: DocumentsConfig::MAX_CHUNK_RAM_DEFAULT,
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
    /// SHA-256 hex, or `None` while the lazy finalize has not
    /// hashed the file yet.
    pub checksum: Option<String>,
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
    /// Requested visibility for the completed document (`public`
    /// or `private`); absent means `private`. The scan gate may
    /// still force `private` when the scan is not clean.
    #[serde(default)]
    pub visibility: Option<String>,
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

/// Body of `POST /api/v1/documents/{id}/flip`.
#[derive(Debug, Clone, Deserialize)]
pub struct FlipVisibilityRequest {
    pub visibility: String,
}

/// One entry of the shared media picker: a public document with
/// the URL an editor attaches (`/files/public/<key>`).
#[derive(Debug, Clone, Serialize)]
pub struct PickerItem {
    pub document_id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub storage_key: String,
    pub url: String,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
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
    /// Temp files directory override; empty string clears it back
    /// to the default.
    #[serde(default)]
    pub spool_dir: Option<String>,
    #[serde(default)]
    pub max_chunk_ram_bytes: Option<u64>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_date() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 23, 10, 30, 0).unwrap()
    }

    fn test_checksum() -> String {
        "ab".repeat(32)
    }

    fn test_tenant() -> Uuid {
        Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap()
    }

    /// Assert the last path segment of `key` is a UUIDv7 file name
    /// (optionally with `suffix`), i.e. the stem parses as a UUID
    /// whose version is 7.
    fn assert_uuidv7_file_name(key: &str, suffix: &str) {
        let last = key.rsplit('/').next().unwrap();
        assert!(
            last.ends_with(suffix),
            "key {key} must end with {suffix}"
        );
        let stem = &last[..last.len() - suffix.len()];
        let id = Uuid::parse_str(stem).expect("file name stem must be a UUID");
        assert_eq!(id.get_version(), Some(uuid::Version::SortRand));
    }

    #[test]
    fn object_key_tenant_path() {
        let first = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        // Scope-free: no `tenants/<id>/` prefix in the key.
        assert!(first.starts_with("documents/2026/11/23/"));
        assert!(
            !first.contains("tenants/"),
            "tenant key must carry no tenant segment: {first}"
        );
        assert_uuidv7_file_name(&first, ".pdf");
        // A second upload of identical bytes mints a different key
        // (one fresh UUIDv7 per upload).
        let second = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn object_key_system_path() {
        let first = build_object_key(
            None,
            ContentKind::Media,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        // Scope-free: no `system/` prefix in the key either.
        assert!(first.starts_with("media/2026/11/23/"));
        assert!(
            !first.contains("system/"),
            "system key must carry no scope segment: {first}"
        );
        assert_uuidv7_file_name(&first, ".pdf");
        // A second upload of identical bytes mints a different key.
        let second = build_object_key(
            None,
            ContentKind::Media,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        assert_ne!(first, second);
        // Bare (no extension) system keys carry a bare UUIDv7.
        let bare =
            build_object_key(None, ContentKind::Media, test_date(), &test_checksum(), None)
                .unwrap();
        assert_uuidv7_file_name(&bare, "");
    }

    #[test]
    fn object_key_tenant_and_system_share_one_shape() {
        // Tenant and system keys are identical in shape: the key
        // alone never says which scope an object belongs to. The
        // distinction lives in the provider layer (`StorageScope`)
        // and the DB metadata.
        for (kind, segment) in [
            (ContentKind::Documents, "documents"),
            (ContentKind::Media, "media"),
            (ContentKind::Others, "others"),
        ] {
            let tenant_key = build_object_key(
                Some(test_tenant()),
                kind,
                test_date(),
                &test_checksum(),
                Some("pdf"),
            )
            .unwrap();
            let system_key =
                build_object_key(None, kind, test_date(), &test_checksum(), Some("pdf")).unwrap();
            for key in [&tenant_key, &system_key] {
                assert!(
                    key.starts_with(&format!("{segment}/2026/11/23/")),
                    "key for {segment} must start with its kind segment: {key}"
                );
                assert!(
                    !key.contains("tenants/") && !key.contains("system/"),
                    "key must carry no scope or tenant segment: {key}"
                );
            }
            // Same shape, different provider roots.
            assert_ne!(
                StorageScope::Tenant(test_tenant()).provider_key(&tenant_key),
                StorageScope::System.provider_key(&system_key)
            );
        }
    }

    #[test]
    fn scope_provider_key_resolves_roots() {
        let scope = StorageScope::Tenant(test_tenant());
        assert_eq!(scope.root(), "tenants/019a1234-5678-7abc-def0-123456789abc");
        assert_eq!(StorageScope::System.root(), "system");
        let key = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        let provider_key = scope.provider_key(&key);
        assert_eq!(
            provider_key,
            format!("tenants/019a1234-5678-7abc-def0-123456789abc/private/{key}")
        );
        assert_eq!(
            StorageScope::System.provider_key(&key),
            format!("system/private/{key}")
        );
        // The rooted (effective) key is still a valid storage key.
        validate_key(&provider_key).unwrap();
        validate_key(&StorageScope::System.provider_key(&key)).unwrap();
        // Scope tokens round-trip through the signed-URL encoding.
        assert_eq!(StorageScope::from_token(&scope.token()), Some(scope));
        assert_eq!(
            StorageScope::from_token(&StorageScope::System.token()),
            Some(StorageScope::System)
        );
        assert_eq!(StorageScope::from_token("not-a-uuid"), None);
        assert_eq!(StorageScope::from_token(""), None);
    }

    #[test]
    fn provider_key_for_area_places_the_area_between_root_and_key() {
        let scope = StorageScope::Tenant(test_tenant());
        let key = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        assert_eq!(
            scope.provider_key_for_area(StorageArea::Public, &key),
            format!("tenants/019a1234-5678-7abc-def0-123456789abc/public/{key}")
        );
        assert_eq!(
            scope.provider_key_for_area(StorageArea::Private, &key),
            format!("tenants/019a1234-5678-7abc-def0-123456789abc/private/{key}")
        );
        // `provider_key` is the private-area key (the default).
        assert_eq!(scope.provider_key(&key), scope.provider_key_for_area(StorageArea::Private, &key));
        // The stored key is the same in both areas: only the
        // provider key differs, so a flip moves the object without
        // touching the stored key (and the URL never changes).
        assert_eq!(
            scope.provider_key_for_area(StorageArea::Public, &key)
                .strip_prefix("tenants/019a1234-5678-7abc-def0-123456789abc/public/"),
            Some(key.as_str())
        );
        validate_key(&scope.provider_key_for_area(StorageArea::Public, &key)).unwrap();
        validate_key(&scope.provider_key_for_area(StorageArea::Private, &key)).unwrap();
    }

    #[test]
    fn storage_area_parses_only_known_values() {
        assert_eq!(StorageArea::from_str("public"), Some(StorageArea::Public));
        assert_eq!(StorageArea::from_str("private"), Some(StorageArea::Private));
        assert_eq!(StorageArea::from_str("unlisted"), None);
        assert_eq!(StorageArea::from_str(""), None);
        assert_eq!(StorageArea::from_str("PUBLIC"), None);
        // Visibility values map to areas; anything else is private
        // (fail closed — never treated as public).
        assert_eq!(StorageArea::from_visibility("public"), StorageArea::Public);
        assert_eq!(StorageArea::from_visibility("private"), StorageArea::Private);
        assert_eq!(StorageArea::from_visibility("unlisted"), StorageArea::Private);
        assert_eq!(StorageArea::from_visibility(""), StorageArea::Private);
    }

    #[test]
    fn object_key_covers_all_kinds() {
        for (kind, segment) in [
            (ContentKind::Documents, "documents"),
            (ContentKind::Media, "media"),
            (ContentKind::Others, "others"),
        ] {
            assert_eq!(kind.as_str(), segment);
            let key = build_object_key(
                Some(test_tenant()),
                kind,
                test_date(),
                &test_checksum(),
                None,
            )
            .unwrap();
            assert!(
                key.starts_with(&format!("{segment}/2026/11/23/")),
                "key for {segment} must start with its kind segment: {key}"
            );
            let key = build_object_key(None, kind, test_date(), &test_checksum(), None).unwrap();
            assert!(
                key.starts_with(&format!("{segment}/2026/11/23/")),
                "system key for {segment} must start with its kind: {key}"
            );
        }
    }

    #[test]
    fn object_key_date_uses_utc_upload_date() {
        let new_year = Utc.with_ymd_and_hms(2027, 1, 2, 0, 5, 0).unwrap();
        let key = build_object_key(
            Some(test_tenant()),
            ContentKind::Others,
            new_year,
            &test_checksum(),
            None,
        )
        .unwrap();
        assert!(
            key.contains("/others/2027/01/02/"),
            "date must format as YYYY/MM/DD: {key}"
        );
    }

    #[test]
    fn object_key_rejects_nil_tenant_and_bad_checksums() {
        assert!(build_object_key(
            Some(Uuid::nil()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            None
        )
        .is_err());
        for bad in ["", "zz".repeat(32).as_str(), "ab".repeat(31).as_str()] {
            assert!(
                build_object_key(
                    Some(test_tenant()),
                    ContentKind::Documents,
                    test_date(),
                    bad,
                    None
                )
                .is_err(),
                "checksum {bad:?} must be rejected"
            );
        }
        // Uppercase hex is not a canonical digest.
        assert!(build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &"AB".repeat(32),
            None
        )
        .is_err());
        // Extensions must match [a-z0-9]{1,10}.
        for bad_ext in ["", "toolongextension", "b-c", "a b"] {
            assert!(
                build_object_key(
                    Some(test_tenant()),
                    ContentKind::Documents,
                    test_date(),
                    &test_checksum(),
                    Some(bad_ext)
                )
                .is_err(),
                "extension {bad_ext:?} must be rejected"
            );
        }
    }

    #[test]
    fn object_key_extension_rules() {
        // Every upload mints a fresh UUIDv7, so the same content
        // with a different extension is a different key (a separate
        // copy) — intended, consistent with no dedup.
        let pdf = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("pdf"),
        )
        .unwrap();
        let txt = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("txt"),
        )
        .unwrap();
        let bare = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            None,
        )
        .unwrap();
        assert_ne!(pdf, txt);
        assert_ne!(pdf, bare);
        assert_uuidv7_file_name(&pdf, ".pdf");
        assert_uuidv7_file_name(&txt, ".txt");
        assert_uuidv7_file_name(&bare, "");
        // The builder lowercases the extension itself (and still
        // mints a fresh name, so it differs from `pdf`).
        let upper = build_object_key(
            Some(test_tenant()),
            ContentKind::Documents,
            test_date(),
            &test_checksum(),
            Some("PDF"),
        )
        .unwrap();
        assert_uuidv7_file_name(&upper, ".pdf");
        assert_ne!(upper, pdf);
    }

    #[test]
    fn storage_extension_cases() {
        assert_eq!(storage_extension("report.pdf").as_deref(), Some("pdf"));
        assert_eq!(storage_extension("PHOTO.JPG").as_deref(), Some("jpg"));
        assert_eq!(
            storage_extension("archive.tar.gz").as_deref(),
            Some("gz")
        );
        assert_eq!(storage_extension("noext"), None);
        assert_eq!(storage_extension("trailing."), None);
        assert_eq!(storage_extension("a.b-c"), None);
        assert_eq!(storage_extension("toolong.abcdefghijk"), None);
        assert_eq!(storage_extension("x.123").as_deref(), Some("123"));
        assert_eq!(storage_extension("x.1234567890").as_deref(), Some("1234567890"));
    }

    #[test]
    fn extension_registry_default_parses() {
        let registry = ExtensionRegistry::default();
        assert!(
            registry.len() >= 50,
            "default registry must hold the shipped list, got {}",
            registry.len()
        );
        assert_eq!(registry.kind_for("pdf"), Some(ContentKind::Documents));
        assert_eq!(registry.kind_for("docx"), Some(ContentKind::Documents));
        assert_eq!(registry.kind_for("JPG"), Some(ContentKind::Media));
        assert_eq!(registry.kind_for("mp4"), Some(ContentKind::Media));
        assert_eq!(registry.kind_for("zip"), Some(ContentKind::Others));
        assert_eq!(registry.kind_for("not-a-real-ext"), None);
    }

    #[test]
    fn extension_registry_resolves_files() {
        let registry = ExtensionRegistry::default();
        assert_eq!(
            registry.resolve("report.pdf"),
            (ContentKind::Documents, Some("pdf".to_string()))
        );
        assert_eq!(
            registry.resolve("PHOTO.JPG"),
            (ContentKind::Media, Some("jpg".to_string()))
        );
        // Unknown extension: kind `others`, suffix kept when it
        // passes the strict check.
        assert_eq!(
            registry.resolve("data.xyz123"),
            (ContentKind::Others, Some("xyz123".to_string()))
        );
        // No usable extension: kind `others`, no suffix.
        assert_eq!(registry.resolve("README"), (ContentKind::Others, None));
        assert_eq!(registry.resolve("trailing."), (ContentKind::Others, None));
    }

    #[test]
    fn extension_registry_overrides() {
        let registry = ExtensionRegistry::default();
        let mut overrides = HashMap::new();
        overrides.insert("svg".to_string(), "documents".to_string());
        overrides.insert("newext".to_string(), "media".to_string());
        overrides.insert("bad key!".to_string(), "media".to_string());
        overrides.insert("pdf".to_string(), "not-a-kind".to_string());
        let registry = registry.with_overrides(&overrides);
        assert_eq!(registry.kind_for("svg"), Some(ContentKind::Documents));
        assert_eq!(registry.kind_for("newext"), Some(ContentKind::Media));
        assert_eq!(registry.kind_for("bad key!"), None);
        assert_eq!(registry.kind_for("pdf"), Some(ContentKind::Documents));
    }

    #[test]
    fn storage_scope_tenant_id() {
        assert_eq!(
            StorageScope::Tenant(test_tenant()).tenant_id(),
            Some(test_tenant())
        );
        assert_eq!(StorageScope::System.tenant_id(), None);
    }
}
