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
//!
//! C11 write-through (task 271): when a database pool is attached
//! (see `attach_finalize_pool`, called once from
//! `DocumentsModule::init`), every job mutation writes the
//! `finalize_jobs` table first and the in-memory map second, the
//! claim is arbitrated in the database (one winner across
//! instances), and the worker re-syncs the map from the table on
//! startup and on every tick, so jobs survive a restart. Without
//! a pool the worker behaves exactly as before (in-memory only).

use super::*;
use diesel::prelude::*;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;
use unified_core::db::{
    DbPool,
    models::{FinalizeJob as DbFinalizeJob, NewFinalizeJob},
};

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

// ---------------------------------------------------------------------------
// Database write-through (C11, task 271).
//
// The pool is attached once, from `DocumentsModule::init`, through
// `attach_finalize_pool`:
//
// ```ignore
// if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
//     crate::modules::documents::state::finalize::attach_finalize_pool(pool);
// }
// ```
//
// Every helper below is a no-op when no pool is attached, so the
// worker and its unit tests behave exactly as before on a
// pool-less process. When the pool IS attached but the database
// is unreachable, the helpers fail soft (the error is swallowed
// at the call site) and the in-memory map stays authoritative
// for that tick — the same degraded-mode contract the licensing
// module uses (database reads fall back to the map when no pool
// is attached). A job row is only ever inserted when its parent
// rows exist (see `db_insert_job`): the `finalize_jobs` foreign
// keys reference `document_upload_sessions` / `documents`, and
// those rows are not persisted yet (the documents-metadata
// worker owns session/document persistence), so a job for an
// unpersisted session stays in-memory-only instead of failing
// the insert on the foreign key.
// ---------------------------------------------------------------------------

/// Attached database pool for the finalize write-through.
/// `None` (never attached) means in-memory-only operation.
static FINALIZE_DB_POOL: OnceLock<DbPool> = OnceLock::new();

/// Attach the database pool the finalize worker writes through
/// to. Idempotent: the first attach wins, later calls are
/// ignored, so a double `init` can never swap the pool under a
/// running worker.
pub(crate) fn attach_finalize_pool(pool: DbPool) {
    let _ = FINALIZE_DB_POOL.set(pool);
}

/// The attached pool, when one was attached.
fn finalize_pool() -> Option<DbPool> {
    FINALIZE_DB_POOL.get().cloned()
}

/// Parse a runtime id (a UUID v4 string from `new_id()`) into
/// the UUID the `finalize_jobs` table stores.
fn parse_job_uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

/// Build the insert row for a runtime job. `None` when the job
/// id or the session id is not a UUID (never in practice:
/// `new_id()` is UUID v4). An unparseable document id degrades
/// to `NULL`: the job is for a session whose document is only
/// created at finalize time.
fn job_insert_row(job: &FinalizeJob) -> Option<NewFinalizeJob> {
    Some(NewFinalizeJob {
        id: parse_job_uuid(&job.id)?,
        session_id: parse_job_uuid(&job.session_id)?,
        document_id: job.document_id.as_deref().and_then(parse_job_uuid),
        tenant_id: job.tenant_id,
    })
}

