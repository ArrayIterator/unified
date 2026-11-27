//! Downloader module - file downloads with progress, pause / resume /
//!
//! Split into one file per concern under `downloader/`; this file only
//! declares the submodules and re-exports the public API. The full
//! module design notes live at the top of `downloader/module.rs`.
pub mod types;
mod validation;
mod checksum;
mod retry;
mod engine;
mod handlers;
pub mod module;

// Re-exports from types.rs.
pub(crate) use self::types::MAX_MAX_RETRIES;
pub(crate) use self::types::MIN_TIMEOUT_SECS;
pub(crate) use self::types::MAX_TIMEOUT_SECS;
pub(crate) use self::types::MIN_CONNECT_TIMEOUT_SECS;
pub(crate) use self::types::MAX_CONNECT_TIMEOUT_SECS;
pub(crate) use self::types::MAX_MAX_REDIRECTS;
pub(crate) use self::types::RETRY_BASE_DELAY;
pub(crate) use self::types::RETRY_MAX_DELAY;
pub(crate) use self::types::MAX_RETRY_AFTER;
pub(crate) use self::types::USER_AGENT;
pub(crate) use self::types::PERM_DOWNLOADER_VIEW;
pub(crate) use self::types::PERM_DOWNLOADER_MANAGE;
pub use self::types::DownloadStatus;
pub use self::types::DownloadJobView;
pub use self::types::DownloadJobState;
pub(crate) use self::types::jobs_store;
pub(crate) use self::types::CreateJobRequest;

// Re-exports from validation.rs.
pub(crate) use self::validation::validate_download_url;
pub(crate) use self::validation::parse_checksum;
pub(crate) use self::validation::build_client;
pub(crate) use self::validation::paginate;

// Re-exports from checksum.rs.
pub(crate) use self::checksum::ChecksumHashers;
pub(crate) use self::checksum::hash_file;
pub(crate) use self::checksum::verify_checksum_file;

// Re-exports from retry.rs.
pub(crate) use self::retry::AttemptError;
pub(crate) use self::retry::classify_reqwest_error;
pub(crate) use self::retry::parse_retry_after;
pub(crate) use self::retry::retry_delay;
pub(crate) use self::retry::parse_content_range_total;

// Re-exports from engine.rs.
pub(crate) use self::engine::run_download;
pub(crate) use self::engine::finalize;

// Re-exports from handlers.rs.
pub(crate) use self::handlers::create_job;
pub(crate) use self::handlers::list_jobs;
pub(crate) use self::handlers::get_job;
pub(crate) use self::handlers::pause_job;
pub(crate) use self::handlers::resume_job;
pub(crate) use self::handlers::cancel_job;
pub(crate) use self::handlers::retry_job;

// Re-exports from module.rs.
pub use self::module::DownloaderModule;
pub use self::module::register;

