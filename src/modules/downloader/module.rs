//! Module registration: struct, Module impl and register().

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
// Downloader module - file downloads with progress, pause / resume /
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
//
// SSRF guard: a download URL must be http(s) with no embedded
// credentials, and its host must not resolve to a private,
// loopback, link-local, or reserved address; every redirect target
// is re-validated with the same rules before it is followed.
// Loopback is forbidden by default - only an explicit
// `DOWNLOADER_ALLOW_LOOPBACK=1` opt-in (development only, never
// in production) re-allows it.
//
// Authentication and destination confinement: every route requires
// the `X-User-Id` / `X-Role` headers and a capability check
// (`downloader.view` for reads, `downloader.manage` for creates
// and controls) through the `role.check` hook, failing closed, so
// no unauthenticated caller can make the server fetch a URL and
// write the result to disk. Destinations are confined to the
// download directory (`DOWNLOADER_DOWNLOAD_DIR`, default
// `downloads`): absolute paths and `..` traversal are rejected,
// so a job can never write outside that directory.


// Module definition.

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
            RouteInfo { method: "GET", path: "/api/v1/downloader/jobs", owner: "downloader", description: "List jobs", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs", owner: "downloader", description: "Create job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/downloader/jobs/{id}", owner: "downloader", description: "Get job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/pause", owner: "downloader", description: "Pause job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/resume", owner: "downloader", description: "Resume job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/cancel", owner: "downloader", description: "Cancel job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/downloader/jobs/{id}/retry", owner: "downloader", description: "Retry job", auth: unified_core::auth::RouteAuth::Authenticated},
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
