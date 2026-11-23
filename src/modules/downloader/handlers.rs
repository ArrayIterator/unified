//! HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Caller identity and destination confinement.
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the assets module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the assets module's capability check).
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only — there is no per-record company check to
/// apply. Real partitioning needs a `tenant_id` on the record, a
/// C11 follow-up, not a C3 gate.
fn require_tenant(req: &HttpRequest) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above — records here carry
/// no company field, so there is nothing to partition on read.
fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

/// Base directory every download destination must live under,
/// from `DOWNLOADER_DOWNLOAD_DIR` (default `downloads`).
pub(crate) fn download_base_dir() -> PathBuf {
    std::env::var("DOWNLOADER_DOWNLOAD_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("downloads"))
}

/// Resolve a caller-supplied destination into a path inside the
/// download directory. Absolute paths and `..` traversal are
/// rejected; a trailing slash (or an existing directory) means the
/// file name comes from the URL. The returned path is absolute
/// (canonical base + relative destination), so a job can never
/// make the server write outside the download directory.
pub(crate) fn resolve_destination(raw: &str, url: &url::Url) -> UnifiedResult<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request("destination is required"));
    }
    let relative = Path::new(trimmed);
    if relative.is_absolute() {
        return Err(UnifiedError::bad_request(
            "destination must be a path relative to the download directory; absolute paths are rejected",
        ));
    }
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(UnifiedError::bad_request(
            "destination must not contain \"..\"",
        ));
    }
    let base = download_base_dir();
    std::fs::create_dir_all(&base).map_err(|error| {
        UnifiedError::internal(format!(
            "Cannot create download directory {}: {error}",
            base.display()
        ))
    })?;
    let base = base.canonicalize().map_err(|error| {
        UnifiedError::internal(format!(
            "Cannot resolve download directory {}: {error}",
            base.display()
        ))
    })?;
    let is_dir_hint = trimmed.ends_with('/') || trimmed.ends_with(std::path::MAIN_SEPARATOR);
    let mut destination = base.join(relative);
    if is_dir_hint || destination.is_dir() {
        let file_name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .unwrap_or("download");
        destination.push(file_name);
    }
    let file_name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .ok_or_else(|| {
            UnifiedError::bad_request("destination must name a file inside the download directory")
        })?;
    // Resolve the parent (creating it) so a symlinked parent
    // cannot smuggle the file outside the base directory.
    let parent = destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| base.clone());
    std::fs::create_dir_all(&parent).map_err(|error| {
        UnifiedError::internal(format!(
            "Cannot create destination directory {}: {error}",
            parent.display()
        ))
    })?;
    let parent = parent.canonicalize().map_err(|error| {
        UnifiedError::internal(format!(
            "Cannot resolve destination directory {}: {error}",
            parent.display()
        ))
    })?;
    if !parent.starts_with(&base) {
        return Err(UnifiedError::bad_request(
            "destination escapes the download directory",
        ));
    }
    Ok(parent.join(file_name))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn create_job(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateJobRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        // Authentication plus a manage permission: anyone who can
        // reach the API must not be able to make the server fetch
        // a URL and write the result to disk.
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_MANAGE)?;
        // SSRF guard: scheme, credentials, and resolved-address
        // checks run before any fetch is scheduled.
        validate_download_url(&body.url)?;
        let url = url::Url::parse(body.url.trim()).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid download URL \"{}\"", body.url))
        })?;
        let (algorithm, checksum) = match &body.checksum {
            Some(value) => {
                let (algorithm, hex) = parse_checksum(value)?;
                (Some(algorithm), Some(hex))
            }
            None => (None, None),
        };
        let destination = resolve_destination(&body.destination, &url)?;
        let client = build_client(&body)?;
        let part_path = {
            let mut part = destination.clone().into_os_string();
            part.push(".part");
            PathBuf::from(part)
        };
        let now = Utc::now();
        let job = Arc::new(DownloadJobState {
            id: Uuid::now_v7(),
            url: url.to_string(),
            destination,
            part_path,
            status: RwLock::new(DownloadStatus::Queued),
            downloaded_bytes: AtomicU64::new(0),
            total_bytes: RwLock::new(None),
            speed_bytes_per_sec: AtomicU64::new(0),
            checksum,
            checksum_algorithm: algorithm,
            checksum_ok: RwLock::new(None),
            error: RwLock::new(None),
            created_at: now,
            updated_at: RwLock::new(now),
            started_at: RwLock::new(None),
            finished_at: RwLock::new(None),
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            retry_count: AtomicU32::new(0),
            max_retries: body.max_retries.min(MAX_MAX_RETRIES),
            resumable: RwLock::new(None),
            etag: RwLock::new(None),
            last_modified: RwLock::new(None),
            max_bytes: body.max_bytes,
            client,
        });
        let view = job.snapshot();
        jobs_store().insert(job.id, Arc::clone(&job));
        let ctx_clone: Arc<AppContext> = Arc::clone(&ctx);
        tokio::spawn(async move { run_download(ctx_clone, job).await });
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_jobs(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_VIEW)?;
        let mut items: Vec<DownloadJobView> = jobs_store()
            .iter()
            .map(|entry| entry.value().snapshot())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|job| job.url.to_lowercase().contains(&needle));
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_job(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_VIEW)?;
        jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().snapshot())
            .ok_or_else(|| UnifiedError::not_found("Download job not found"))
    })();
    json::respond(result, debug)
}

