// Downloader module — file downloads with progress, pause / resume /
// cancel / retry, HTTP Range resume with ETag / Last-Modified validation,
// and checksum verification.
//
// Each job runs in a background tokio task that streams the response body
// to a `<destination>.part` file with reqwest, so the HTTP request that
// created the job returns immediately. The partial file is renamed to
// the destination only after the download and its checksum verify, so a
// failed or cancelled download never leaves a corrupt file at the final
// path. Progress (bytes downloaded / total / speed / ETA) is kept in
// atomics on the shared job state. `download.started` is emitted when a
// job starts and `download.finished` when it completes or fails.
//
// Retry behaviour: transient failures (connection errors, timeouts,
// HTTP 408 / 429 / 5xx) are retried with exponential backoff, resuming
// from the partial file when the server supports it. Permanent failures
// (other 4xx responses, checksum mismatch after the final attempt,
// oversize) fail the job immediately. A failed job can be retried
// manually through `POST /jobs/{id}/retry`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Limits (clamped the same way for every job).
// ---------------------------------------------------------------------------

/// Default number of retries after a transient failure.
const DEFAULT_MAX_RETRIES: u32 = 3;
/// Hard cap on retries per job.
const MAX_MAX_RETRIES: u32 = 10;
/// Default total request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
/// Total request timeout bounds in seconds.
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 3600;
/// Default connect timeout in seconds.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Connect timeout bounds in seconds.
const MIN_CONNECT_TIMEOUT_SECS: u64 = 1;
const MAX_CONNECT_TIMEOUT_SECS: u64 = 60;
/// Default and maximum number of redirects to follow.
const DEFAULT_MAX_REDIRECTS: u32 = 10;
const MAX_MAX_REDIRECTS: u32 = 10;
/// Base delay before the first retry; doubled per attempt, capped.
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
/// Longest `Retry-After` delay honoured from the server.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Default `User-Agent` sent with download requests.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

// ---------------------------------------------------------------------------
// Job status: a small state machine with validated transitions.
// ---------------------------------------------------------------------------

/// Lifecycle of a download job. Terminal states (`Completed`, `Failed`,
/// `Cancelled`) accept no further transition, except that a `Failed` job
/// may return to `Downloading` through an explicit manual retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn can_transition_to(&self, next: Self) -> bool {
        match self {
            Self::Queued => matches!(
                next,
                Self::Downloading | Self::Paused | Self::Cancelled
            ),
            Self::Downloading => matches!(
                next,
                Self::Paused | Self::Completed | Self::Failed | Self::Cancelled
            ),
            Self::Paused => matches!(
                next,
                Self::Downloading | Self::Completed | Self::Failed | Self::Cancelled
            ),
            Self::Failed => matches!(next, Self::Downloading | Self::Cancelled),
            Self::Completed | Self::Cancelled => false,
        }
    }
}

impl std::fmt::Display for DownloadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    pub progress_percent: Option<f64>,
    pub speed_bytes_per_sec: u64,
    pub eta_seconds: Option<u64>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub resumable: Option<bool>,
    pub checksum: Option<String>,
    pub checksum_algorithm: Option<String>,
    pub checksum_ok: Option<bool>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_seconds: Option<u64>,
}

