//! Public data types, request/response shapes and their impls.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Limits (clamped the same way for every job).
// ---------------------------------------------------------------------------

/// Default number of retries after a transient failure.
const DEFAULT_MAX_RETRIES: u32 = 3;
/// Hard cap on retries per job.
pub(crate) const MAX_MAX_RETRIES: u32 = 10;
/// Default total request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
/// Total request timeout bounds in seconds.
pub(crate) const MIN_TIMEOUT_SECS: u64 = 5;
pub(crate) const MAX_TIMEOUT_SECS: u64 = 3600;
/// Default connect timeout in seconds.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Connect timeout bounds in seconds.
pub(crate) const MIN_CONNECT_TIMEOUT_SECS: u64 = 1;
pub(crate) const MAX_CONNECT_TIMEOUT_SECS: u64 = 60;
/// Default and maximum number of redirects to follow.
const DEFAULT_MAX_REDIRECTS: u32 = 10;
pub(crate) const MAX_MAX_REDIRECTS: u32 = 10;
/// Base delay before the first retry; doubled per attempt, capped.
pub(crate) const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
pub(crate) const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
/// Longest `Retry-After` delay honoured from the server.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Default `User-Agent` sent with download requests.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

/// Capability names checked through the `role.check` hook.
/// Every downloader route requires `X-User-Id` / `X-Role`
/// headers and one of these permissions; checks fail closed.
pub(crate) const PERM_DOWNLOADER_VIEW: &str = "downloader.view";
pub(crate) const PERM_DOWNLOADER_MANAGE: &str = "downloader.manage";

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
    pub(crate) fn status(&self) -> DownloadStatus {
        *self.status.read().expect("job lock poisoned")
    }

    pub(crate) fn snapshot(&self) -> DownloadJobView {
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
    pub(crate) fn set_status(&self, status: DownloadStatus) -> bool {
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

    pub(crate) fn set_error(&self, message: String) {
        *self.error.write().expect("job lock poisoned") = Some(message);
    }

    pub(crate) fn mark_started(&self) {
        let mut started_at = self.started_at.write().expect("job lock poisoned");
        if started_at.is_none() {
            *started_at = Some(Utc::now());
        }
    }

    /// The `If-Range` validator for a resume request: the ETag when the
    /// server sent one, otherwise the `Last-Modified` timestamp.
    pub(crate) fn if_range_value(&self) -> Option<String> {
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

pub(crate) fn jobs_store() -> &'static DashMap<Uuid, Arc<DownloadJobState>> {
    JOBS.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateJobRequest {
    pub(crate) url: String,
    pub(crate) destination: String,
    /// Optional checksum to verify after the download, for example
    /// `"sha256:<hex>"`, `"sha512:<hex>"`, `"sha1:<hex>"`, or a bare hex
    /// digest (sha256).
    #[serde(default)]
    pub(crate) checksum: Option<String>,
    /// Retries after transient failures (0..=10, default 3).
    #[serde(default = "default_max_retries")]
    pub(crate) max_retries: u32,
    /// Total request timeout in seconds (5..=3600, default 3600).
    #[serde(default = "default_timeout_secs")]
    pub(crate) timeout_secs: u64,
    /// Connect timeout in seconds (1..=60, default 10).
    #[serde(default = "default_connect_timeout_secs")]
    pub(crate) connect_timeout_secs: u64,
    /// Redirects to follow (0..=10, default 10).
    #[serde(default = "default_max_redirects")]
    pub(crate) max_redirects: u32,
    /// Follow redirects (default true).
    #[serde(default = "default_follow_redirect")]
    pub(crate) follow_redirect: bool,
    /// Maximum download size in bytes. Exceeding it fails the job.
    /// `None` (default) means unlimited.
    #[serde(default)]
    pub(crate) max_bytes: Option<u64>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_status_spellings() {
        assert_eq!(DownloadStatus::Queued.as_str(), "queued");
        assert_eq!(DownloadStatus::Downloading.as_str(), "downloading");
        assert_eq!(DownloadStatus::Paused.as_str(), "paused");
        assert_eq!(DownloadStatus::Completed.as_str(), "completed");
        assert_eq!(DownloadStatus::Failed.as_str(), "failed");
        assert_eq!(DownloadStatus::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn is_finished_terminal_states() {
        assert!(DownloadStatus::Completed.is_finished());
        assert!(DownloadStatus::Failed.is_finished());
        assert!(DownloadStatus::Cancelled.is_finished());
        assert!(!DownloadStatus::Queued.is_finished());
        assert!(!DownloadStatus::Downloading.is_finished());
        assert!(!DownloadStatus::Paused.is_finished());
    }

    #[test]
    fn can_transition_to_cases() {
        assert!(DownloadStatus::Queued.can_transition_to(DownloadStatus::Downloading));
        assert!(!DownloadStatus::Queued.can_transition_to(DownloadStatus::Completed));
        assert!(DownloadStatus::Downloading.can_transition_to(DownloadStatus::Paused));
        assert!(!DownloadStatus::Downloading.can_transition_to(DownloadStatus::Queued));
        assert!(DownloadStatus::Paused.can_transition_to(DownloadStatus::Downloading));
        assert!(DownloadStatus::Failed.can_transition_to(DownloadStatus::Downloading));
        assert!(!DownloadStatus::Failed.can_transition_to(DownloadStatus::Completed));
        assert!(!DownloadStatus::Completed.can_transition_to(DownloadStatus::Downloading));
        assert!(!DownloadStatus::Cancelled.can_transition_to(DownloadStatus::Downloading));
    }
}
