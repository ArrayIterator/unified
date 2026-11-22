// Downloader module — file downloads with progress, pause / resume /
// cancel, HTTP Range resume, and checksum verification.
//
// Each job runs in a background tokio task that streams the response body
// to disk with reqwest, so the HTTP request that created the job returns
// immediately. Progress (bytes downloaded / total / speed) is kept in
// atomics on the shared job state. `download.started` is emitted when a
// job starts and `download.finished` when it completes or fails.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Digest as Sha1Digest;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Job state.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DownloadJobView {
    pub id: Uuid,
    pub url: String,
    pub destination: String,
    pub status: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub speed_bytes_per_sec: u64,
    pub checksum: Option<String>,
    pub checksum_algorithm: Option<String>,
    pub checksum_ok: Option<bool>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct DownloadJobState {
    pub id: Uuid,
    pub url: String,
    pub destination: PathBuf,
    pub status: RwLock<String>,
    pub downloaded_bytes: AtomicU64,
    pub total_bytes: RwLock<Option<u64>>,
    pub speed_bytes_per_sec: AtomicU64,
    pub checksum: Option<String>,
    pub checksum_algorithm: Option<String>,
    pub checksum_ok: RwLock<Option<bool>>,
    pub error: RwLock<Option<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: RwLock<DateTime<Utc>>,
    pub paused: AtomicBool,
    pub cancelled: AtomicBool,
}

impl DownloadJobState {
    fn snapshot(&self) -> DownloadJobView {
        DownloadJobView {
            id: self.id,
            url: self.url.clone(),
            destination: self.destination.to_string_lossy().into_owned(),
            status: self.status.read().expect("job lock poisoned").clone(),
            downloaded_bytes: self.downloaded_bytes.load(Ordering::SeqCst),
            total_bytes: *self.total_bytes.read().expect("job lock poisoned"),
            speed_bytes_per_sec: self.speed_bytes_per_sec.load(Ordering::SeqCst),
            checksum: self.checksum.clone(),
            checksum_algorithm: self.checksum_algorithm.clone(),
            checksum_ok: *self.checksum_ok.read().expect("job lock poisoned"),
            error: self.error.read().expect("job lock poisoned").clone(),
            created_at: self.created_at,
            updated_at: *self.updated_at.read().expect("job lock poisoned"),
        }
    }

    fn set_status(&self, status: &str) {
        *self.status.write().expect("job lock poisoned") = status.to_string();
        *self.updated_at.write().expect("job lock poisoned") = Utc::now();
    }

    fn set_error(&self, message: String) {
        *self.error.write().expect("job lock poisoned") = Some(message);
    }
}

static JOBS: OnceLock<DashMap<Uuid, Arc<DownloadJobState>>> = OnceLock::new();

fn jobs_store() -> &'static DashMap<Uuid, Arc<DownloadJobState>> {
    JOBS.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    url: String,
    destination: String,
    /// Optional checksum to verify after the download, for example
    /// `"sha256:<hex>"`, `"sha1:<hex>"`, or a bare hex digest (sha256).
    #[serde(default)]
    checksum: Option<String>,
}

fn parse_checksum(input: &str) -> UnifiedResult<(String, String)> {
    let input = input.trim().to_lowercase();
    let (algorithm, hex) = match input.split_once(':') {
        Some((algorithm, hex)) => (algorithm.to_string(), hex.to_string()),
        None => ("sha256".to_string(), input),
    };
    if !matches!(algorithm.as_str(), "sha256" | "sha1") {
        return Err(UnifiedError::bad_request(
            "Unsupported checksum algorithm (expected sha256 or sha1)",
        ));
    }
    let expected_len = if algorithm == "sha256" { 64 } else { 40 };
    if hex.len() != expected_len || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid {algorithm} checksum: expected {expected_len} hex characters"
        )));
    }
    Ok((algorithm, hex))
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

// ---------------------------------------------------------------------------
// Background download worker.
// ---------------------------------------------------------------------------

async fn run_download(ctx: Arc<AppContext>, job: Arc<DownloadJobState>) {
    ctx.hooks.emit(
        events::DOWNLOAD_STARTED,
        serde_json::json!({
            "job_id": job.id,
            "url": job.url,
            "destination": job.destination.to_string_lossy(),
        }),
    );
    job.set_status("downloading");
    let outcome = download_once(&job).await;
    match outcome {
        Ok(()) => {
            job.set_status("completed");
            ctx.hooks.emit(
                events::DOWNLOAD_FINISHED,
                serde_json::json!({
                    "job_id": job.id,
                    "url": job.url,
                    "destination": job.destination.to_string_lossy(),
                    "success": true,
                    "downloaded_bytes": job.downloaded_bytes.load(Ordering::SeqCst),
                }),
            );
        }
        Err(error) => {
            if job.cancelled.load(Ordering::SeqCst) {
                job.set_status("cancelled");
            } else {
                job.set_status("failed");
                job.set_error(error.message.clone());
            }
            ctx.hooks.emit(
                events::DOWNLOAD_FINISHED,
                serde_json::json!({
                    "job_id": job.id,
                    "url": job.url,
                    "destination": job.destination.to_string_lossy(),
                    "success": false,
                    "error": error.message,
                }),
            );
        }
    }
}

