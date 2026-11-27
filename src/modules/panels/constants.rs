//! panels module - Event names, limits and the collection catalog.

use std::time::Duration;

// Events.

/// Emitted after a connection test succeeds.
pub const PANEL_CONNECTED: &str = "panel.connected";

/// Emitted once per client record imported or updated by a sync.
pub const PANEL_CLIENT_SYNCED: &str = "panel.client.synced";

/// Emitted once per service record imported or updated by a sync.
pub const PANEL_SERVICE_SYNCED: &str = "panel.service.synced";

/// Emitted after a sync run completes (even partially).
pub const PANEL_SYNC_COMPLETED: &str = "panel.sync.completed";

/// Emitted when a sync run fails for every collection, or fails
/// before any collection could be fetched.
pub const PANEL_SYNC_FAILED: &str = "panel.sync.failed";

/// Emitted when a connection is added or deleted (audit trail).
pub const PANEL_CONNECTION_CREATED: &str = "panel.connection.created";

pub const PANEL_CONNECTION_DELETED: &str = "panel.connection.deleted";

/// Emitted when a two-way sync conflict is detected or resolved.
pub const PANEL_SYNC_CONFLICT: &str = "panel.sync.conflict";

// Permissions.

/// Read panel connections, stored records, and stored sync status.
/// Every handler requires authentication first (see `caller` in
/// `handlers.rs`); a caller without this permission gets a 403.
pub(crate) const PERM_PANELS_VIEW: &str = "panels.view";

/// Create / update / delete connections, test them, run syncs, and
/// resolve conflicts. All of these can trigger outbound requests to
/// a panel, so they need the manage permission, not just view.
pub(crate) const PERM_PANELS_MANAGE: &str = "panels.manage";

// Limits.

/// Default HTTP timeout for panel API calls.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Default connect timeout for panel API calls.
pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Longest `Retry-After` delay honoured from a panel.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

/// Most redirects followed for one panel request. The redirect
/// policy re-validates every target against the configured host
/// (see `build_client`); this cap bounds the chain even when every
/// target is valid.
pub(crate) const MAX_REDIRECTS: u32 = 5;

/// Maximum records fetched in one live list call per collection.
pub(crate) const MAX_LIST_LIMIT: u32 = 250;

/// User-Agent sent to panel APIs.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " panels");

/// The six importable collections, in a stable order.
pub const COLLECTIONS: [&str; 6] = [
    "clients",
    "products",
    "services",
    "domains",
    "invoices",
    "server_accounts",
];