fn find_job(id: Uuid) -> UnifiedResult<Arc<DownloadJobState>> {
    jobs_store()
        .get(&id)
        .map(|entry| Arc::clone(entry.value()))
        .ok_or_else(|| UnifiedError::not_found("Download job not found"))
}

fn control_job(id: Uuid, action: &str) -> UnifiedResult<DownloadJobView> {
    let job = find_job(id)?;
    let status = job.status();
    match action {
        "pause" => {
            if status != DownloadStatus::Downloading && status != DownloadStatus::Queued {
                return Err(UnifiedError::conflict(format!(
                    "Cannot pause a download job in status \"{status}\""
                )));
            }
            job.paused.store(true, Ordering::SeqCst);
            if !job.set_status(DownloadStatus::Paused) {
                // The job finished between the status check and the
                // transition: roll the flag back and refuse with a
                // 409 instead of returning 200 with a snapshot
                // showing the terminal status as if the pause
                // applied.
                job.paused.store(false, Ordering::SeqCst);
                return Err(UnifiedError::conflict(format!(
                    "Cannot pause a download job in status \"{}\"",
                    job.status()
                )));
            }
        }
        "resume" => {
            if status != DownloadStatus::Paused {
                return Err(UnifiedError::conflict(format!(
                    "Cannot resume a download job in status \"{status}\""
                )));
            }
            job.paused.store(false, Ordering::SeqCst);
            if !job.set_status(DownloadStatus::Downloading) {
                job.paused.store(true, Ordering::SeqCst);
                return Err(UnifiedError::conflict(format!(
                    "Cannot resume a download job in status \"{}\"",
                    job.status()
                )));
            }
        }
        "cancel" => {
            if status.is_finished() {
                return Err(UnifiedError::conflict(format!(
                    "Cannot cancel a download job in status \"{status}\""
                )));
            }
            let was_paused = job.paused.load(Ordering::SeqCst);
            job.cancelled.store(true, Ordering::SeqCst);
            job.paused.store(false, Ordering::SeqCst);
            if !job.set_status(DownloadStatus::Cancelled) {
                job.cancelled.store(false, Ordering::SeqCst);
                job.paused.store(was_paused, Ordering::SeqCst);
                return Err(UnifiedError::conflict(format!(
                    "Cannot cancel a download job in status \"{}\"",
                    job.status()
                )));
            }
        }
        _ => return Err(UnifiedError::bad_request("Unknown job action")),
    }
    Ok(job.snapshot())
}

pub(crate) async fn pause_job(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_MANAGE)?;
        control_job(id.into_inner(), "pause")
    })();
    json::respond(result, debug)
}

pub(crate) async fn resume_job(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_MANAGE)?;
        control_job(id.into_inner(), "resume")
    })();
    json::respond(result, debug)
}