async fn download_once(job: &DownloadJobState) -> UnifiedResult<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;

    // Resume: when a partial file exists, ask for the remaining bytes.
    let existing_len = std::fs::metadata(&job.destination)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let mut request = client.get(job.url.clone());
    if existing_len > 0 {
        request = request.header("Range", format!("bytes={existing_len}-"));
    }
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Download request failed: {error}"))
    })?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // The server says the range is already complete.
        job.downloaded_bytes.store(existing_len, Ordering::SeqCst);
        *job.total_bytes.write().expect("job lock poisoned") = Some(existing_len);
        return verify_checksum(job);
    }
    if !status.is_success() {
        return Err(UnifiedError::internal(format!(
            "Download failed with HTTP {status}"
        )));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT && existing_len > 0;
    let start_offset = if resumed { existing_len } else { 0 };
    if !resumed && existing_len > 0 {
        // The server ignored the Range request: start from scratch.
        job.downloaded_bytes.store(0, Ordering::SeqCst);
    } else {
        job.downloaded_bytes.store(start_offset, Ordering::SeqCst);
    }
    let total = response
        .content_length()
        .map(|len| len + start_offset);
    *job.total_bytes.write().expect("job lock poisoned") = total;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&job.destination)
        .map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot open destination {}: {error}",
                job.destination.display()
            ))
        })?;

    let mut sha256: Option<sha2::Sha256> = None;
    let mut sha1: Option<sha1::Sha1> = None;
    match job.checksum_algorithm.as_deref() {
        Some("sha256") => sha256 = Some(sha2::Sha256::new()),
        Some("sha1") => sha1 = Some(sha1::Sha1::new()),
        _ => {}
    }
    // When resuming, the checksum must cover the whole file, so hash the
    // existing prefix first.
    if resumed {
        if let Some(hasher) = sha256.as_mut() {
            let prefix = std::fs::read(&job.destination).map_err(|error| {
                UnifiedError::internal(format!("Cannot read partial file for checksum: {error}"))
            })?;
            // Only the bytes before the current write position belong to
            // the prefix; the file was opened in append mode, so read the
            // first `start_offset` bytes.
            use sha2::Digest;
            hasher.update(&prefix[..start_offset as usize]);
        }
        if let Some(hasher) = sha1.as_mut() {
            let prefix = std::fs::read(&job.destination).map_err(|error| {
                UnifiedError::internal(format!("Cannot read partial file for checksum: {error}"))
            })?;
            hasher.update(&prefix[..start_offset as usize]);
        }
    }

    let started = Instant::now();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if job.cancelled.load(Ordering::SeqCst) {
            return Err(UnifiedError::internal("Download cancelled"));
        }
        while job.paused.load(Ordering::SeqCst) {
            if job.cancelled.load(Ordering::SeqCst) {
                return Err(UnifiedError::internal("Download cancelled"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let chunk = chunk.map_err(|error| {
            UnifiedError::internal(format!("Download stream failed: {error}"))
        })?;
        file.write_all(&chunk).map_err(|error| {
            UnifiedError::internal(format!("Cannot write download: {error}"))
        })?;
        if let Some(hasher) = sha256.as_mut() {
            use sha2::Digest;
            hasher.update(&chunk);
        }
        if let Some(hasher) = sha1.as_mut() {
            hasher.update(&chunk);
        }
        let downloaded = job
            .downloaded_bytes
            .fetch_add(chunk.len() as u64, Ordering::SeqCst)
            + chunk.len() as u64;
        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        job.speed_bytes_per_sec.store(
            (downloaded as f64 / elapsed) as u64,
            Ordering::SeqCst,
        );
    }
    drop(file);

    // Finalise the checksum from the streaming hashers.
    if let Some(hasher) = sha256 {
        use sha2::Digest;
        let digest = hex_encode(&hasher.finalize());
        finish_checksum(job, &digest)?;
    }
    if let Some(hasher) = sha1 {
        let digest = hex_encode(&hasher.finalize());
        finish_checksum(job, &digest)?;
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn finish_checksum(job: &DownloadJobState, digest: &str) -> UnifiedResult<()> {
    let expected = job
        .checksum
        .as_ref()
        .map(|value| value.trim().to_lowercase());
    let ok = match expected {
        Some(expected) => {
            let matches = digest == expected;
            *job.checksum_ok.write().expect("job lock poisoned") = Some(matches);
            matches
        }
        None => true,
    };
    if !ok {
        return Err(UnifiedError::internal(format!(
            "Checksum mismatch for {}: expected {}, got {digest}",
            job.destination.display(),
            expected.unwrap_or_default()
        )));
    }
    Ok(())
}

/// Verify the checksum of a file that is already fully on disk (used when
/// the server reports the requested range as already satisfied).
fn verify_checksum(job: &DownloadJobState) -> UnifiedResult<()> {
    let Some(expected) = job.checksum.clone() else {
        return Ok(());
    };
    let bytes = std::fs::read(&job.destination).map_err(|error| {
        UnifiedError::internal(format!("Cannot read downloaded file: {error}"))
    })?;
    let digest = match job.checksum_algorithm.as_deref() {
        Some("sha1") => {
            let mut hasher = sha1::Sha1::new();
            hasher.update(&bytes);
            hex_encode(&hasher.finalize())
        }
        _ => {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(&bytes);
            hex_encode(&hasher.finalize())
        }
    };
    finish_checksum(job, &digest)
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn create_job(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateJobRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let url = url::Url::parse(body.url.trim()).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid download URL \"{}\"", body.url))
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Download URL must use http or https",
            ));
        }
        let (algorithm, checksum) = match &body.checksum {
            Some(value) => {
                let (algorithm, hex) = parse_checksum(value)?;
                (Some(algorithm), Some(hex))
            }
            None => (None, None),
        };
        let mut destination = PathBuf::from(body.destination.trim());
        if destination.as_os_str().is_empty() {
            return Err(UnifiedError::bad_request("destination is required"));
        }
        let is_dir_hint = body.destination.ends_with('/')
            || body.destination.ends_with(std::path::MAIN_SEPARATOR);
        if is_dir_hint || destination.is_dir() {
            let file_name = url
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty())
                .unwrap_or("download");
            destination.push(file_name);
        }
        if let Some(parent) = destination.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot create destination directory {}: {error}",
                        parent.display()
                    ))
                })?;
            }
        }
        let now = Utc::now();
        let job = Arc::new(DownloadJobState {
            id: Uuid::now_v7(),
            url: url.to_string(),
            destination,
            status: RwLock::new("queued".to_string()),
            downloaded_bytes: AtomicU64::new(0),
            total_bytes: RwLock::new(None),
            speed_bytes_per_sec: AtomicU64::new(0),
            checksum,
            checksum_algorithm: algorithm,
            checksum_ok: RwLock::new(None),
            error: RwLock::new(None),
            created_at: now,
            updated_at: RwLock::new(now),
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
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

async fn list_jobs(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn get_job(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().snapshot())
            .ok_or_else(|| UnifiedError::not_found("Download job not found"))
    })();
    json::respond(result, debug)
}

