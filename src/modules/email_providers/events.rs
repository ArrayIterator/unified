//! email_providers - events.

use super::{
    adapters::*, credentials::*, handlers::*, hooks::*, http::*, module::*, provider_kinds::*,
    registry::*, store::*, worker::*,
};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------
pub const EVENT_EMAIL_SEND_REQUESTED: &str = "email.send_requested";

pub const EVENT_EMAIL_SENT: &str = "email.sent";

pub const EVENT_EMAIL_FAILED: &str = "email.failed";

pub const EVENT_EMAIL_PROVIDER_CONNECTED: &str = "email_provider.connected";

pub const EVENT_EMAIL_PROVIDER_CONNECTION_CREATED: &str = "email_provider.connection.created";

pub const EVENT_EMAIL_PROVIDER_CONNECTION_DELETED: &str = "email_provider.connection.deleted";

pub const EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY: &str = "email_provider.webhook_verify";

/// Emitted when the catalog builds a provider's settings-field list:
/// other modules may add, remove, or replace fields. Payload
/// `{ kind, fields }`; see `settings_fields_for_catalog` in
/// `handlers.rs` for the result contract (task 350, addition A).
pub const EVENT_EMAIL_PROVIDER_SETTINGS_FIELDS: &str = "email_provider.settings_fields";

/// Emitted when an OAuth account is linked to a connection (the
/// callback stored fresh tokens). Payload `{ connection_id, kind,
/// account, expires_at }`.
pub const EVENT_EMAIL_PROVIDER_OAUTH_LINKED: &str = "email_provider.oauth_linked";

/// Emitted when an adapter refreshed an expired access token from
/// the stored refresh token. Payload `{ connection_id, kind,
/// expires_at }`.
pub const EVENT_EMAIL_PROVIDER_OAUTH_REFRESHED: &str = "email_provider.oauth_refreshed";

pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";

pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

pub(crate) const MAX_BATCH_RECIPIENTS: usize = 1000;

pub(crate) const USER_AGENT: &str =
    concat!("unified/", env!("CARGO_PKG_VERSION"), " email-providers");
