// documents — In-memory state and its methods.

use super::*;
use unified_core::config::DocumentsConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::storage::{
    LocalProvider, StorageKind, StorageProvider, sha256_hex, validate_key,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, RwLock};

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod scanning;
pub mod storage;
pub mod access;
pub mod uploads;
pub mod sessions;
pub mod finalize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct DocumentsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) documents: DashMap<String, DocumentRecord>,
    pub(crate) versions: DashMap<String, DocumentVersion>,
    pub(crate) sessions: DashMap<String, UploadSession>,
    /// Parts keyed by session id, then part index.
    pub(crate) parts: DashMap<String, DashMap<u32, UploadPart>>,
    /// Lazy-finalize jobs keyed by job id. The row is the unit the
    /// worker claims, retries and reports on (see `FinalizeJob` in
    /// types.rs); the in-memory map is the runtime authority —
    /// the `finalize_jobs` DB table is the persistence target for
    /// the future C11 write-through.
    pub(crate) finalize_jobs: DashMap<String, FinalizeJob>,
    /// Whether the finalize worker was started (guarded by an
    /// atomic swap so `start_finalize_worker` is idempotent).
    pub(crate) finalize_worker_started: AtomicBool,
    /// Hash jobs currently running (bounded by
    /// `FINALIZE_MAX_CONCURRENT`).
    pub(crate) finalize_running: AtomicUsize,
    pub(crate) allowlist: DashMap<String, AllowlistRule>,
    pub(crate) downloads: DashMap<String, DownloadRecord>,
    /// Local quotas keyed by `"<scope>:<scope_id>"`.
    pub(crate) quotas: DashMap<String, DocumentQuota>,
    /// Registered virus scanners, keyed by scanner name.
    pub(crate) scanners: DashMap<String, Arc<dyn DocumentScanner>>,
    /// One mutex per document id, serializing the version-number
    /// allocation + version insert + document update of a re-upload
    /// (see `finalize_upload` and `restore_version`). The guard is
    /// only ever held across synchronous map work, never across an
    /// await, so it cannot block the async executor.
    pub(crate) document_locks: DashMap<String, Arc<Mutex<()>>>,
    pub(crate) settings: RwLock<DocumentSettings>,
    /// Extension -> content-kind registry (data-driven: shipped
    /// defaults in `extension_kinds.yaml` plus admin overrides
    /// from `documents.extensions` in config.yaml). Used to
    /// validate + normalize extensions and to derive the storage
    /// kind when the caller does not specify one explicitly.
    pub(crate) extensions: RwLock<ExtensionRegistry>,
    /// Per-process random HMAC key for signed download URLs.
    /// Generated at startup, never persisted or logged.
    signing_key: RwLock<[u8; 32]>,
    pub(crate) provider: Option<Arc<dyn StorageProvider>>,
    /// Canonical local storage root (`data/documents`), captured
    /// from the local provider at construction. Chunked-upload part
    /// spool files and streamed object writes live under it; it is
    /// never `/tmp`.
    pub(crate) storage_dir: PathBuf,
}