fn control_job(id: Uuid, action: &str) -> UnifiedResult<DownloadJobView> {
    let job = jobs_store()
        .get(&id)
        .map(|entry| Arc::clone(entry.value()))
        .ok_or_else(|| UnifiedError::not_found("Download job not found"))?;
    let status = job.status.read().expect("job lock poisoned").clone();
    match action {
        "pause" => {
            if status != "downloading" && status != "queued" {
                return Err(UnifiedError::conflict(format!(
                    "Cannot pause a download job in status \"{status}\""
                )));
            }
            job.paused.store(true, Ordering::SeqCst);
            job.set_status("paused");
        }
        "resume" => {
            if status != "paused" {
                return Err(UnifiedError::conflict(format!(
                    "Cannot resume a download job in status \"{status}\""
                )));
            }
            job.paused.store(false, Ordering::SeqCst);
            job.set_status("downloading");
        }
        "cancel" => {
            if status == "completed" || status == "cancelled" || status == "failed" {
                return Err(UnifiedError::conflict(format!(
                    "Cannot cancel a download job in status \"{status}\""
                )));
            }
            job.cancelled.store(true, Ordering::SeqCst);
            job.paused.store(false, Ordering::SeqCst);
            job.set_status("cancelled");
        }
        _ => return Err(UnifiedError::bad_request("Unknown job action")),
    }
    Ok(job.snapshot())
}

async fn pause_job(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(control_job(id.into_inner(), "pause"), debug)
}

async fn resume_job(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(control_job(id.into_inner(), "resume"), debug)
}

async fn cancel_job(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(control_job(id.into_inner(), "cancel"), debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct DownloaderModule;

impl Module for DownloaderModule {
    fn id(&self) -> &'static str {
        "downloader"
    }

    fn name(&self) -> &'static str {
        "File Downloader"
    }

    fn description(&self) -> &'static str {
        "Streaming file downloads with progress, pause/resume via HTTP Range, cancellation, and checksum verification."
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/downloader")
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}/pause", web::post().to(pause_job))
                .route("/jobs/{id}/resume", web::post().to(resume_job))
                .route("/jobs/{id}/cancel", web::post().to(cancel_job)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DownloaderModule));
}
