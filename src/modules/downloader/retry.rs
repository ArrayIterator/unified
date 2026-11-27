//! Per-attempt failure classification and backoff.

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

// Retry handling.

/// Failure of one download attempt. `retryable` failures (connection
/// errors, timeouts, HTTP 408 / 429 / 5xx, checksum mismatch) may be
/// retried; anything else fails the job at once.
pub(crate) struct AttemptError {
    pub(crate) retryable: bool,
    pub(crate) retry_after: Option<Duration>,
    pub(crate) error: UnifiedError,
}

impl AttemptError {
    pub(crate) fn terminal(message: impl Into<String>) -> Self {
        Self {
            retryable: false,
            retry_after: None,
            error: UnifiedError::internal(message),
        }
    }

    pub(crate) fn retryable(message: impl Into<String>) -> Self {
        Self {
            retryable: true,
            retry_after: None,
            error: UnifiedError::internal(message),
        }
    }

    pub(crate) fn retryable_after(
        message: impl Into<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            retryable: true,
            retry_after,
            error: UnifiedError::internal(message),
        }
    }
}

/// Classify a reqwest error: connection problems, timeouts and body /
/// stream errors are transient; anything else (bad URL, redirect policy,
/// request-building errors, decoding) is terminal. Request-building
/// errors in particular will never succeed on retry, so they must not
/// burn the retry budget.
pub(crate) fn classify_reqwest_error(error: reqwest::Error) -> AttemptError {
    if error.is_connect() || error.is_timeout() || error.is_body() {
        AttemptError::retryable(format!("Download request failed: {error}"))
    } else {
        AttemptError::terminal(format!("Download request failed: {error}"))
    }
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are ignored),
/// capped so a misbehaving server cannot stall a job indefinitely.
pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
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
/// server `Retry-After` delay when it is longer. The result is capped
/// at `MAX_RETRY_AFTER`, so an uncapped server delay cannot stall a
/// job (or a cancel) indefinitely.
pub(crate) fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let backoff = RETRY_BASE_DELAY
        .saturating_mul(1u32 << attempt.min(6))
        .min(RETRY_MAX_DELAY);
    match retry_after {
        Some(delay) => backoff.max(delay).min(MAX_RETRY_AFTER),
        None => backoff,
    }
}

/// Parse the total size out of a `Content-Range` header
/// (`bytes 100-199/1000` or `bytes */1000`).
pub(crate) fn parse_content_range_total(response: &reqwest::Response) -> Option<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::downloader::{RETRY_BASE_DELAY, RETRY_MAX_DELAY};

    #[test]
    fn retry_delay_grows_with_attempt() {
        assert_eq!(retry_delay(0, None), RETRY_BASE_DELAY);
        assert_eq!(retry_delay(1, None), RETRY_BASE_DELAY * 2);
        assert_eq!(retry_delay(2, None), RETRY_BASE_DELAY * 4);
    }

    #[test]
    fn retry_delay_caps_at_max() {
        assert_eq!(retry_delay(10, None), RETRY_MAX_DELAY);
        assert_eq!(retry_delay(100, None), RETRY_MAX_DELAY);
    }

    #[test]
    fn retry_delay_honours_long_retry_after() {
        assert_eq!(retry_delay(0, Some(Duration::from_secs(60))), Duration::from_secs(60));
    }

    #[test]
    fn retry_delay_ignores_short_retry_after() {
        assert_eq!(retry_delay(2, Some(Duration::from_millis(100))), RETRY_BASE_DELAY * 4);
    }

    #[test]
    fn retry_delay_caps_uncapped_retry_after() {
        // A server asking for an hour must not stall the job (or a
        // cancel) beyond MAX_RETRY_AFTER.
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(3600))),
            MAX_RETRY_AFTER
        );
        assert_eq!(
            retry_delay(0, Some(Duration::from_secs(30))),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn classify_reqwest_error_request_building_is_terminal() {
        // A request-building error (invalid URL at build time) will
        // never succeed on retry and must not burn the retry budget.
        let error = reqwest::Client::new()
            .get("://not-a-url")
            .build()
            .unwrap_err();
        assert!(error.is_builder());
        assert!(!classify_reqwest_error(error).retryable);
    }

    #[test]
    fn classify_reqwest_error_connect_is_retryable() {
        // Connection refused on loopback: a transient connect failure.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime.block_on(async {
            reqwest::Client::new()
                .get("http://127.0.0.1:9/file.zip")
                .send()
                .await
                .unwrap_err()
        });
        assert!(error.is_connect());
        assert!(classify_reqwest_error(error).retryable);
    }
}
