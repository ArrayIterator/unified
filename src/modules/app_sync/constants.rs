//! app_sync - constants.



/// Capabilities enforced on the admin endpoints of this module (device
/// register/list/get/revoke). Registration requires `app-sync:manage`:
/// an unauthenticated caller must never be able to mint a device token.
/// Device-facing endpoints (`sync/push`, `sync/pull`) authenticate with
/// the HMAC device token instead.
pub const PERM_APP_SYNC_VIEW: &str = "app-sync:view";

pub const PERM_APP_SYNC_MANAGE: &str = "app-sync:manage";