pub struct DownloadJobState {
    pub id: Uuid,
    pub url: String,
    pub destination: PathBuf,
    /// Partial download; renamed to `destination` only on success.
    pub part_path: PathBuf,
    pub status: RwLock<DownloadStatus>,
    pub downloaded_bytes: AtomicU64,
    pub total_bytes: RwLock<Option<u64>>,
    pub speed_bytes_per_sec: AtomicU64,
    pub checksum: Option<String>,
    pub checksum_algorithm: Option<String>,
    pub checksum_ok: RwLock<Option<bool>>,
    pub error: RwLock<Option<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: RwLock<DateTime<Utc>>,
    pub started_at: RwLock<Option<DateTime<Utc>>>,
    pub finished_at: RwLock<Option<DateTime<Utc>>>,
    pub paused: AtomicBool,
    pub cancelled: AtomicBool,
    pub retry_count: AtomicU32,
    pub max_retries: u32,
    /// Whether the server advertises byte-range support (`Accept-Ranges:
    /// bytes`) or honoured a Range request. `None` until the first
    /// response arrives.
    pub resumable: RwLock<Option<bool>>,
    /// Validators captured from the first response, sent back as
    /// `If-Range` on resume so a changed remote file restarts the
    /// download instead of producing a corrupt mix.
    pub etag: RwLock<Option<String>>,
    pub last_modified: RwLock<Option<String>>,
    pub max_bytes: Option<u64>,
    /// One client per job, reused across retry attempts.
    pub client: reqwest::Client,
}

impl DownloadJobState {
    fn status(&self) -> DownloadStatus {
        *self.status.read().expect("job lock poisoned")
    }

    fn snapshot(&self) -> DownloadJobView {
        let downloaded = self.downloaded_bytes.load(Ordering::SeqCst);
        let total = *self.total_bytes.read().expect("job lock poisoned");
        let speed = self.speed_bytes_per_sec.load(Ordering::SeqCst);
        let progress_percent = total
            .filter(|total| *total > 0)
            .map(|total| (downloaded as f64 / total as f64) * 100.0);
        let eta_seconds = match (total, speed) {
            (Some(total), speed) if total > downloaded && speed > 0 => {
                Some((total - downloaded) / speed)
            }
            _ => None,
        };
        let started_at = *self.started_at.read().expect("job lock poisoned");
        let finished_at = *self.finished_at.read().expect("job lock poisoned");
        let duration_seconds = match (started_at, finished_at) {
            (Some(started), Some(finished)) => Some(
                finished
                    .signed_duration_since(started)
                    .num_seconds()
                    .max(0) as u64,
            ),
            _ => None,
        };
        DownloadJobView {
            id: self.id,
            url: self.url.clone(),
            destination: self.destination.to_string_lossy().into_owned(),
            status: self.status().as_str().to_string(),
            downloaded_bytes: downloaded,
            total_bytes: total,
            progress_percent,
            speed_bytes_per_sec: speed,
            eta_seconds,
            retry_count: self.retry_count.load(Ordering::SeqCst),
            max_retries: self.max_retries,
            resumable: *self.resumable.read().expect("job lock poisoned"),
            checksum: self.checksum.clone(),
            checksum_algorithm: self.checksum_algorithm.clone(),
            checksum_ok: *self.checksum_ok.read().expect("job lock poisoned"),
            error: self.error.read().expect("job lock poisoned").clone(),
            created_at: self.created_at,
            updated_at: *self.updated_at.read().expect("job lock poisoned"),
            started_at,
            finished_at,
            duration_seconds,
        }
    }

    /// Attempt a status transition. Returns `false` when the transition
    /// is illegal from the current state; the state is left untouched.
    fn set_status(&self, status: DownloadStatus) -> bool {
        let mut current = self.status.write().expect("job lock poisoned");
        if !current.can_transition_to(status) {
            return false;
        }
        *current = status;
        *self.updated_at.write().expect("job lock poisoned") = Utc::now();
        if status.is_finished() {
            *self.finished_at.write().expect("job lock poisoned") = Some(Utc::now());
        }
        true
    }

    fn set_error(&self, message: String) {
        *self.error.write().expect("job lock poisoned") = Some(message);
    }

    fn mark_started(&self) {
        let mut started_at = self.started_at.write().expect("job lock poisoned");
        if started_at.is_none() {
            *started_at = Some(Utc::now());
        }
    }

