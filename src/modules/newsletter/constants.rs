// Newsletter module - event names, permissions, and limits.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

pub const EVENT_NEWSLETTER_SUBSCRIBED: &str = "newsletter.subscribed";
pub const EVENT_NEWSLETTER_CONFIRMED: &str = "newsletter.confirmed";
pub const EVENT_NEWSLETTER_UNSUBSCRIBED: &str = "newsletter.unsubscribed";
pub const EVENT_NEWSLETTER_CAMPAIGN_SENT: &str = "newsletter.campaign.sent";
pub const EVENT_NEWSLETTER_CAMPAIGN_COMPLETED: &str = "newsletter.campaign.completed";
pub const EVENT_NEWSLETTER_OPEN: &str = "newsletter.open";
pub const EVENT_NEWSLETTER_CLICK: &str = "newsletter.click";
pub const EVENT_NEWSLETTER_CAMPAIGN_SEND: &str = "newsletter.campaign.send";
pub const EVENT_EMAIL_SEND_REQUESTED: &str = "email.send_requested";
pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";
pub const EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY: &str = "email_provider.webhook_verify";
pub const EVENT_SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

pub(crate) const CONFIRM_TOKEN_TTL_SECS: i64 = 48 * 3600;
pub(crate) const UNSUBSCRIBE_TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;

/// Permissions checked on every admin route through the `role.check`
/// hook (same pattern as the backup and workflows modules). Reads need
/// `newsletter:view`; every write needs `newsletter:manage`. The
/// token-based public endpoints (subscribe, confirm, unsubscribe via
/// link, tracking) and the signature-verified provider webhook stay
/// unauthenticated.
pub const PERM_NEWSLETTER_VIEW: &str = "newsletter:view";
pub const PERM_NEWSLETTER_MANAGE: &str = "newsletter:manage";

/// Hook event the roles module answers to confirm a caller holds a
/// permission. Fails closed: no answer means not allowed.
pub(crate) const EVENT_ROLE_CHECK: &str = "role.check";

/// A campaign left in `sending` longer than this is treated as stale
/// (the sender crashed mid-loop) and may be recovered: reclaimed by a
/// new send attempt, or reset to `failed` by the startup/worker sweep.
pub(crate) const STALE_SENDING_TIMEOUT_SECS: i64 = 30 * 60;

/// Cap on distinct click-target URLs recorded per campaign, so the
/// click allowlist cannot grow without bound.
pub(crate) const MAX_TRACKED_URLS_PER_CAMPAIGN: usize = 10_000;

/// Transparent 1x1 GIF returned by the open-tracking endpoint.
pub(crate) const TRACKING_PIXEL: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];
