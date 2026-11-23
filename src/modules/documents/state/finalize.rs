//! Documents state — lazy finalize worker (split from state.rs).
//!
//! The worker hashes stored objects AFTER the upload completes
//! (lazy finalize): the upload returns as soon as all parts are
//! received, and the whole-file SHA-256 runs off the request path.
//! Jobs are the durable unit of work (see `FinalizeJob` in
//! types.rs and the `finalize_jobs` table in migration 000123);
//! the worker claims them atomically, hashes the stored object in
//! 64 KiB chunks, and either verifies the checksum or fails the
//! job (mismatch -> object deleted, session failed, owner
//! notified through `document.finalize.failed`).
//!
//! The worker drains only while the upload queue is really empty
//! (no part received within the idle window); a job waiting longer
//! than the starvation guard runs anyway. Hashing is CPU-bound,
//! so concurrent jobs are capped.

use super::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often the worker wakes to claim work.
const FINALIZE_TICK: Duration = Duration::from_secs(5);
/// A `finalizing` claim older than this is stale (the worker was
/// killed mid-hash) and is re-enqueued.
const FINALIZE_STALE_CLAIM: chrono::Duration = chrono::Duration::minutes(10);
/// Maximum concurrent hash jobs (hashing is CPU-bound).
const FINALIZE_MAX_CONCURRENT: usize = 4;
/// A job waiting longer than this runs even while uploads are
/// active (the starvation guard, N = 24h).
const FINALIZE_STARVATION: chrono::Duration = chrono::Duration::hours(24);
/// No part received within this window means the upload queue is
/// really empty (idle) and the worker may drain.
const FINALIZE_IDLE_WINDOW: chrono::Duration = chrono::Duration::seconds(60);

/// Guard that decrements the running-jobs counter when the job
/// task ends (including on panic, through Drop).
struct FinalizeRunningGuard<'a>(&'a AtomicUsize);