    /// The `If-Range` validator for a resume request: the ETag when the
    /// server sent one, otherwise the `Last-Modified` timestamp.
    fn if_range_value(&self) -> Option<String> {
        self.etag
            .read()
            .expect("job lock poisoned")
            .clone()
            .or_else(|| {
                self.last_modified
                    .read()
                    .expect("job lock poisoned")
                    .clone()
            })
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
    /// `"sha256:<hex>"`, `"sha512:<hex>"`, `"sha1:<hex>"`, or a bare hex
    /// digest (sha256).
    #[serde(default)]
    checksum: Option<String>,
    /// Retries after transient failures (0..=10, default 3).
    #[serde(default = "default_max_retries")]
    max_retries: u32,
    /// Total request timeout in seconds (5..=3600, default 3600).
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
    /// Connect timeout in seconds (1..=60, default 10).
    #[serde(default = "default_connect_timeout_secs")]
    connect_timeout_secs: u64,
    /// Redirects to follow (0..=10, default 10).
    #[serde(default = "default_max_redirects")]
    max_redirects: u32,
    /// Follow redirects (default true).
    #[serde(default = "default_follow_redirect")]
    follow_redirect: bool,
    /// Maximum download size in bytes. Exceeding it fails the job.
    /// `None` (default) means unlimited.
    #[serde(default)]
    max_bytes: Option<u64>,
}

fn default_max_retries() -> u32 {
    DEFAULT_MAX_RETRIES
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

fn default_connect_timeout_secs() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_SECS
}

fn default_max_redirects() -> u32 {
    DEFAULT_MAX_REDIRECTS
}

fn default_follow_redirect() -> bool {
    true
}

fn parse_checksum(input: &str) -> UnifiedResult<(String, String)> {
    let input = input.trim().to_lowercase();
    let (algorithm, hex) = match input.split_once(':') {
        Some((algorithm, hex)) => (algorithm.to_string(), hex.to_string()),
        None => ("sha256".to_string(), input),
    };
    if !matches!(algorithm.as_str(), "sha256" | "sha512" | "sha1") {
        return Err(UnifiedError::bad_request(
            "Unsupported checksum algorithm (expected sha256, sha512 or sha1)",
        ));
    }
    let expected_len = match algorithm.as_str() {
        "sha256" => 64,
        "sha512" => 128,
        _ => 40,
    };
    if hex.len() != expected_len || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid {algorithm} checksum: expected {expected_len} hex characters"
        )));
    }
    Ok((algorithm, hex))
}

fn build_client(request: &CreateJobRequest) -> UnifiedResult<reqwest::Client> {
    let max_redirects = request.max_redirects.min(MAX_MAX_REDIRECTS);
    let redirect = if request.follow_redirect && max_redirects > 0 {
        reqwest::redirect::Policy::limited(max_redirects as usize)
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(
            request.timeout_secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS),
        ))
        .connect_timeout(Duration::from_secs(
            request
                .connect_timeout_secs
                .clamp(MIN_CONNECT_TIMEOUT_SECS, MAX_CONNECT_TIMEOUT_SECS),
        ))
        .redirect(redirect)
        .tcp_nodelay(true)
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
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
// Checksums.
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Constant-time digest comparison, so a checksum check does not leak
/// how many leading characters matched.
fn digests_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Streaming hashers for the requested checksum algorithm. Only the
/// requested algorithm is computed.
struct ChecksumHashers {
    sha256: Option<sha2::Sha256>,
    sha512: Option<sha2::Sha512>,
    sha1: Option<sha1::Sha1>,
}

impl ChecksumHashers {
    fn for_algorithm(algorithm: Option<&str>) -> Self {
        match algorithm {
            Some("sha256") => {
                use sha2::Digest;
                Self {
                    sha256: Some(sha2::Sha256::new()),
                    sha512: None,
                    sha1: None,
                }
            }
            Some("sha512") => {
                use sha2::Digest;
                Self {
                    sha256: None,
                    sha512: Some(sha2::Sha512::new()),
                    sha1: None,
                }
            }
            Some("sha1") => {
                use sha1::Digest;
                Self {
                    sha256: None,
                    sha512: None,
                    sha1: Some(sha1::Sha1::new()),
                }
            }
            _ => Self {
                sha256: None,
                sha512: None,
                sha1: None,
            },
        }
    }

