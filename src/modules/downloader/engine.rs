//! Download engine: attempts, resume and finalize.

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
// Background download worker.
// ---------------------------------------------------------------------------

pub(crate) async fn run_download(ctx: Arc<AppContext>, job: Arc<DownloadJobState>) {
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

/// Sleep for `delay`, but wake every 100 ms to check whether the
/// job was cancelled, so a cancel takes effect promptly instead of
/// waiting out the full retry backoff.
async fn sleep_cancellable(cancelled: &AtomicBool, delay: Duration) {
    let mut remaining = delay;
    while !remaining.is_zero() {
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        let slice = remaining.min(Duration::from_millis(100));
        tokio::time::sleep(slice).await;
        remaining = remaining.saturating_sub(slice);
    }
}

/// Per-attempt download speed: only the bytes downloaded in this
/// attempt count, divided by the attempt's elapsed time. The
/// resumed prefix (bytes downloaded by earlier attempts) must not
/// inflate the speed or shrink the ETA.
fn attempt_speed_bytes_per_sec(
    downloaded: u64,
    attempt_start_offset: u64,
    elapsed_secs: f64,
) -> u64 {
    let attempt_bytes = downloaded.saturating_sub(attempt_start_offset);
    (attempt_bytes as f64 / elapsed_secs.max(0.001)) as u64
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
                sleep_cancellable(&job.cancelled, delay).await;
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
        // The server says the range is already complete — but that
        // only proves the server thinks so. With a configured
        // checksum the partial file is verified below before it is
        // finalised; without one there is nothing to prove the
        // bytes are the complete file, so finalising here would
        // bless a truncated or corrupt file. Discard the partial
        // file and retry from scratch instead: the next attempt
        // finds no partial file and downloads the whole file.
        if job.checksum.is_none() {
            let _ = std::fs::remove_file(&job.part_path);
            job.downloaded_bytes.store(0, Ordering::SeqCst);
            *job.total_bytes.write().expect("job lock poisoned") = None;
            return Err(AttemptError::retryable(
                "Server reports the download range complete, but no checksum is configured to verify the partial file; restarting the download from scratch",
            ));
        }
        job.downloaded_bytes.store(existing_len, Ordering::SeqCst);
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
        let elapsed = started.elapsed().as_secs_f64();
        job.speed_bytes_per_sec.store(
            attempt_speed_bytes_per_sec(downloaded, start_offset, elapsed),
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
pub(crate) fn finalize(job: &DownloadJobState) -> Result<(), AttemptError> {
    std::fs::rename(&job.part_path, &job.destination).map_err(|error| {
        AttemptError::terminal(format!(
            "Cannot move completed download into place at {}: {error}",
            job.destination.display()
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_speed_counts_only_this_attempt_bytes() {
        // 1000 resumed bytes + 500 new bytes in 1 s: the speed is
        // 500 B/s, not the cumulative 1500 B/s the old code showed.
        assert_eq!(attempt_speed_bytes_per_sec(1500, 1000, 1.0), 500);
        assert_eq!(attempt_speed_bytes_per_sec(500, 0, 1.0), 500);
        assert_eq!(attempt_speed_bytes_per_sec(1500, 1000, 0.5), 1000);
        // A download that has not moved past its resume offset
        // reports zero, never a negative or wrapped speed.
        assert_eq!(attempt_speed_bytes_per_sec(1000, 1000, 1.0), 0);
        assert_eq!(attempt_speed_bytes_per_sec(900, 1000, 1.0), 0);
    }

    #[tokio::test]
    async fn sleep_cancellable_returns_early_when_cancelled() {
        let cancelled = AtomicBool::new(true);
        let started = Instant::now();
        sleep_cancellable(&cancelled, Duration::from_secs(60)).await;
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn sleep_cancellable_waits_the_full_delay_when_not_cancelled() {
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        sleep_cancellable(&cancelled, Duration::from_millis(250)).await;
        assert!(started.elapsed() >= Duration::from_millis(200));
    }
}