pub(crate) async fn cancel_job(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_MANAGE)?;
        control_job(id.into_inner(), "cancel")
    })();
    json::respond(result, debug)
}

/// Reset a failed job's per-attempt snapshot fields before a manual
/// retry, so the retried job does not carry the previous attempt's
/// start time, total size, downloaded bytes, or last speed into its
/// new duration and progress.
fn reset_job_for_retry(job: &DownloadJobState) {
    *job.error.write().expect("job lock poisoned") = None;
    *job.finished_at.write().expect("job lock poisoned") = None;
    *job.checksum_ok.write().expect("job lock poisoned") = None;
    *job.started_at.write().expect("job lock poisoned") = None;
    *job.total_bytes.write().expect("job lock poisoned") = None;
    job.downloaded_bytes.store(0, Ordering::SeqCst);
    job.speed_bytes_per_sec.store(0, Ordering::SeqCst);
}

pub(crate) async fn retry_job(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOWNLOADER_MANAGE)?;
        let job = find_job(id.into_inner())?;
        let status = job.status();
        if status != DownloadStatus::Failed {
            return Err(UnifiedError::conflict(format!(
                "Cannot retry a download job in status \"{status}\""
            )));
        }
        reset_job_for_retry(&job);
        job.retry_count.store(0, Ordering::SeqCst);
        job.cancelled.store(false, Ordering::SeqCst);
        job.paused.store(false, Ordering::SeqCst);
        if !job.set_status(DownloadStatus::Downloading) {
            return Err(UnifiedError::conflict(
                "Cannot retry a download job in its current status",
            ));
        }
        let view = job.snapshot();
        let ctx_clone: Arc<AppContext> = Arc::clone(&ctx);
        tokio::spawn(async move { run_download(ctx_clone, job).await });
        Ok(view)
    })();
    json::respond(result, debug)
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
    use actix_web::test::TestRequest;

    fn test_base() -> PathBuf {
        let base = std::env::temp_dir().join("unified-downloader-test");
        std::env::set_var("DOWNLOADER_DOWNLOAD_DIR", &base);
        base
    }

    fn test_url() -> url::Url {
        url::Url::parse("https://example.com/files/archive.zip").unwrap()
    }

    fn test_job() -> Arc<DownloadJobState> {
        let now = Utc::now();
        Arc::new(DownloadJobState {
            id: Uuid::now_v7(),
            url: "https://example.com/files/archive.zip".to_string(),
            destination: PathBuf::from("archive.zip"),
            part_path: PathBuf::from("archive.zip.part"),
            status: RwLock::new(DownloadStatus::Queued),
            downloaded_bytes: AtomicU64::new(0),
            total_bytes: RwLock::new(None),
            speed_bytes_per_sec: AtomicU64::new(0),
            checksum: None,
            checksum_algorithm: None,
            checksum_ok: RwLock::new(None),
            error: RwLock::new(None),
            created_at: now,
            updated_at: RwLock::new(now),
            started_at: RwLock::new(None),
            finished_at: RwLock::new(None),
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            retry_count: AtomicU32::new(0),
            max_retries: 3,
            resumable: RwLock::new(None),
            etag: RwLock::new(None),
            last_modified: RwLock::new(None),
            max_bytes: None,
            client: reqwest::Client::new(),
        })
    }

    #[test]
    fn resolve_destination_accepts_relative_paths() {
        let base = test_base();
        let dest = resolve_destination("archive.zip", &test_url()).unwrap();
        assert_eq!(dest, base.canonicalize().unwrap().join("archive.zip"));
        let dest = resolve_destination("sub/dir/archive.zip", &test_url()).unwrap();
        assert_eq!(
            dest,
            base.canonicalize().unwrap().join("sub/dir/archive.zip")
        );
    }

    #[test]
    fn resolve_destination_rejects_absolute_and_traversal() {
        test_base();
        assert!(resolve_destination("/etc/passwd", &test_url()).is_err());
        assert!(resolve_destination("../escape.zip", &test_url()).is_err());
        assert!(resolve_destination("sub/../../escape.zip", &test_url()).is_err());
        assert!(resolve_destination("   ", &test_url()).is_err());
    }

    #[test]
    fn resolve_destination_appends_file_name_for_directories() {
        let base = test_base();
        let dest = resolve_destination("sub/", &test_url()).unwrap();
        assert_eq!(dest, base.canonicalize().unwrap().join("sub/archive.zip"));
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = verified_request("user-1", "admin");
        let call = caller(&req).unwrap();
        assert_eq!(call.actor, test_user_uuid("user-1").to_string());
        assert_eq!(call.role, "admin");
    }

    #[test]
    fn control_job_pause_on_terminal_is_conflict() {
        let job = test_job();
        jobs_store().insert(job.id, Arc::clone(&job));
        assert!(job.set_status(DownloadStatus::Downloading));
        assert!(job.set_status(DownloadStatus::Completed));
        let error = control_job(job.id, "pause").unwrap_err();
        assert_eq!(error.code, 409);
        assert!(!job.paused.load(Ordering::SeqCst));
        jobs_store().remove(&job.id);
    }

    #[test]
    fn control_job_pause_then_resume_round_trip() {
        let job = test_job();
        jobs_store().insert(job.id, Arc::clone(&job));
        let view = control_job(job.id, "pause").unwrap();
        assert_eq!(view.status, "paused");
        assert!(job.paused.load(Ordering::SeqCst));
        let view = control_job(job.id, "resume").unwrap();
        assert_eq!(view.status, "downloading");
        assert!(!job.paused.load(Ordering::SeqCst));
        jobs_store().remove(&job.id);
    }

    #[test]
    fn control_job_race_never_reports_success_for_terminal_job() {
        // One thread keeps trying to complete the job while the
        // main thread pauses it: whichever wins, a pause that
        // returns Ok must report the job Paused, never terminal.
        for _ in 0..50 {
            let job = test_job();
            jobs_store().insert(job.id, Arc::clone(&job));
            assert!(job.set_status(DownloadStatus::Downloading));
            let racer = Arc::clone(&job);
            std::thread::scope(|scope| {
                scope.spawn(move || {
                    let _ = racer.set_status(DownloadStatus::Completed);
                });
                // Assert on the returned snapshot, not the live job:
                // a racer may still complete the job after control
                // returns, but the snapshot control_job hands back
                // must never claim a terminal job was paused.
                match control_job(job.id, "pause") {
                    Ok(view) => assert_eq!(view.status, "paused"),
                    Err(error) => assert_eq!(error.code, 409),
                }
            });
            jobs_store().remove(&job.id);
        }
    }

    #[test]
    fn reset_job_for_retry_clears_stale_snapshot_fields() {
        let job = test_job();
        *job.error.write().unwrap() = Some("boom".to_string());
        *job.finished_at.write().unwrap() = Some(Utc::now());
        *job.checksum_ok.write().unwrap() = Some(false);
        *job.started_at.write().unwrap() = Some(Utc::now());
        *job.total_bytes.write().unwrap() = Some(4096);
        job.downloaded_bytes.store(2048, Ordering::SeqCst);
        job.speed_bytes_per_sec.store(1234, Ordering::SeqCst);
        reset_job_for_retry(&job);
        assert!(job.error.read().unwrap().is_none());
        assert!(job.finished_at.read().unwrap().is_none());
        assert!(job.checksum_ok.read().unwrap().is_none());
        assert!(job.started_at.read().unwrap().is_none());
        assert!(job.total_bytes.read().unwrap().is_none());
        assert_eq!(job.downloaded_bytes.load(Ordering::SeqCst), 0);
        assert_eq!(job.speed_bytes_per_sec.load(Ordering::SeqCst), 0);
        // The snapshot no longer spans the original attempt.
        assert_eq!(job.snapshot().duration_seconds, None);
        assert_eq!(job.snapshot().downloaded_bytes, 0);
        assert_eq!(job.snapshot().speed_bytes_per_sec, 0);
    }

    /// Build a request carrying a resolved tenant context plus a
    /// verified caller, for the tenant-gate tests below.
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        // No tenant company: the verified caller's company is the scope.
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &uuid::Uuid::now_v7().to_string(),
            Some(&uuid::Uuid::now_v7().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), company);
    }
}
