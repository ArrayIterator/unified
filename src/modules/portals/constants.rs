//! Portals - limits, sections, capability and event names.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_SLUG_LEN: usize = 128;

pub(crate) const MAX_LABEL_LEN: usize = 128;

pub(crate) const MAX_URL_LEN: usize = 2048;

pub(crate) const MAX_DOMAIN_LEN: usize = 253;

pub(crate) const MAX_COMPANY_LEN: usize = 128;

pub(crate) const MAX_STORAGE_KEY_LEN: usize = 512;

pub(crate) const MAX_ROLES_PER_MENU: usize = 20;

pub(crate) const MAX_ROLE_LEN: usize = 80;

pub(crate) const MAX_MENUS_PER_PORTAL: usize = 50;

/// Maximum page-view rows kept per portal; the oldest view is
/// dropped once the cap is reached so views stay bounded.
pub(crate) const MAX_VIEWS_PER_PORTAL: usize = 10_000;

/// Portal sections a client may see. The canonical order is the
/// default section order of a new portal.
pub(crate) const SECTION_NAMES: [&str; 5] = [
    "invoices",
    "tickets",
    "downloads",
    "subscriptions",
    "licenses",
];

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_PORTALS_VIEW: &str = "portals.view";

pub(crate) const PERM_PORTALS_MANAGE: &str = "portals.manage";

/// Hook event names this module listens to for its read-only
/// client-data projection.
pub(crate) const INVOICE_CREATED: &str = "invoice.created";

pub(crate) const INVOICE_PAID: &str = "invoice.paid";

pub(crate) const INVOICE_VOIDED: &str = "invoice.voided";

pub(crate) const TICKET_CREATED: &str = "ticket.created";

pub(crate) const TICKET_UPDATED: &str = "ticket.updated";

pub(crate) const ORDER_CREATED: &str = "store.order.created";

pub(crate) const ORDER_COMPLETED: &str = "store.order.completed";

pub(crate) const ORDER_CANCELLED: &str = "store.order.cancelled";

pub(crate) const LICENSE_ISSUED: &str = "license.issued";

pub(crate) const LICENSE_ACTIVATED: &str = "license.activated";

pub(crate) const LICENSE_SUSPENDED: &str = "license.suspended";

pub(crate) const LICENSE_REVOKED: &str = "license.revoked";

pub(crate) const DOWNLOAD_STARTED: &str = "download.started";

pub(crate) const DOWNLOAD_FINISHED: &str = "download.finished";