    fn is_active(&self) -> bool {
        self.sha256.is_some() || self.sha512.is_some() || self.sha1.is_some()
    }

    fn update(&mut self, chunk: &[u8]) {
        if let Some(hasher) = self.sha256.as_mut() {
            use sha2::Digest;
            hasher.update(chunk);
        }
        if let Some(hasher) = self.sha512.as_mut() {
            use sha2::Digest;
            hasher.update(chunk);
        }
        if let Some(hasher) = self.sha1.as_mut() {
            use sha1::Digest;
            hasher.update(chunk);
        }
    }

    /// Finalize the hash and compare it with the expected digest. On a
    /// mismatch the partial file is removed (its bytes are wrong, so a
    /// resume would only re-download the same wrong content) and a
    /// retryable error is returned.
    fn finish(self, job: &DownloadJobState) -> Result<(), AttemptError> {
        let expected = job
            .checksum
            .as_ref()
            .map(|value| value.trim().to_lowercase());
        let Some(expected) = expected else {
            return Ok(());
        };
        let digest = if let Some(hasher) = self.sha256 {
            use sha2::Digest;
            hex_encode(&hasher.finalize())
        } else if let Some(hasher) = self.sha512 {
            use sha2::Digest;
            hex_encode(&hasher.finalize())
        } else if let Some(hasher) = self.sha1 {
            use sha1::Digest;
            hex_encode(&hasher.finalize())
        } else {
            return Ok(());
        };
        let matches = digests_equal(&digest, &expected);
        *job.checksum_ok.write().expect("job lock poisoned") = Some(matches);
        if !matches {
            let _ = std::fs::remove_file(&job.part_path);
            return Err(AttemptError::retryable(format!(
                "Checksum mismatch for {}: expected {expected}, got {digest}",
                job.destination.display()
            )));
        }
        Ok(())
    }
}

/// Stream a file through the hashers in 64 KiB chunks, never loading the
/// whole file into memory. `limit` hashes only the first `limit` bytes
/// (the already-downloaded prefix when resuming); `None` hashes the
/// whole file.
fn hash_file(
    path: &Path,
    limit: Option<u64>,
    hashers: &mut ChecksumHashers,
) -> Result<(), AttemptError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        AttemptError::terminal(format!("Cannot read partial file for checksum: {error}"))
    })?;
    let mut remaining = limit.unwrap_or(u64::MAX);
    let mut buffer = [0u8; 65_536];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..want]).map_err(|error| {
            AttemptError::terminal(format!("Cannot read partial file for checksum: {error}"))
        })?;
        if read == 0 {
            break;
        }
        hashers.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(())
}

/// Verify the checksum of a file that is already fully on disk (used
/// when the server reports the requested range as already satisfied).
fn verify_checksum_file(job: &DownloadJobState) -> Result<(), AttemptError> {
    if job.checksum.is_none() {
        return Ok(());
    }
    let mut hashers = ChecksumHashers::for_algorithm(job.checksum_algorithm.as_deref());
    hash_file(&job.part_path, None, &mut hashers)?;
    hashers.finish(job)
}

// ---------------------------------------------------------------------------
// Retry handling.
// ---------------------------------------------------------------------------

/// Failure of one download attempt. `retryable` failures (connection
/// errors, timeouts, HTTP 408 / 429 / 5xx, checksum mismatch) may be
/// retried; anything else fails the job at once.
struct AttemptError {
    retryable: bool,
    retry_after: Option<Duration>,
    error: UnifiedError,
}

impl AttemptError {
    fn terminal(message: impl Into<String>) -> Self {
        Self {
            retryable: false,
            retry_after: None,
            error: UnifiedError::internal(message),
        }
    }

