//! email_providers - events.

use std::time::Duration;
use super::{provider_kinds::*, credentials::*, store::*, http::*, registry::*, adapters::*, worker::*, hooks::*, handlers::*, module::*};



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

pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";

pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";



// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

pub(crate) const MAX_BATCH_RECIPIENTS: usize = 1000;

pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " email-providers");