/// Convert a database row back into the runtime job shape.
/// `attempts` is clamped at zero: the column check forbids
/// negatives, but a defensive conversion never panics.
fn db_job_to_runtime(row: DbFinalizeJob) -> FinalizeJob {
    FinalizeJob {
        id: row.id.to_string(),
        session_id: row.session_id.to_string(),
        document_id: row.document_id.map(|id| id.to_string()),
        tenant_id: row.tenant_id,
        status: row.status,
        attempts: row.attempts.max(0) as u32,
        last_error: row.last_error,
        claimed_at: row.claimed_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// True when the upload session row exists in the database.
/// A finalize job may only reference a persisted session (the
/// `session_id` foreign key), so the write-through checks this
/// before inserting the job row.
fn db_session_exists(pool: &DbPool, session_id: Uuid) -> UnifiedResult<bool> {
    use unified_core::db::schema::document_upload_sessions::dsl as sess_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    let found = sess_dsl::document_upload_sessions
        .filter(sess_dsl::id.eq(session_id))
        .select(sess_dsl::id)
        .first::<Uuid>(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load upload session: {error}")))?;
    Ok(found.is_some())
}

/// True when the document row exists in the database (same
/// foreign-key guard as `db_session_exists`, for `document_id`).
fn db_document_exists(pool: &DbPool, document_id: Uuid) -> UnifiedResult<bool> {
    use unified_core::db::schema::documents::dsl as doc_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    let found = doc_dsl::documents
        .filter(doc_dsl::id.eq(document_id))
        .select(doc_dsl::id)
        .first::<Uuid>(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load document: {error}")))?;
    Ok(found.is_some())
}

/// Insert a job row. Returns `false` (and inserts nothing) when
/// the job cannot be represented as a row, or when its session
/// (or document) row does not exist yet in the database — the
/// caller keeps the job in-memory-only in both cases. The
/// insert is idempotent on the job id, so a retried creation
/// never duplicates the row.
fn db_insert_job(pool: &DbPool, job: &FinalizeJob) -> UnifiedResult<bool> {
    let Some(row) = job_insert_row(job) else {
        return Ok(false);
    };
    if !db_session_exists(pool, row.session_id)? {
        return Ok(false);
    }
    if let Some(document_id) = row.document_id {
        if !db_document_exists(pool, document_id)? {
            return Ok(false);
        }
    }
    use unified_core::db::schema::finalize_jobs::dsl as fj;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(fj::finalize_jobs)
        .values(&row)
        .on_conflict(fj::id)
        .do_nothing()
        .execute(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot insert finalize job: {error}")))?;
    Ok(true)
}

/// Try to claim one job row in the database: `pending` ->
/// `finalizing`, one winner. Returns `Ok(Some(true))` when this
/// instance won the claim, `Ok(Some(false))` when the row exists
/// but is no longer `pending` (another instance claimed it
/// first), and `Ok(None)` when the row does not exist (an
/// in-memory-only job whose session was never persisted). A
/// database error propagates: the caller keeps the in-memory
/// claim instead of dropping work it already owns.
fn db_try_claim_job(pool: &DbPool, job_id: Uuid) -> UnifiedResult<Option<bool>> {
    use unified_core::db::schema::finalize_jobs::dsl as fj;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    let affected = diesel::update(
        fj::finalize_jobs
            .filter(fj::id.eq(job_id))
            .filter(fj::status.eq("pending")),
    )
    .set((
        fj::status.eq("finalizing"),
        fj::claimed_at.eq(now()),
        fj::updated_at.eq(now()),
    ))
    .execute(&mut conn)
    .map_err(|error| UnifiedError::internal(format!("Cannot claim finalize job: {error}")))?;
    if affected == 1 {
        return Ok(Some(true));
    }
    // Zero rows: either the row is already claimed/finished, or
    // it was never written (in-memory-only job). Tell the two
    // apart so the caller only rolls back a real loss.
    let exists = fj::finalize_jobs
        .filter(fj::id.eq(job_id))
        .select(fj::id)
        .first::<Uuid>(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load finalize job: {error}")))?
        .is_some();
    Ok(if exists { Some(false) } else { None })
}

/// Reap stale `finalizing` rows back to `pending` (attempts + 1,
/// claim cleared). Returns the number of rows reaped.
fn db_reap_stale_claims(pool: &DbPool, cutoff: DateTime<Utc>) -> UnifiedResult<usize> {
    use unified_core::db::schema::finalize_jobs::dsl as fj;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    let affected = diesel::update(
        fj::finalize_jobs
            .filter(fj::status.eq("finalizing"))
            .filter(fj::claimed_at.lt(cutoff)),
    )
    .set((
        fj::status.eq("pending"),
        fj::attempts.eq(fj::attempts + 1),
        fj::claimed_at.eq(None::<DateTime<Utc>>),
        fj::updated_at.eq(now()),
    ))
    .execute(&mut conn)
    .map_err(|error| UnifiedError::internal(format!("Cannot reap finalize claims: {error}")))?;
    Ok(affected as usize)
}

/// Mark a job row terminal (`verified` or `failed`), clearing
/// the claim and recording the failure reason when one is given.
fn db_finish_job(
    pool: &DbPool,
    job_id: Uuid,
    status: &str,
    last_error: Option<&str>,
) -> UnifiedResult<()> {
    use unified_core::db::schema::finalize_jobs::dsl as fj;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::update(fj::finalize_jobs.filter(fj::id.eq(job_id)))
        .set((
            fj::status.eq(status),
            fj::last_error.eq(last_error),
            fj::claimed_at.eq(None::<DateTime<Utc>>),
            fj::updated_at.eq(now()),
        ))
        .execute(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot finish finalize job: {error}")))?;
    Ok(())
}

/// Load the open (`pending` / `finalizing`) job rows.
fn db_load_open_jobs(pool: &DbPool) -> UnifiedResult<Vec<DbFinalizeJob>> {
    use unified_core::db::schema::finalize_jobs::dsl as fj;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    fj::finalize_jobs
        .filter(fj::status.eq_any(vec!["pending", "finalizing"]))
        .select(DbFinalizeJob::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load finalize jobs: {error}")))
}

/// Load job rows by id (any status). Empty input loads nothing:
/// an empty `IN ()` is never sent to the database.
fn db_load_finalize_jobs_by_id(pool: &DbPool, ids: &[Uuid]) -> UnifiedResult<Vec<DbFinalizeJob>> {
    use unified_core::db::schema::finalize_jobs::dsl as fj;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    fj::finalize_jobs
        .filter(fj::id.eq_any(ids))
        .select(DbFinalizeJob::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load finalize jobs: {error}")))
}

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
    /// runtime. The first action is the database sync and the
    /// startup re-scan, so jobs orphaned by a restart are picked
    /// up immediately.
    pub(crate) fn start_finalize_worker(self: &Arc<Self>) {
        if self.finalize_worker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let state = Arc::clone(self);
        let task = async move {
            state.db_sync_finalize_jobs();
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

    /// Synchronize the in-memory job map with the `finalize_jobs`
    /// table (idempotent): open rows whose session is present in
    /// the map are hydrated, rows another instance already
    /// claimed are adopted (a `pending` map row becomes
    /// `finalizing`, so this instance never re-claims work
    /// another instance owns), and map rows whose database row
    /// reached a terminal state (`verified` / `failed`) are
    /// marked terminal here, so they are never re-claimed after
    /// a restart. Rows whose session is not in the map stay in
    /// the database: the job cannot run until its session is
    /// hydrated (session persistence is the documents-metadata
    /// worker's follow-up), and hydrating it would let the
    /// worker fail it as "session not found". A map row this
    /// instance already claimed (`finalizing`) is never
    /// downgraded from a database row: the in-memory claim is
    /// the live one, and the database claim that follows it is
    /// the cross-instance arbiter. Returns the number of map
    /// rows inserted or updated. No-op when no pool is attached
    /// or the database is unreachable.
    pub(crate) fn db_sync_finalize_jobs(&self) -> usize {
        let Some(pool) = finalize_pool() else {
            return 0;
        };
        let Ok(open_rows) = db_load_open_jobs(&pool) else {
            return 0;
        };
        let mut changed = 0;
        for row in open_rows {
            let job = db_job_to_runtime(row);
            match self.finalize_jobs.get_mut(&job.id) {
                Some(mut entry) => {
                    if entry.status == "pending" && job.status == "finalizing" {
                        entry.status = job.status;
                        entry.claimed_at = job.claimed_at;
                        entry.attempts = job.attempts;
                        entry.updated_at = job.updated_at;
                        changed += 1;
                    }
                }
                None => {
                    if self.sessions.contains_key(&job.session_id) {
                        self.finalize_jobs.insert(job.id.clone(), job);
                        changed += 1;
                    }
                }
            }
        }
        // Terminal rows: only rows this process already knows
        // about are checked, so one query covers them.
        let known_ids: Vec<Uuid> = self
            .finalize_jobs
            .iter()
            .filter(|entry| matches!(entry.value().status.as_str(), "pending" | "finalizing"))
            .filter_map(|entry| Uuid::parse_str(entry.key()).ok())
            .collect();
        if let Ok(rows) = db_load_finalize_jobs_by_id(&pool, &known_ids) {
            for row in rows {
                if !matches!(row.status.as_str(), "verified" | "failed") {
                    continue;
                }
                if let Some(mut entry) = self.finalize_jobs.get_mut(&row.id.to_string()) {
                    if matches!(entry.status.as_str(), "pending" | "finalizing") {
                        entry.status = row.status.clone();
                        entry.last_error = row.last_error.clone();
                        entry.claimed_at = None;
                        entry.updated_at = row.updated_at;
                        changed += 1;
                    }
                }
            }
        }
        changed
    }

    /// Create a finalize job for a received session (called by
    /// complete_session right after the document row is created).
    /// Idempotent for the session: an existing `pending` /
    /// `finalizing` job for the session is returned instead of a
    /// duplicate. The row is written to the database first when
    /// a pool is attached (see `db_insert_job`); the map insert
    /// still runs when the row cannot be persisted, so the
    /// worker keeps working in-memory and the startup re-scan
    /// re-enqueues the job.
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
        if let Some(pool) = finalize_pool() {
            // Best-effort write-through: when the row cannot be
            // persisted (the session/document rows do not exist
            // yet, or the database is unreachable) the job still
            // runs in-memory; see `db_insert_job`.
            let _ = db_insert_job(&pool, &job);
        }
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
    /// forever). The database sync in `db_sync_finalize_jobs`
    /// runs before this (see `start_finalize_worker`), so jobs
    /// whose rows survived the restart are hydrated first and
    /// this scan only creates jobs for sessions with no row at
    /// all; a restart still orphans sessions whose rows were
    /// never persisted, until session persistence lands (the
    /// documents-metadata worker's follow-up).
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
    /// When a pool is attached the database claim runs right
    /// after the in-memory claim and arbitrates across
    /// instances: when another instance already claimed the
    /// row, this instance rolls its in-memory claim back to
    /// `pending` and tries the next job instead of hashing the
    /// same object twice. A job with no database row, or a
    /// database error, keeps the in-memory claim. Returns the
    /// claimed job, or `None` when nothing is claimable.
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
                let claimed = entry.clone();
                drop(entry);
                if let Some(pool) = finalize_pool() {
                    if let Some(job_id) = parse_job_uuid(&job.id) {
                        // A database error keeps the in-memory
                        // claim (see `db_try_claim_job`); only a
                        // lost claim rolls it back.
                        if let Ok(Some(won)) = db_try_claim_job(&pool, job_id) {
                            if !won {
                                if let Some(mut entry) = self.finalize_jobs.get_mut(&job.id) {
                                    entry.status = "pending".to_string();
                                    entry.claimed_at = None;
                                    entry.updated_at = now();
                                }
                                continue;
                            }
                        }
                    }
                }
                return Some(claimed);
            }
        }
        None
    }

    /// Reap stale `finalizing` claims back to `pending`
    /// (attempts + 1, claimed_at cleared). Returns the number
    /// reaped. When a pool is attached the database reap runs
    /// first and its result is mirrored into the map, so one
    /// stale claim is never counted twice (once by the database
    /// reap, once by the in-memory pass below); the in-memory
    /// pass then only reaps jobs with no database row.
    pub(crate) fn reap_stale_finalize_claims(&self) -> usize {
        let cutoff = now() - FINALIZE_STALE_CLAIM;
        if let Some(pool) = finalize_pool() {
            let stale_ids: Vec<Uuid> = self
                .finalize_jobs
                .iter()
                .filter(|entry| {
                    entry.value().status == "finalizing"
                        && entry
                            .value()
                            .claimed_at
                            .is_some_and(|claimed_at| claimed_at < cutoff)
                })
                .filter_map(|entry| Uuid::parse_str(entry.key()).ok())
                .collect();
            if !stale_ids.is_empty() {
                let _ = db_reap_stale_claims(&pool, cutoff);
                if let Ok(rows) = db_load_finalize_jobs_by_id(&pool, &stale_ids) {
                    for row in rows {
                        if row.status != "pending" {
                            continue;
                        }
                        if let Some(mut entry) = self.finalize_jobs.get_mut(&row.id.to_string()) {
                            if entry.status == "finalizing" {
                                entry.status = "pending".to_string();
                                entry.attempts = row.attempts.max(0) as u32;
                                entry.claimed_at = None;
                                entry.updated_at = row.updated_at;
                            }
                        }
                    }
                }
            }
        }
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

    /// One worker tick: sync the map from the database, reap
    /// stale claims, then claim and run jobs while the queue is
    /// idle (or a job is starving), up to the concurrency bound.
    async fn finalize_tick(self: &Arc<Self>) {
        self.db_sync_finalize_jobs();
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
                if let Some(pool) = finalize_pool() {
                    if let Some(job_id) = parse_job_uuid(&job.id) {
                        let _ = db_finish_job(&pool, job_id, "verified", None);
                    }
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
                if let Some(pool) = finalize_pool() {
                    if let Some(job_id) = parse_job_uuid(&job.id) {
                        let _ = db_finish_job(&pool, job_id, "failed", Some(&error.to_string()));
                    }
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

    #[test]
    fn db_job_insert_row_maps_ids_and_tenant() {
        let job = test_job("sess-1", now());
        let row = job_insert_row(&job).expect("uuid ids map");
        assert_eq!(row.id.to_string(), job.id);
        assert_eq!(row.session_id.to_string(), job.session_id);
        assert_eq!(row.document_id, None);
        assert_eq!(
            row.tenant_id,
            Some(Uuid::parse_str("019a1234-5678-7abc-def0-123456789abc").unwrap())
        );
    }

    #[test]
    fn db_job_insert_row_rejects_non_uuid_ids() {
        let mut job = test_job("sess-1", now());
        job.id = "not-a-uuid".to_string();
        assert!(job_insert_row(&job).is_none());
        let mut job = test_job("sess-1", now());
        job.session_id = "not-a-uuid".to_string();
        assert!(job_insert_row(&job).is_none());
    }

    #[test]
    fn db_job_round_trips_through_runtime() {
        let job = test_job("sess-1", now());
        let row = job_insert_row(&job).expect("uuid ids map");
        let db_row = DbFinalizeJob {
            id: row.id,
            session_id: row.session_id,
            document_id: row.document_id,
            tenant_id: row.tenant_id,
            status: "pending".to_string(),
            attempts: 3,
            last_error: Some("boom".to_string()),
            claimed_at: None,
            created_at: job.created_at,
            updated_at: job.updated_at,
        };
        let back = db_job_to_runtime(db_row);
        assert_eq!(back.id, job.id);
        assert_eq!(back.session_id, job.session_id);
        assert_eq!(back.tenant_id, job.tenant_id);
        assert_eq!(back.attempts, 3);
        assert_eq!(back.last_error.as_deref(), Some("boom"));
        // A negative stored attempt count (forbidden by the
        // column check, but never trusted blindly) clamps to 0.
        let mut negative = test_job("sess-2", now());
        negative.attempts = 0;
        let row = job_insert_row(&negative).expect("uuid ids map");
        let db_row = DbFinalizeJob {
            id: row.id,
            session_id: row.session_id,
            document_id: None,
            tenant_id: None,
            status: "pending".to_string(),
            attempts: -5,
            last_error: None,
            claimed_at: None,
            created_at: negative.created_at,
            updated_at: negative.updated_at,
        };
        assert_eq!(db_job_to_runtime(db_row).attempts, 0);
    }

    #[test]
    fn db_sync_is_noop_without_pool() {
        let state = DocumentsState::new();
        let job = test_job("sess-1", now());
        state.finalize_jobs.insert(job.id.clone(), job.clone());
        assert_eq!(state.db_sync_finalize_jobs(), 0);
        assert_eq!(state.finalize_jobs.len(), 1);
        // Creating and claiming also stay purely in-memory
        // without a pool: the worker keeps working.
        let session = test_session("sess-1", "received");
        let created = state.create_finalize_job(&session, None);
        assert_eq!(created.session_id, "sess-1");
        assert!(state.claim_finalize_job().is_some());
    }
}