    fn retryable(message: impl Into<String>) -> Self {
        Self {
            retryable: true,
            retry_after: None,
            error: UnifiedError::internal(message),
        }
    }

    fn retryable_after(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            retryable: true,
            retry_after,
            error: UnifiedError::internal(message),
        }
    }
}

/// Classify a reqwest error: connection problems, timeouts and body /
/// stream errors are transient; anything else (bad URL, redirect policy,
/// decoding) is terminal.
fn classify_reqwest_error(error: reqwest::Error) -> AttemptError {
    if error.is_connect() || error.is_timeout() || error.is_body() || error.is_request() {
        AttemptError::retryable(format!("Download request failed: {error}"))
    } else {
        AttemptError::terminal(format!("Download request failed: {error}"))
    }
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are ignored),
/// capped so a misbehaving server cannot stall a job indefinitely.
fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response
        .headers()
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Exponential backoff for attempt `attempt` (0-based), honouring a
/// server `Retry-After` delay when it is longer.
fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let backoff = RETRY_BASE_DELAY
        .saturating_mul(1u32 << attempt.min(6))
        .min(RETRY_MAX_DELAY);
    match retry_after {
        Some(delay) => backoff.max(delay),
        None => backoff,
    }
}

/// Parse the total size out of a `Content-Range` header
/// (`bytes 100-199/1000` or `bytes */1000`).
fn parse_content_range_total(response: &reqwest::Response) -> Option<u64> {
    let value = response
        .headers()
        .get(header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    let total = value.split('/').nth(1)?;
    if total == "*" {
        return None;
    }
    total.trim().parse::<u64>().ok()
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
    job.mark_started();
    if job.cancelled.load(Ordering::SeqCst) {
        job.set_status(DownloadStatus::Cancelled);
    } else {
        job.set_status(DownloadStatus::Downloading);
        if job.paused.load(Ordering::SeqCst) {
            job.set_status(DownloadStatus::Paused);
        }
    }
    let outcome = run_with_retries(&job).await;
    match outcome {
        Ok(()) => {
            job.set_status(DownloadStatus::Completed);
            ctx.hooks.emit(
                events::DOWNLOAD_FINISHED,
                serde_json::json!({
                    "job_id": job.id,
                    "url": job.url,
                    "destination": job.destination.to_string_lossy(),
                    "success": true,
                    "downloaded_bytes": job.downloaded_bytes.load(Ordering::SeqCst),
                    "retry_count": job.retry_count.load(Ordering::SeqCst),
                }),
            );
        }
        Err(error) => {
            if job.cancelled.load(Ordering::SeqCst) {
                job.set_status(DownloadStatus::Cancelled);
            } else {
                job.set_status(DownloadStatus::Failed);
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
                    "retry_count": job.retry_count.load(Ordering::SeqCst),
                }),
            );
        }
    }
}

