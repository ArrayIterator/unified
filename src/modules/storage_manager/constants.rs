// Limits and permission capability names.

/// Maximum object upload size through the API (100 MiB), matching
/// the storage module. Larger objects must be uploaded through the
/// provider directly or in chunks by the caller.
pub(crate) const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Default hub download-token lifetime in seconds.
pub(crate) const DEFAULT_SIGNED_URL_SECS: u64 = 3600;
/// Maximum hub download-token lifetime in seconds (7 days).
pub(crate) const MAX_SIGNED_URL_SECS: u64 = 604_800;

/// Maximum age of a BYO validation that may still activate a
/// connection (24 hours). An older validation is stale: the
/// credentials or endpoint may have changed since it ran.
pub(crate) const BYO_MAX_VALIDATION_AGE_SECS: i64 = 24 * 3600;

/// Development fallback for the credential-encryption secret. The
/// configured `app.secret_key` wins whenever it is set.
pub(crate) const DEV_ENCRYPTION_SECRET: &str = "unified-storage-manager-development-secret";

// Permissions (capability names checked through the `role.check` hook).

pub(crate) const PERM_READ: &str = "storage_manager.read";
pub(crate) const PERM_MANAGE_BUCKETS: &str = "storage_manager.manage_buckets";
pub(crate) const PERM_MANAGE_BINDINGS: &str = "storage_manager.manage_bindings";
pub(crate) const PERM_MANAGE_QUOTAS: &str = "storage_manager.manage_quotas";
pub(crate) const PERM_BROWSE: &str = "storage_manager.browse";
pub(crate) const PERM_UPLOAD: &str = "storage_manager.upload";
pub(crate) const PERM_MANAGE_LIFECYCLE: &str = "storage_manager.manage_lifecycle";
pub(crate) const PERM_MANAGE_SIGNED_URLS: &str = "storage_manager.manage_signed_urls";
pub(crate) const PERM_HEALTH: &str = "storage_manager.health";
pub(crate) const PERM_MANAGE_PLANS: &str = "storage_manager.manage_plans";
pub(crate) const PERM_MANAGE_BYO: &str = "storage_manager.manage_byo";
pub(crate) const PERM_BYO_USE: &str = "storage_manager.use_byo";

// Hook event names.