impl DocumentsState {
    pub fn new() -> Self {
        let local = LocalProvider::new(PathBuf::from("data/documents")).ok();
        // The spool root is the provider's own canonical base
        // directory, so part files and streamed objects live under
        // the configured storage root, never `/tmp`.
        let storage_dir = local
            .as_ref()
            .map(|provider| provider.base_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("data/documents"));
        let provider = local.map(|provider| Arc::new(provider) as Arc<dyn StorageProvider>);
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
            sessions: DashMap::new(),
            parts: DashMap::new(),
            finalize_jobs: DashMap::new(),
            finalize_worker_started: AtomicBool::new(false),
            finalize_running: AtomicUsize::new(0),
            allowlist: DashMap::new(),
            downloads: DashMap::new(),
            quotas: DashMap::new(),
            scanners: DashMap::new(),
            document_locks: DashMap::new(),
            settings: RwLock::new(DocumentSettings::default()),
            extensions: RwLock::new(ExtensionRegistry::default()),
            signing_key: RwLock::new(key),
            provider,
            storage_dir,
        };
        state.seed_default_allowlist();
        state
    }

    /// Seed the default allowlist rule for this module's own files.
    /// Other modules have no rule until an admin creates one, and
    /// uploads for a module without a rule are rejected (fail
    /// closed). The default set is common office / image / archive
    /// types; nothing executable is ever allowed.
    fn seed_default_allowlist(&self) {
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

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn settings(&self) -> DocumentSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    pub(crate) fn extensions(&self) -> ExtensionRegistry {
        self.extensions
            .read()
            .expect("extensions lock poisoned")
            .clone()
    }

    pub(crate) fn provider(&self) -> UnifiedResult<Arc<dyn StorageProvider>> {
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

    /// The mutex serializing version allocation for `document_id`.
    /// Created lazily; two callers asking for the same id get the
    /// same mutex, so holding its guard makes the read of
    /// `current_version`, the +1, and the version/document inserts
    /// atomic against concurrent re-uploads of that document.
    pub(crate) fn document_lock(&self, document_id: &str) -> Arc<Mutex<()>> {
        self.document_locks
            .entry(document_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// True when `owner_id` owns at least one document in
    /// `company_id` (local membership evidence for the quota-usage
    /// company check).
    pub(crate) fn owns_document_in_company(&self, owner_id: &str, company_id: &str) -> bool {
        self.documents.iter().any(|entry| {
            entry.value().owner_id == owner_id && entry.value().company_id == company_id
        })
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
}

/// Read at most `limit` bytes from the head of `path` (enough for
/// magic-byte MIME sniffing).
fn read_file_head(path: &Path, limit: usize) -> UnifiedResult<Vec<u8>> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| UnifiedError::internal(format!("Cannot open file: {error}")))?;
    let mut head = vec![0u8; limit];
    let mut filled = 0;
    while filled < limit {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(UnifiedError::internal(format!("Cannot read file: {error}")));
            }
        }
    }
    head.truncate(filled);
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hardening::{HardeningState, SecuritySettings};
    use unified_core::hooks::HookContext;
    use chrono::{Duration, TimeZone};

    struct FixedScanner {
        name: String,
        verdict: ScanVerdict,
    }

    impl DocumentScanner for FixedScanner {
        fn name(&self) -> &str {
            &self.name
        }

        fn scan(&self, _bytes: &[u8]) -> ScanVerdict {
            self.verdict.clone()
        }
    }

    fn test_record(scan_status: &str) -> DocumentRecord {
        DocumentRecord {
            id: "doc-1".to_string(),
            name: "report.pdf".to_string(),
            module_id: "documents".to_string(),
            company_id: "company-1".to_string(),
            owner_id: "user-1".to_string(),
            scope: "owner".to_string(),
            mime: "application/pdf".to_string(),
            size: 100,
            checksum: Some("aa".repeat(32)),
            storage_key: format!("documents/2026/11/23/{}", "aa".repeat(32)),
            current_version: 1,
            version_count: 1,
            scan_status: scan_status.to_string(),
            scan_threat: None,
            visibility: "private".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn test_session(id: &str, status: &str) -> UploadSession {
        UploadSession {
            id: id.to_string(),
            document_id: None,
            name: "report.pdf".to_string(),
            module_id: "documents".to_string(),
            company_id: String::new(),
            owner_id: "user-1".to_string(),
            total_size: 10,
            expected_sha256: "aa".repeat(32),
            chunk_size: 1024 * 1024,
            part_count: 1,
            received_parts: Vec::new(),
            missing_parts: vec![0],
            status: status.to_string(),
            visibility: "private".to_string(),
            tenant_id: Some(Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap()),
            expires_at: now() + Duration::hours(1),
            created_at: now(),
            updated_at: now(),
            spool_dir: None,
        }
    }

    fn test_version(document_id: &str, version: u32, size: u64, checksum: &str) -> DocumentVersion {
        DocumentVersion {
            id: format!("ver-{version}"),
            document_id: document_id.to_string(),
            version,
            checksum: Some(checksum.to_string()),
            storage_key: format!("documents/2026/11/23/{checksum}"),
            size,
            mime: "application/pdf".to_string(),
            uploader_id: "user-1".to_string(),
            scan_status: "clean".to_string(),
            scan_threat: None,
            created_at: now(),
        }
    }

    #[test]
    fn default_upload_prefixes_cover_documents_uploads() {
        let settings = SecuritySettings::default();
        assert!(
            settings
                .upload_path_prefixes
                .iter()
                .any(|prefix| prefix == "/api/v1/documents"),
            "documents uploads must use the upload body limit"
        );
        assert!(
            settings
                .upload_path_prefixes
                .iter()
                .any(|prefix| prefix == "/api/v1/timetracker/agent"),
            "agent uploads must use the upload body limit"
        );
        let state = HardeningState::default();
        let (limit, kind) =
            state.body_limit_for_path("/api/v1/documents/upload", &settings);
        assert_eq!(kind, "upload");
        assert_eq!(limit, 100 * 1024 * 1024);
        let (default_limit, default_kind) =
            state.body_limit_for_path("/api/v1/documents", &settings);
        assert_eq!(default_kind, "upload");
        assert_eq!(default_limit, 100 * 1024 * 1024);
        let (other_limit, other_kind) =
            state.body_limit_for_path("/api/v1/other", &settings);
        assert_eq!(other_kind, "default");
        assert_eq!(other_limit, 1024 * 1024);
    }

    #[test]
    fn document_lock_serializes_concurrent_version_allocation() {
        let state = Arc::new(DocumentsState::new());
        let counter = Arc::new(Mutex::new(0u32));
        let mut observed = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let state = Arc::clone(&state);
                let counter = Arc::clone(&counter);
                handles.push(scope.spawn(move || {
                    // The exact read-+1-write sequence
                    // `finalize_upload` performs under the lock.
                    let lock = state.document_lock("doc-1");
                    let _guard = lock.lock().expect("lock poisoned");
                    let mut value = counter.lock().expect("lock poisoned");
                    *value += 1;
                    *value
                }));
            }
            for handle in handles {
                observed.push(handle.join().expect("thread panicked"));
            }
        });
        observed.sort_unstable();
        assert_eq!(observed, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(*counter.lock().expect("lock poisoned"), 8);
    }

    #[tokio::test]
    async fn delete_document_refunds_sum_of_all_version_sizes() {
        let mut state = DocumentsState::new();
        // No real storage in tests: releases must not touch disk.
        state.provider = None;
        let mut document = test_record("clean");
        document.size = 250;
        document.current_version = 2;
        document.version_count = 2;
        state.documents.insert(document.id.clone(), document);
        state.versions.insert(
            "ver-1".to_string(),
            test_version("doc-1", 1, 100, &"aa".repeat(32)),
        );
        state.versions.insert(
            "ver-2".to_string(),
            test_version("doc-1", 2, 250, &"bb".repeat(32)),
        );
        for (scope, scope_id) in [("company", "company-1"), ("user", "user-1")] {
            state.quotas.insert(
                format!("{scope}:{scope_id}"),
                DocumentQuota {
                    id: format!("quota-{scope}"),
                    scope: scope.to_string(),
                    scope_id: scope_id.to_string(),
                    bytes_limit: Some(1_000_000),
                    used_bytes: 350,
                    created_at: now(),
                    updated_at: now(),
                },
            );
        }
        let deleted = state
            .delete_document(
                StorageScope::Tenant(
                    Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap(),
                ),
                "doc-1",
            )
            .await
            .unwrap();
        assert_eq!(deleted.id, "doc-1");
        assert!(state.documents.get("doc-1").is_none());
        assert!(state.versions.is_empty());
        // 100 + 250 refunded, not just the current version's 250.
        assert_eq!(
            state
                .quotas
                .get("company:company-1")
                .map(|entry| entry.used_bytes),
            Some(0)
        );
        assert_eq!(
            state
                .quotas
                .get("user:user-1")
                .map(|entry| entry.used_bytes),
            Some(0)
        );
    }

    #[test]
    fn scanners_run_in_sorted_order_and_report_real_names() {
        let state = DocumentsState::new();
        // Registered out of order on purpose: the alphabetically
        // first infected scanner must win, deterministically.
        state.register_scanner(Arc::new(FixedScanner {
            name: "zebra".to_string(),
            verdict: ScanVerdict::Infected {
                threat: "z-threat".to_string(),
            },
        }));
        state.register_scanner(Arc::new(FixedScanner {
            name: "alpha".to_string(),
            verdict: ScanVerdict::Infected {
                threat: "a-threat".to_string(),
            },
        }));
        let (status, threat, scanner) = state.scan_document_bytes("doc-1", 1, b"bytes");
        assert_eq!(status, ScanStatus::Infected);
        assert_eq!(threat.as_deref(), Some("a-threat"));
        assert_eq!(scanner.as_deref(), Some("alpha"));

        let clean_state = DocumentsState::new();
        clean_state.register_scanner(Arc::new(FixedScanner {
            name: "beta".to_string(),
            verdict: ScanVerdict::Clean,
        }));
        clean_state.register_scanner(Arc::new(FixedScanner {
            name: "alpha".to_string(),
            verdict: ScanVerdict::Clean,
        }));
        let (status, threat, scanner) =
            clean_state.scan_document_bytes("doc-1", 1, b"bytes");
        assert_eq!(status, ScanStatus::Clean);
        assert_eq!(threat, None);
        assert_eq!(scanner.as_deref(), Some("alpha,beta"));

        let error_state = DocumentsState::new();
        error_state.register_scanner(Arc::new(FixedScanner {
            name: "zeta".to_string(),
            verdict: ScanVerdict::Error {
                reason: "boom".to_string(),
            },
        }));
        error_state.register_scanner(Arc::new(FixedScanner {
            name: "alpha".to_string(),
            verdict: ScanVerdict::Clean,
        }));
        let (status, threat, scanner) =
            error_state.scan_document_bytes("doc-1", 1, b"bytes");
        assert_eq!(status, ScanStatus::Error);
        assert_eq!(threat.as_deref(), Some("boom"));
        assert_eq!(scanner.as_deref(), Some("zeta"));

        let empty_state = DocumentsState::new();
        let (status, threat, scanner) =
            empty_state.scan_document_bytes("doc-1", 1, b"bytes");
        assert_eq!(status, ScanStatus::Skipped);
        assert_eq!(threat, None);
        assert_eq!(scanner, None);
    }

    #[test]
    fn download_gate_blocks_everything_but_clean() {
        let state = DocumentsState::new();
        assert!(state.download_gate(&test_record("clean")).is_ok());
        assert!(state.download_gate(&test_record("infected")).is_err());
        assert!(state.download_gate(&test_record("pending")).is_err());
        assert!(state.download_gate(&test_record("error")).is_err());
        // Skipped is blocked by default (fail closed).
        assert!(state.download_gate(&test_record("skipped")).is_err());
        let mut settings = state.settings();
        settings.allow_unscanned_downloads = true;
        *state.settings.write().expect("settings lock poisoned") = settings;
        assert!(state.download_gate(&test_record("skipped")).is_ok());
        // The opt-in only opens `skipped`, never the other states.
        assert!(state.download_gate(&test_record("infected")).is_err());
    }

    #[test]
    fn quota_check_takes_most_restrictive_hook_answer() {
        let state = DocumentsState::new();
        state.bus().register(
            "storage.object.upload_requested",
            "test-ok",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "resolved": true }));
            }),
        );
        state.bus().register(
            "storage.object.upload_requested",
            "test-full",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(
                    json!({ "quota_exceeded": true, "error": "storage is full" }),
                );
            }),
        );
        // A resolving answer first must not hide the exceeded one.
        let error = state
            .check_storage_quota("user-1", "company-1", 10, "application/pdf")
            .unwrap_err();
        assert_eq!(error.code, 409);
        assert!(error.message.contains("storage is full"));

        // Exceeded first, resolving second: still rejected.
        let reversed = DocumentsState::new();
        reversed.bus().register(
            "storage.object.upload_requested",
            "test-full",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "quota_exceeded": true }));
            }),
        );
        reversed.bus().register(
            "storage.object.upload_requested",
            "test-ok",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "resolved": true }));
            }),
        );
        assert!(reversed
            .check_storage_quota("user-1", "company-1", 10, "application/pdf")
            .is_err());

        // All resolved, none exceeded: allowed.
        let ok_state = DocumentsState::new();
        ok_state.bus().register(
            "storage.object.upload_requested",
            "test-ok",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "resolved": true }));
            }),
        );
        assert!(ok_state
            .check_storage_quota("user-1", "company-1", 10, "application/pdf")
            .is_ok());

        // No handler at all: falls through to the local quotas
        // (no rows here, so unlimited locally).
        let silent = DocumentsState::new();
        assert!(silent
            .check_storage_quota("user-1", "company-1", 10, "application/pdf")
            .is_ok());
    }

    #[test]
    fn session_completion_is_compare_and_swap() {
        let state = DocumentsState::new();
        state
            .sessions
            .insert("sess-1".to_string(), test_session("sess-1", "active"));
        assert!(state.try_begin_completion("sess-1").is_ok());
        assert_eq!(
            state
                .sessions
                .get("sess-1")
                .map(|entry| entry.status.clone()),
            Some("completing".to_string())
        );
        // A second concurrent complete is a conflict, not a second
        // finalize.
        let error = state.try_begin_completion("sess-1").unwrap_err();
        assert_eq!(error.code, 409);
        // Failure releases the claim so the client can retry.
        state.finish_completion("sess-1", false);
        assert_eq!(
            state
                .sessions
                .get("sess-1")
                .map(|entry| entry.status.clone()),
            Some("active".to_string())
        );
        assert!(state.try_begin_completion("sess-1").is_ok());
        state.finish_completion("sess-1", true);
        assert_eq!(
            state
                .sessions
                .get("sess-1")
                .map(|entry| entry.status.clone()),
            Some("completed".to_string())
        );
        assert!(state.try_begin_completion("sess-1").is_err());
        assert!(state.try_begin_completion("missing").is_err());
    }

    #[test]
    fn abort_rejects_completed_sessions() {
        let state = DocumentsState::new();
        state
            .sessions
            .insert("done".to_string(), test_session("done", "completed"));
        state
            .sessions
            .insert("busy".to_string(), test_session("busy", "completing"));
        state
            .sessions
            .insert("live".to_string(), test_session("live", "active"));
        state.parts.insert("live".to_string(), DashMap::new());
        let error = state.remove_session_if_abortable("done").unwrap_err();
        assert_eq!(error.code, 409);
        assert!(state.sessions.get("done").is_some());
        let error = state.remove_session_if_abortable("busy").unwrap_err();
        assert_eq!(error.code, 409);
        assert!(state.sessions.get("busy").is_some());
        let removed = state.remove_session_if_abortable("live").unwrap();
        assert_eq!(removed.id, "live");
        assert!(state.sessions.get("live").is_none());
        assert!(state.parts.get("live").is_none());
        assert!(state.remove_session_if_abortable("missing").is_err());
    }

    #[test]
    fn owns_document_in_company_matches_owner_and_company() {
        let state = DocumentsState::new();
        state
            .documents
            .insert("doc-1".to_string(), test_record("clean"));
        assert!(state.owns_document_in_company("user-1", "company-1"));
        assert!(!state.owns_document_in_company("user-1", "company-2"));
        assert!(!state.owns_document_in_company("user-2", "company-1"));
    }
    #[test]
    fn sign_token_is_deterministic_64_hex() {
        let state = DocumentsState::new();
        let first = state.sign_token("doc-1", "system", 1_700_000_000);
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(state.sign_token("doc-1", "system", 1_700_000_000), first);
        assert_ne!(state.sign_token("doc-2", "system", 1_700_000_000), first);
        // The scope is part of the signed payload: the same
        // document + expiry signs differently per scope.
        assert_ne!(
            state.sign_token(
                "doc-1",
                "019a1234-5678-7abc-def0-123456789abc",
                1_700_000_000
            ),
            first
        );
    }

    #[test]
    fn signed_token_round_trip_carries_scope() {
        let state = DocumentsState::new();
        let tenant = Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap();
        let (token, _) = state.issue_signed_token("doc-1", StorageScope::Tenant(tenant), 3_600);
        let (document_id, scope) = state.verify_signed_token(&token).unwrap();
        assert_eq!(document_id, "doc-1");
        assert_eq!(scope, StorageScope::Tenant(tenant));
        // A token issued for the system scope verifies as system.
        let (token, _) = state.issue_signed_token("doc-1", StorageScope::System, 3_600);
        let (_, scope) = state.verify_signed_token(&token).unwrap();
        assert_eq!(scope, StorageScope::System);
        // Tampering with the scope segment invalidates the token:
        // the scope is HMAC-signed, not merely asserted.
        let tampered = token.replacen(".system.", ".019a1234-5678-7abc-def0-123456789abc.", 1);
        assert!(state.verify_signed_token(&tampered).is_err());
    }

    #[tokio::test]
    async fn chunked_upload_spools_parts_to_disk_and_streams_assembly() {
        // 5 MiB payload in five 1 MiB parts, stored out of order
        // with a resume gap, then assembled + finalized from disk.
        let mut state = DocumentsState::new();
        let spool_root =
            std::env::temp_dir().join(format!("unified-documents-spool-test-{}", Uuid::new_v4()));
        state.storage_dir = spool_root.clone();
        let _ = std::fs::remove_dir_all(&spool_root);
        let total: usize = 5 * 1024 * 1024;
        let mut payload = Vec::with_capacity(total);
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        while payload.len() < total {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            payload.extend_from_slice(&seed.to_le_bytes());
        }
        payload.truncate(total);
        let expected = sha256_hex(&payload);
        let chunk_size: u64 = 1024 * 1024;
        let part_count = ((total as u64 + chunk_size - 1) / chunk_size) as u32;
        assert_eq!(part_count, 5);
        let mut session = test_session("sess-spool", "active");
        session.total_size = total as u64;
        session.expected_sha256 = expected.clone();
        session.chunk_size = chunk_size;
        session.part_count = part_count;
        session.missing_parts = (0..part_count).collect();
        session.spool_dir = Some(
            spool_root
                .join("uploads")
                .join("sessions")
                .join("sess-spool"),
        );
        state.sessions.insert(session.id.clone(), session.clone());
        state.parts.insert(session.id.clone(), DashMap::new());
        // Parts arrive out of order: 3, 0, 4, 1 — part 2 is the
        // resume gap.
        for index in [3u32, 0, 4, 1] {
            let start = index as usize * 1024 * 1024;
            let end = ((index as usize + 1) * 1024 * 1024).min(total);
            let bytes = &payload[start..end];
            let part = state
                .store_part_bytes(&session, index, bytes, sha256_hex(bytes))
                .unwrap();
            assert_eq!(part.size, bytes.len() as u64);
            assert!(part.path.is_file(), "part must be spooled to disk");
            state
                .parts
                .entry(session.id.clone())
                .or_default()
                .insert(index, part);
        }
        // Resume: exactly part 2 is missing.
        let received: Vec<u32> = state
            .parts
            .get(&session.id)
            .unwrap()
            .iter()
            .map(|entry| *entry.key())
            .collect();
        let missing: Vec<u32> = (0..part_count).filter(|i| !received.contains(i)).collect();
        assert_eq!(missing, vec![2]);
        let index = 2u32;
        let start = index as usize * 1024 * 1024;
        let end = ((index as usize + 1) * 1024 * 1024).min(total);
        let bytes = &payload[start..end];
        let part = state
            .store_part_bytes(&session, index, bytes, sha256_hex(bytes))
            .unwrap();
        state
            .parts
            .entry(session.id.clone())
            .or_default()
            .insert(index, part);
        // Streaming assembly: bytes match, digest matches.
        let (assembled, digest) = state.assemble_session_file(&session).unwrap();
        assert_eq!(digest, expected);
        assert_eq!(std::fs::read(&assembled).unwrap(), payload);
        // Streaming finalize (lazy): document + object on disk,
        // bytes match; the checksum is NULL until the finalize
        // worker hashes the stored object, and no `.sha256`
        // sidecar exists yet.
        let record = state
            .finalize_upload_from_file(
                "big-report.txt".to_string(),
                "documents".to_string(),
                String::new(),
                "user-1".to_string(),
                "owner".to_string(),
                Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap(),
                None,
                &assembled,
                total as u64,
                expected.clone(),
                "private".to_string(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(record.size, total as u64);
        assert_eq!(record.checksum, None);
        assert_eq!(record.visibility, "private");
        // The stored key is scope-free; the object lives under the
        // scope root resolved from the tenant at runtime.
        let tenant_scope =
            StorageScope::Tenant(Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap());
        let object_path = spool_root.join(tenant_scope.provider_key(&record.storage_key));
        assert_eq!(std::fs::read(&object_path).unwrap(), payload);
        let mut sidecar = object_path.as_os_str().to_os_string();
        sidecar.push(".sha256");
        assert!(
            !Path::new(&sidecar).exists(),
            "lazy finalize stores no sidecar before the worker verifies the hash"
        );
        // Completion cleanup removes the spool directory.
        state.parts.remove(&session.id);
        state.remove_spool_dir(&session);
        assert!(
            !spool_root
                .join("uploads")
                .join("sessions")
                .join("sess-spool")
                .exists()
        );
        let _ = std::fs::remove_dir_all(&spool_root);
    }

    #[test]
    fn config_settings_apply_and_validate() {
        let state = DocumentsState::new();
        // Defaults: no override, 100 MiB cap.
        assert_eq!(state.settings().spool_dir, None);
        assert_eq!(
            state.settings().max_chunk_ram_bytes,
            DocumentsConfig::MAX_CHUNK_RAM_DEFAULT
        );
        assert_eq!(
            state.settings().max_chunk_ram_bytes,
            DocumentsConfig::MAX_CHUNK_RAM_MIN
        );
        // Default spool root lives under the storage root.
        assert_eq!(
            state.effective_spool_root(),
            state.storage_dir.join("temps").join("uploads")
        );
        // Valid config applies.
        let mut config = DocumentsConfig::default();
        config.spool_dir = Some("/tmp/unified-spool".to_string());
        config.max_chunk_ram_bytes = Some(512 * 1024 * 1024);
        state.apply_config_settings(&config).unwrap();
        assert_eq!(
            state.settings().spool_dir.as_deref(),
            Some("/tmp/unified-spool")
        );
        assert_eq!(state.settings().max_chunk_ram_bytes, 512 * 1024 * 1024);
        assert_eq!(
            state.effective_spool_root(),
            PathBuf::from("/tmp/unified-spool")
        );
        // Out-of-range cap is rejected.
        let mut bad = DocumentsConfig::default();
        bad.max_chunk_ram_bytes = Some(50 * 1024 * 1024);
        assert!(state.apply_config_settings(&bad).is_err());
        let mut bad = DocumentsConfig::default();
        bad.max_chunk_ram_bytes = Some(9 * 1024 * 1024 * 1024);
        assert!(state.apply_config_settings(&bad).is_err());
        // Empty spool dir is rejected.
        let mut bad = DocumentsConfig::default();
        bad.spool_dir = Some("   ".to_string());
        assert!(state.apply_config_settings(&bad).is_err());
        // A failed apply leaves the previous settings alone.
        assert_eq!(
            state.settings().spool_dir.as_deref(),
            Some("/tmp/unified-spool")
        );
    }

    #[test]
    fn session_spool_dir_is_date_nested() {
        let state = DocumentsState::new();
        let created = Utc.with_ymd_and_hms(2026, 11, 23, 10, 30, 0).unwrap();
        assert_eq!(
            state.session_spool_dir("sess-1", created).unwrap(),
            state
                .effective_spool_root()
                .join("2026")
                .join("11")
                .join("23")
                .join("sess-1")
        );
        // Invalid ids can never escape the spool root.
        assert!(state.session_spool_dir("../escape", created).is_err());
        assert!(state.session_spool_dir("a/b", created).is_err());
    }

    #[test]
    fn spool_part_files_are_six_digit_zero_padded() {
        let mut state = DocumentsState::new();
        let spool_root = std::env::temp_dir()
            .join(format!("unified-documents-parts-test-{}", Uuid::new_v4()));
        state.storage_dir = spool_root.clone();
        let _ = std::fs::remove_dir_all(&spool_root);
        let session = test_session("sess-parts", "active");
        let part = state
            .store_part_bytes(&session, 1, b"hello", sha256_hex(b"hello"))
            .unwrap();
        assert_eq!(
            part.path.file_name().unwrap().to_string_lossy(),
            "000001.part"
        );
        assert!(part.path.is_file(), "part must be spooled to disk");
        let part = state
            .store_part_bytes(&session, 42, b"world", sha256_hex(b"world"))
            .unwrap();
        assert_eq!(
            part.path.file_name().unwrap().to_string_lossy(),
            "000042.part"
        );
        // Parts live under the date-nested spool directory, dated
        // by the session creation date (UTC).
        let date = session.created_at.format("%Y/%m/%d").to_string();
        assert!(
            part.path
                .starts_with(spool_root.join("temps").join("uploads").join(&date)),
            "part path must be date-nested: {}",
            part.path.display()
        );
        let _ = std::fs::remove_dir_all(&spool_root);
    }

    #[tokio::test]
    async fn reupload_stores_its_own_copy() {
        // No deduplication: every upload stores its own copy, so
        // the store writes through a temp provider rooted at a
        // scratch directory (never the real `data/documents`).
        let mut state = DocumentsState::new();
        let store_root = std::env::temp_dir()
            .join(format!("unified-documents-own-copy-test-{}", Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&store_root);
        state.provider = Some(Arc::new(
            LocalProvider::new(store_root.clone()).unwrap(),
        ));
        let tenant = Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap();
        let tenant_scope = StorageScope::Tenant(tenant);
        let bytes = b"same bytes, own copy";
        // Two uploads of identical bytes mint two distinct objects
        // (one fresh UUIDv7 per upload — the "upload freely"
        // semantics); both files exist behind their own keys.
        // Stored keys are scope-free; the files live under the
        // scope root resolved from the scope at runtime.
        let first = state
            .store_content(tenant_scope, StorageArea::Private, ContentKind::Documents, Some("txt"), bytes)
            .await
            .unwrap();
        assert!(first.ends_with(".txt"));
        assert!(!first.contains("tenants/"));
        assert!(store_root.join(tenant_scope.provider_key(&first)).is_file());
        let second = state
            .store_content(tenant_scope, StorageArea::Private, ContentKind::Documents, Some("txt"), bytes)
            .await
            .unwrap();
        assert_ne!(first, second);
        assert!(
            store_root
                .join(tenant_scope.provider_key(&second))
                .is_file()
        );
        // A different tenant gets a different key for the same
        // bytes: no cross-tenant sharing. The keys themselves are
        // scope-free (neither carries a tenant segment); the
        // separation lives in the provider roots.
        let other = Uuid::parse_str("019a9999-8888-7abc-def0-123456789abc").unwrap();
        let other_scope = StorageScope::Tenant(other);
        let other_key = state
            .store_content(other_scope, StorageArea::Private, ContentKind::Documents, Some("txt"), bytes)
            .await
            .unwrap();
        assert_ne!(first, other_key);
        assert!(!other_key.contains("tenants/"));
        assert_ne!(
            tenant_scope.provider_key(&first),
            other_scope.provider_key(&other_key)
        );
        assert!(
            store_root
                .join(other_scope.provider_key(&other_key))
                .is_file()
        );
        // Tenant and system keys share one shape (the key alone
        // never says which scope an object belongs to) but land
        // under different provider roots.
        let system_key = state
            .store_content(
                StorageScope::System,
                StorageArea::Private,
                ContentKind::Documents,
                Some("txt"),
                bytes,
            )
            .await
            .unwrap();
        assert!(!system_key.contains("system/"));
        assert_ne!(
            tenant_scope.provider_key(&system_key),
            StorageScope::System.provider_key(&system_key)
        );
        assert!(
            store_root
                .join(StorageScope::System.provider_key(&system_key))
                .is_file()
        );
        // Same content, different extension -> a different key (a
        // separate copy) — intended, consistent with no dedup.
        let pdf_key = state
            .store_content(tenant_scope, StorageArea::Private, ContentKind::Documents, Some("pdf"), bytes)
            .await
            .unwrap();
        assert_ne!(first, pdf_key);
        assert!(pdf_key.ends_with(".pdf"));
        assert!(
            store_root
                .join(tenant_scope.provider_key(&pdf_key))
                .is_file()
        );
        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn orphan_sweep_removes_dated_orphans_but_keeps_live_sessions() {
        let mut state = DocumentsState::new();
        let spool_root = std::env::temp_dir()
            .join(format!("unified-documents-sweep-test-{}", Uuid::new_v4()));
        state.storage_dir = spool_root.clone();
        let _ = std::fs::remove_dir_all(&spool_root);
        // One live session; the rest are orphans.
        let live = test_session("sess-live", "active");
        state.sessions.insert(live.id.clone(), live);
        let live_dir = spool_root
            .join("temps")
            .join("uploads")
            .join("2026")
            .join("11")
            .join("23")
            .join("sess-live");
        let orphan_day = spool_root
            .join("temps")
            .join("uploads")
            .join("2026")
            .join("11")
            .join("23")
            .join("sess-orphan");
        let orphan_other_day = spool_root
            .join("temps")
            .join("uploads")
            .join("2026")
            .join("11")
            .join("22")
            .join("sess-old");
        for dir in [&live_dir, &orphan_day, &orphan_other_day] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("000001.part"), b"x").unwrap();
        }
        assert_eq!(state.sweep_orphan_spool_dirs(), 2);
        assert!(live_dir.is_dir(), "live session must survive the sweep");
        assert!(!orphan_day.exists(), "dated orphan must be swept");
        assert!(
            !orphan_other_day.exists(),
            "orphan on another day must be swept"
        );
        // Empty date directories left behind are removed; the live
        // session's date chain stays.
        assert!(
            !spool_root
                .join("temps")
                .join("uploads")
                .join("2026")
                .join("11")
                .join("22")
                .exists(),
            "emptied date directory must be removed"
        );
        assert!(
            spool_root
                .join("temps")
                .join("uploads")
                .join("2026")
                .join("11")
                .join("23")
                .is_dir(),
            "date directory holding a live session must stay"
        );
        let _ = std::fs::remove_dir_all(&spool_root);
    }

    #[test]
    fn scan_respects_in_ram_cap() {
        let state = DocumentsState::new();
        state.register_scanner(Arc::new(FixedScanner {
            name: "fixed".to_string(),
            verdict: ScanVerdict::Clean,
        }));
        // A file larger than the cap is never opened for a
        // non-streaming scanner: the verdict is Error, fail closed.
        let (status, _threat, _scanner) = state.scan_document_file(
            "doc-1",
            1,
            Path::new("/nonexistent/does-not-exist.bin"),
            200 * 1024 * 1024,
        );
        assert_eq!(status, ScanStatus::Error);
        // A small file is scanned normally.
        let dir =
            std::env::temp_dir().join(format!("unified-documents-scan-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("small.txt");
        std::fs::write(&path, b"hello").unwrap();
        let (status, threat, scanner) = state.scan_document_file("doc-1", 1, &path, 5);
        assert_eq!(status, ScanStatus::Clean);
        assert_eq!(threat, None);
        assert_eq!(scanner.as_deref(), Some("fixed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphan_spool_dirs_are_swept() {
        let mut state = DocumentsState::new();
        let spool_root =
            std::env::temp_dir().join(format!("unified-documents-sweep-test-{}", Uuid::new_v4()));
        state.storage_dir = spool_root.clone();
        let sessions_root = spool_root
            .join("temps")
            .join("uploads")
            .join("2026")
            .join("11")
            .join("23");
        // An orphan directory (no live session) and a live one.
        let orphan = sessions_root.join("orphan-session");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("000001.part"), b"junk").unwrap();
        let mut live = test_session("live-session", "active");
        live.spool_dir = Some(sessions_root.join("live-session"));
        std::fs::create_dir_all(live.spool_dir.as_ref().unwrap()).unwrap();
        state.sessions.insert(live.id.clone(), live);
        assert_eq!(state.sweep_orphan_spool_dirs(), 1);
        assert!(!orphan.exists());
        assert!(sessions_root.join("live-session").exists());
        let _ = std::fs::remove_dir_all(&spool_root);
    }
}