/// Run attempts until one succeeds, a terminal error occurs, or the
/// retry budget is exhausted.
async fn run_with_retries(job: &DownloadJobState) -> UnifiedResult<()> {
    let mut attempt: u32 = 0;
    loop {
        match download_attempt(job).await {
            Ok(()) => return Ok(()),
            Err(attempt_error) => {
                if job.cancelled.load(Ordering::SeqCst) {
                    return Err(UnifiedError::internal("Download cancelled"));
                }
                if !attempt_error.retryable || attempt >= job.max_retries {
                    return Err(attempt_error.error);
                }
                let delay = retry_delay(attempt, attempt_error.retry_after);
                job.retry_count.store(attempt + 1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// One download attempt: request (with `Range` / `If-Range` when a
/// partial file exists), stream the body into the `.part` file, and
/// verify the checksum. On success the `.part` file is renamed to the
/// destination.
async fn download_attempt(job: &DownloadJobState) -> Result<(), AttemptError> {
    if job.cancelled.load(Ordering::SeqCst) {
        return Err(AttemptError::terminal("Download cancelled"));
    }
    // A job paused while a previous attempt was backing off must not
    // start a new request until it is resumed.
    while job.paused.load(Ordering::SeqCst) {
        if job.cancelled.load(Ordering::SeqCst) {
            return Err(AttemptError::terminal("Download cancelled"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let existing_len = std::fs::metadata(&job.part_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let mut request = job.client.get(job.url.clone());
    if existing_len > 0 {
        request = request.header(header::RANGE, format!("bytes={existing_len}-"));
        if let Some(if_range) = job.if_range_value() {
            request = request.header(header::IF_RANGE, if_range);
        }
    }
    let response = request
        .send()
        .await
        .map_err(classify_reqwest_error)?;
    let status = response.status();

    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // The server says the range is already complete.
        job.downloaded_bytes
            .store(existing_len, Ordering::SeqCst);
        *job.total_bytes.write().expect("job lock poisoned") = Some(existing_len);
        job.resumable
            .write()
            .expect("job lock poisoned")
            .get_or_insert(true);
        verify_checksum_file(job)?;
        return finalize(job);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
    {
        return Err(AttemptError::retryable_after(
            format!("Download failed with HTTP {status}"),
            parse_retry_after(&response),
        ));
    }
    if !status.is_success() {
        return Err(AttemptError::terminal(format!(
            "Download failed with HTTP {status}"
        )));
    }

    // Record resumability and validators from this response for later
    // attempts.
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT && existing_len > 0;
    let accept_ranges = response
        .headers()
        .get(header::ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    *job.resumable.write().expect("job lock poisoned") =
        Some(accept_ranges.contains("bytes") || resumed);
    if let Some(etag) = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
    {
        *job.etag.write().expect("job lock poisoned") = Some(etag.to_string());
    }
    if let Some(last_modified) = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
    {
        *job.last_modified.write().expect("job lock poisoned") =
            Some(last_modified.to_string());
    }

    // Total size: prefer the `Content-Range` total on a 206 response,
    // otherwise the `Content-Length` plus the resume offset.
    let total = parse_content_range_total(&response)
        .or_else(|| response.content_length().map(|len| len + if resumed { existing_len } else { 0 }));
    if resumed {
        if let Some(total) = total {
            if existing_len > total {
                // The remote file shrank below our partial file: drop the
                // stale partial file and start over on the next attempt.
                let _ = std::fs::remove_file(&job.part_path);
                return Err(AttemptError::retryable(
                    "Partial file is larger than the remote file; restarting from scratch",
                ));
            }
        }
    }
    let start_offset = if resumed { existing_len } else { 0 };
    if !resumed && existing_len > 0 {
        // The server ignored the Range request (or the validators no
        // longer match): start from scratch.
        job.downloaded_bytes.store(0, Ordering::SeqCst);
    } else {
        job.downloaded_bytes
            .store(start_offset, Ordering::SeqCst);
    }
    *job.total_bytes.write().expect("job lock poisoned") = total;
    if let Some(max_bytes) = job.max_bytes {
        if total.map(|total| total > max_bytes).unwrap_or(false) {
            return Err(AttemptError::terminal(format!(
                "Download size exceeds the configured maximum of {max_bytes} bytes"
            )));
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&job.part_path)
        .map_err(|error| {
            AttemptError::terminal(format!(
                "Cannot open partial download {}: {error}",
                job.part_path.display()
            ))
        })?;

    let mut hashers = ChecksumHashers::for_algorithm(job.checksum_algorithm.as_deref());
    // When resuming, the checksum must cover the whole file, so hash the
    // existing prefix first — streamed, never loaded into memory.
    if resumed && hashers.is_active() {
        hash_file(&job.part_path, Some(start_offset), &mut hashers)?;
    }

    let started = Instant::now();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if job.cancelled.load(Ordering::SeqCst) {
            return Err(AttemptError::terminal("Download cancelled"));
        }
        while job.paused.load(Ordering::SeqCst) {
            job.set_status(DownloadStatus::Paused);
            if job.cancelled.load(Ordering::SeqCst) {
                return Err(AttemptError::terminal("Download cancelled"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let chunk = chunk.map_err(classify_reqwest_error)?;
        file.write_all(&chunk).map_err(|error| {
            AttemptError::terminal(format!("Cannot write download: {error}"))
        })?;
        hashers.update(&chunk);
        let downloaded = job
            .downloaded_bytes
            .fetch_add(chunk.len() as u64, Ordering::SeqCst)
            + chunk.len() as u64;
        if let Some(total) = total {
            if downloaded > total {
                return Err(AttemptError::terminal(format!(
                    "Downloaded {downloaded} bytes but the server declared {total}; aborting"
                )));
            }
        }
        if let Some(max_bytes) = job.max_bytes {
            if downloaded > max_bytes {
                return Err(AttemptError::terminal(format!(
                    "Download exceeds the configured maximum of {max_bytes} bytes"
                )));
            }
        }
        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        job.speed_bytes_per_sec.store(
            (downloaded as f64 / elapsed) as u64,
            Ordering::SeqCst,
        );
    }
    // Flush the file to disk before the checksum and the rename.
    file.sync_all().map_err(|error| {
        AttemptError::terminal(format!("Cannot flush download: {error}"))
    })?;
    drop(file);

    hashers.finish(job)?;
    finalize(job)
}

/// Move the verified `.part` file into place at the destination.
fn finalize(job: &DownloadJobState) -> Result<(), AttemptError> {
    std::fs::rename(&job.part_path, &job.destination).map_err(|error| {
        AttemptError::terminal(format!(
            "Cannot move completed download into place at {}: {error}",
            job.destination.display()
        ))
    })?;
    Ok(())
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
            job.set_status(DownloadStatus::Paused);
        }
        "resume" => {
            if status != DownloadStatus::Paused {
                return Err(UnifiedError::conflict(format!(
                    "Cannot resume a download job in status \"{status}\""
                )));
            }
            job.paused.store(false, Ordering::SeqCst);
            job.set_status(DownloadStatus::Downloading);
        }
        "cancel" => {
            if status.is_finished() {
                return Err(UnifiedError::conflict(format!(
                    "Cannot cancel a download job in status \"{status}\""
                )));
            }
            job.cancelled.store(true, Ordering::SeqCst);
            job.paused.store(false, Ordering::SeqCst);
            job.set_status(DownloadStatus::Cancelled);
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

async fn retry_job(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DownloadJobView> = (|| {
        let job = find_job(id.into_inner())?;
        let status = job.status();
        if status != DownloadStatus::Failed {
            return Err(UnifiedError::conflict(format!(
                "Cannot retry a download job in status \"{status}\""
            )));
        }
        *job.error.write().expect("job lock poisoned") = None;
        *job.finished_at.write().expect("job lock poisoned") = None;
        *job.checksum_ok.write().expect("job lock poisoned") = None;
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
        "Streaming file downloads with progress and ETA, pause/resume via HTTP Range with ETag validation, retry with backoff, cancellation, and checksum verification."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/downloader/jobs", owner: "downloader", description: "List jobs" },
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs", owner: "downloader", description: "Create job" },
            RouteInfo { method: "GET", path: "/api/v1/downloader/jobs/{id}", owner: "downloader", description: "Get job" },
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/pause", owner: "downloader", description: "Pause job" },
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/resume", owner: "downloader", description: "Resume job" },
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/cancel", owner: "downloader", description: "Cancel job" },
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/retry", owner: "downloader", description: "Retry job" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/downloader")
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}/pause", web::post().to(pause_job))
                .route("/jobs/{id}/resume", web::post().to(resume_job))
                .route("/jobs/{id}/cancel", web::post().to(cancel_job))
                .route("/jobs/{id}/retry", web::post().to(retry_job)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DownloaderModule));
}
