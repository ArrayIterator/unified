// Third-party VCS integrations module.

// Split into per-concern files under `src/modules/vcs/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod providers;
pub mod store;
mod helpers;
mod validation;
mod service;
mod handlers_providers;
mod handlers_connections;
mod handlers_drafts;
mod webhooks;
pub mod module;

// Re-exported from `types`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::UpdateProviderSettings;
pub use self::types::RepositoryConnection;
pub use self::types::DraftAsset;
pub use self::types::VersionDraft;
pub use self::types::SyncHistoryEntry;
pub use self::types::RateLimitInfo;
pub use self::types::CreateConnection;
pub use self::types::UpdateConnection;
pub use self::types::ImportRelease;
pub use self::types::DownloadAssetRequest;

// Re-exported from `providers`.
pub use self::providers::Provider;
pub use self::providers::VcsProvider;
pub use self::providers::ProviderSettings;
pub use self::providers::ProviderSettingsView;
pub use self::providers::ProviderView;
pub use self::providers::ProviderRelease;
pub use self::providers::ProviderAsset;
pub use self::providers::ProviderReleases;

// Re-exported from `store`.
pub use self::store::RateLimitState;
pub use self::store::VcsState;

// Re-exported from `module`.
pub use self::module::VcsModule;
pub use self::module::register;