impl Drop for FinalizeRunningGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl DocumentsState {
    /// Start the finalize worker (idempotent). Mirrors the
    /// scheduler's `start_ticker`: spawn on the tokio runtime
    /// when a handle exists, else a dedicated thread with its own
    /// runtime. The first action is the startup re-scan, so jobs
    /// orphaned by a restart are picked up immediately.
    pub(crate) fn start_finalize_worker(self: &Arc<Self>) {
        if self.finalize_worker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let state = Arc::clone(self);
        let task = async move {
            state.rescan_finalize_jobs();
            let mut interval = tokio::time::interval(FINALIZE_TICK);
            loop {
                interval.tick().await;
                state.finalize_tick().await;
            }
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(task);
        } else {
            std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(task);
            });
        }
    }

    /// Create a finalize job for a received session (called by
    /// complete_session right after the document row is created).
    /// Idempotent for the session: an existing `pending` /
    /// `finalizing` job for the session is returned instead of a
    /// duplicate.
    pub(crate) fn create_finalize_job(
        &self,
        session: &UploadSession,
        document_id: Option<String>,
    ) -> FinalizeJob {
        if let Some(job) = self
            .finalize_jobs
            .iter()
            .find(|entry| {
                entry.value().session_id == session.id
                    && matches!(entry.value().status.as_str(), "pending" | "finalizing")
            })
            .map(|entry| entry.value().clone())
        {
            return job;
        }
        let job = FinalizeJob {
            id: new_id(),
            session_id: session.id.clone(),
            document_id,
            tenant_id: session.tenant_id,
            status: "pending".to_string(),
            attempts: 0,
            last_error: None,
            claimed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        self.finalize_jobs.insert(job.id.clone(), job.clone());
        job
    }

    /// Mark a session `received` (all parts in, awaiting the
    /// finalize worker). Called by complete_session after the
    /// document row is created.
    pub(crate) fn mark_session_received(&self, session_id: &str) {
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            session.status = "received".to_string();
            session.updated_at = now();
        }
    }

    /// Startup re-scan (REQUIRED, idempotent): for every session
    /// in status `received`, create a `pending` job when no
    /// `pending` / `finalizing` job exists for it. Returns the
    /// number of jobs created. Without this, a restart silently
    /// orphans received sessions (their checksum stays NULL
    /// forever). Only the in-memory sessions are visible here —
    /// full restart recovery of the DB rows needs the C11
    /// write-through (task 271).
    pub(crate) fn rescan_finalize_jobs(&self) -> usize {
        let mut created = 0;
        for entry in self.sessions.iter() {
            let session = entry.value();
            if session.status != "received" {
                continue;
            }
            let has_job = self.finalize_jobs.iter().any(|job| {
                job.value().session_id == session.id
                    && matches!(job.value().status.as_str(), "pending" | "finalizing")
            });
            if has_job {
                continue;
            }
            self.create_finalize_job(session, session.document_id.clone());
            created += 1;
        }
        created
    }

    /// Claim one pending job atomically: `pending` ->
    /// `finalizing` with `claimed_at` set, under the job entry's
    /// lock (one winner). The oldest pending job wins (FIFO).
    /// Returns the claimed job, or `None` when nothing is
    /// claimable.
    pub(crate) fn claim_finalize_job(&self) -> Option<FinalizeJob> {
        let mut pending: Vec<FinalizeJob> = self
            .finalize_jobs
            .iter()
            .filter(|entry| entry.value().status == "pending")
            .map(|entry| entry.value().clone())
            .collect();
        pending.sort_by_key(|job| job.created_at);
        for job in pending {
            if let Some(mut entry) = self.finalize_jobs.get_mut(&job.id) {
                if entry.status != "pending" {
                    continue;
                }
                entry.status = "finalizing".to_string();
                entry.claimed_at = Some(now());
                entry.updated_at = now();
                return Some(entry.clone());
            }
        }
        None
    }

    /// Reap stale `finalizing` claims back to `pending`
    /// (attempts + 1, claimed_at cleared). Returns the number
    /// reaped.
    pub(crate) fn reap_stale_finalize_claims(&self) -> usize {
        let cutoff = now() - FINALIZE_STALE_CLAIM;
        let mut reaped = 0;
        for mut entry in self.finalize_jobs.iter_mut() {
            let job = entry.value_mut();
            if job.status == "finalizing" {
                if let Some(claimed_at) = job.claimed_at {
                    if claimed_at < cutoff {
                        job.status = "pending".to_string();
                        job.attempts = job.attempts.saturating_add(1);
                        job.claimed_at = None;
                        job.updated_at = now();
                        reaped += 1;
                    }
                }
            }
        }
        reaped
    }

    /// Whether any pending job is starving (waiting longer than
    /// the starvation guard): starving jobs run even while
    /// uploads are active.
    fn has_starving_finalize_job(&self) -> bool {
        let cutoff = now() - FINALIZE_STARVATION;
        self.finalize_jobs.iter().any(|entry| {
            entry.value().status == "pending" && entry.value().created_at < cutoff
        })
    }

    /// Whether the upload queue is really empty: no part received
    /// within the idle window.
    fn finalize_idle(&self) -> bool {
        let cutoff = now() - FINALIZE_IDLE_WINDOW;
        !self.parts.iter().any(|session_parts| {
            session_parts
                .value()
                .iter()
                .any(|part| part.value().received_at > cutoff)
        })
    }

    /// One worker tick: reap stale claims, then claim and run
    /// jobs while the queue is idle (or a job is starving), up to
    /// the concurrency bound.
    async fn finalize_tick(self: &Arc<Self>) {
        self.reap_stale_finalize_claims();
        while self.finalize_running.load(Ordering::SeqCst) < FINALIZE_MAX_CONCURRENT {
            if !self.finalize_idle() && !self.has_starving_finalize_job() {
                break;
            }
            let Some(job) = self.claim_finalize_job() else {
                break;
            };
            self.finalize_running.fetch_add(1, Ordering::SeqCst);
            let state = Arc::clone(self);
            tokio::spawn(async move {
                let _guard = FinalizeRunningGuard(&state.finalize_running);
                state.run_finalize_job(job).await;
            });
        }
    }

    /// Hash the stored object for one job: stream it in 64 KiB
    /// chunks and fold it into SHA-256, so RAM stays flat
    /// regardless of file size. Local storage reads the file
    /// directly on a blocking thread; remote providers have no
    /// ranged streaming hash, so the object is downloaded whole
    /// (bounded by the upload size cap) and hashed in memory.
    async fn hash_stored_object(
        &self,
        scope: &StorageScope,
        area: StorageArea,
        key: &str,
    ) -> UnifiedResult<String> {
        let provider = self.provider()?;
        let provider_key = scope.provider_key_for_area(area, key);
        if matches!(provider.kind(), StorageKind::Local) {
            let path = self.local_object_path(&provider_key)?;
            tokio::task::spawn_blocking(move || -> UnifiedResult<String> {
                let mut file = std::fs::File::open(&path).map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot open stored object for finalize: {error}"
                    ))
                })?;
                let mut hasher = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    let read = file.read(&mut buffer).map_err(|error| {
                        UnifiedError::internal(format!(
                            "Cannot read stored object for finalize: {error}"
                        ))
                    })?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                Ok(format!("{:x}", hasher.finalize()))
            })
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Finalize hash task failed: {error}"))
            })?
        } else {
            let bytes = provider.download(&provider_key).await?;
            Ok(sha256_hex(&bytes))
        }
    }

    /// Verify one claimed job: load the session + document, hash
    /// the stored object, compare with the session's expected
    /// hash. Returns the verified checksum and the document row.
    async fn verify_finalize_job(
        &self,
        job: &FinalizeJob,
    ) -> UnifiedResult<(String, DocumentRecord)> {
        let session = self
            .sessions
            .get(&job.session_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Finalize session not found"))?;
        let document_id = job
            .document_id
            .clone()
            .or(session.document_id.clone())
            .ok_or_else(|| {
                UnifiedError::internal("Finalize job has no document id")
            })?;
        let document = self.load_document(&document_id)?;
        let tenant_id = job.tenant_id.ok_or_else(|| {
            UnifiedError::internal("Finalize job has no tenant scope")
        })?;
        let scope = StorageScope::Tenant(tenant_id);
        let area = StorageArea::from_visibility(&document.visibility);
        let actual = self
            .hash_stored_object(&scope, area, &document.storage_key)
            .await?;
        if actual != session.expected_sha256 {
            return Err(UnifiedError::new(
                format!(
                    "Finalize hash mismatch for document {document_id}: expected {}, got {actual}",
                    session.expected_sha256
                ),
                400,
            ));
        }
        Ok((actual, document))
    }

    /// Run one claimed job to completion: verify, then either
    /// store the checksum (+ sidecar) and mark the job verified,
    /// or fail the job, mark the session failed, delete the
    /// untrusted object, and emit the failure event (the
    /// notification hook for the owner).
    async fn run_finalize_job(self: &Arc<Self>, job: FinalizeJob) {
        match self.verify_finalize_job(&job).await {
            Ok((checksum, document)) => {
                if let Some(mut record) = self.documents.get_mut(&document.id) {
                    record.checksum = Some(checksum.clone());
                    record.updated_at = now();
                }
                for mut version in self.versions.iter_mut() {
                    if version.document_id == document.id
                        && version.version == document.current_version
                    {
                        version.checksum = Some(checksum.clone());
                    }
                }
                // Write the `.sha256` sidecar now that the hash is
                // verified (best-effort: a sidecar failure does not
                // fail the verification — the DB row is the
                // authority).
                if let Some(tenant_id) = job.tenant_id {
                    let scope = StorageScope::Tenant(tenant_id);
                    let area = StorageArea::from_visibility(&document.visibility);
                    let _ = self.record_object_checksum(
                        &scope.provider_key_for_area(area, &document.storage_key),
                        &checksum,
                    );
                }
                if let Some(mut entry) = self.finalize_jobs.get_mut(&job.id) {
                    entry.status = "verified".to_string();
                    entry.claimed_at = None;
                    entry.updated_at = now();
                }
                self.emit(
                    events::FINALIZE_VERIFIED,
                    json!({
                        "document_id": document.id,
                        "session_id": job.session_id,
                        "checksum": checksum,
                    }),
                );
            }
            Err(error) => {
                // Quarantine: the bytes cannot be trusted, so the
                // object is deleted. The document row stays with
                // checksum NULL so the failure is visible to the
                // owner.
                if let Some(tenant_id) = job.tenant_id {
                    let document_id = job.document_id.clone().or_else(|| {
                        self.sessions
                            .get(&job.session_id)
                            .and_then(|session| session.document_id.clone())
                    });
                    if let Some(document_id) = document_id {
                        if let Ok(document) = self.load_document(&document_id) {
                            let scope = StorageScope::Tenant(tenant_id);
                            let area = StorageArea::from_visibility(&document.visibility);
                            if let Ok(provider) = self.provider() {
                                let _ = provider
                                    .delete(
                                        &scope.provider_key_for_area(area, &document.storage_key),
                                    )
                                    .await;
                            }
                        }
                    }
                }
                if let Some(mut session) = self.sessions.get_mut(&job.session_id) {
                    session.status = "failed".to_string();
                    session.updated_at = now();
                }
                if let Some(mut entry) = self.finalize_jobs.get_mut(&job.id) {
                    entry.status = "failed".to_string();
                    entry.last_error = Some(error.to_string());
                    entry.claimed_at = None;
                    entry.updated_at = now();
                }
                self.emit(
                    events::FINALIZE_FAILED,
                    json!({
                        "document_id": job.document_id,
                        "session_id": job.session_id,
                        "error": error.to_string(),
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(session_id: &str, created_at: DateTime<Utc>) -> FinalizeJob {
        FinalizeJob {
            id: new_id(),
            session_id: session_id.to_string(),
            document_id: None,
            tenant_id: Some(
                Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap(),
            ),
            status: "pending".to_string(),
            attempts: 0,
            last_error: None,
            claimed_at: None,
            created_at,
            updated_at: created_at,
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
            tenant_id: Some(
                Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap(),
            ),
            expires_at: now() + chrono::Duration::hours(1),
            created_at: now(),
            updated_at: now(),
            spool_dir: None,
        }
    }

    #[test]
    fn atomic_claim_gives_one_winner() {
        let state = DocumentsState::new();
        let job = test_job("sess-1", now());
        state.finalize_jobs.insert(job.id.clone(), job.clone());
        let first = state.claim_finalize_job().expect("first claim wins");
        assert_eq!(first.id, job.id);
        assert_eq!(first.status, "finalizing");
        assert!(first.claimed_at.is_some());
        // Second claim finds nothing pending.
        assert!(state.claim_finalize_job().is_none());
        // The row itself moved to `finalizing`.
        assert_eq!(
            state.finalize_jobs.get(&job.id).unwrap().status,
            "finalizing"
        );
    }

    #[test]
    fn atomic_claim_is_fifo() {
        let state = DocumentsState::new();
        let older = test_job("sess-old", now() - chrono::Duration::minutes(5));
        let newer = test_job("sess-new", now());
        state.finalize_jobs.insert(newer.id.clone(), newer.clone());
        state.finalize_jobs.insert(older.id.clone(), older.clone());
        let claimed = state.claim_finalize_job().expect("a job is claimed");
        assert_eq!(claimed.session_id, "sess-old");
    }

    #[test]
    fn stale_claim_is_reaped_and_retried() {
        let state = DocumentsState::new();
        let mut job = test_job("sess-1", now());
        job.status = "finalizing".to_string();
        job.claimed_at = Some(now() - chrono::Duration::minutes(11));
        job.attempts = 2;
        state.finalize_jobs.insert(job.id.clone(), job.clone());
        assert_eq!(state.reap_stale_finalize_claims(), 1);
        let reaped = state.finalize_jobs.get(&job.id).unwrap().clone();
        assert_eq!(reaped.status, "pending");
        assert_eq!(reaped.attempts, 3);
        assert!(reaped.claimed_at.is_none());
        // A fresh claim is not stale.
        let mut fresh = test_job("sess-2", now());
        fresh.status = "finalizing".to_string();
        fresh.claimed_at = Some(now());
        state.finalize_jobs.insert(fresh.id.clone(), fresh.clone());
        assert_eq!(state.reap_stale_finalize_claims(), 0);
    }

    #[test]
    fn startup_rescan_is_idempotent() {
        let state = DocumentsState::new();
        let session = test_session("sess-1", "received");
        state.sessions.insert(session.id.clone(), session.clone());
        // First scan creates one job.
        assert_eq!(state.rescan_finalize_jobs(), 1);
        // Second scan creates none (the pending job exists).
        assert_eq!(state.rescan_finalize_jobs(), 0);
        assert_eq!(state.finalize_jobs.len(), 1);
        // A session that is not `received` is ignored.
        let active = test_session("sess-2", "active");
        state.sessions.insert(active.id.clone(), active.clone());
        assert_eq!(state.rescan_finalize_jobs(), 0);
        assert_eq!(state.finalize_jobs.len(), 1);
    }

    #[test]
    fn create_finalize_job_is_idempotent_per_session() {
        let state = DocumentsState::new();
        let session = test_session("sess-1", "received");
        let first = state.create_finalize_job(&session, Some("doc-1".to_string()));
        let second = state.create_finalize_job(&session, Some("doc-1".to_string()));
        assert_eq!(first.id, second.id);
        assert_eq!(state.finalize_jobs.len(), 1);
    }
}
