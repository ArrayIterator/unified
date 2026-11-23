// cloud module — hook event names.
//
// These are additive. The `hosting.*` and `provider.*` event names
// are unchanged; existing subscribers keep working.

/// Emitted when an app deployment succeeds.
pub const CLOUD_APP_DEPLOYED: &str = "cloud.app.deployed";

/// Emitted when an app is deleted (after its remote resource is gone).
pub const CLOUD_APP_DELETED: &str = "cloud.app.deleted";

/// Emitted when an app deployment fails after the plan and connection
/// checks passed (the remote call or the start step failed).
pub const CLOUD_APP_FAILED: &str = "cloud.app.failed";
