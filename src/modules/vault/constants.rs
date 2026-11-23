//! vault module - Limits, permission names and audit action names.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_STORAGE_TARGET_LEN: usize = 128;

pub(crate) const MAX_STORAGE_KEY_LEN: usize = 1024;

pub(crate) const MAX_MIME_TYPE_LEN: usize = 128;

pub(crate) const MAX_REASON_LEN: usize = 2000;

pub(crate) const MAX_NOTE_LEN: usize = 2000;

pub(crate) const MAX_CLIENTS: usize = 500;

/// Maximum versions kept per document in the in-memory store. Old
/// versions past this cap are still listed in the migration tables
/// as history; the in-memory store drops the oldest non-current
/// version first (the current version is never dropped).
pub(crate) const MAX_VERSIONS_PER_DOCUMENT: usize = 100;

/// Maximum downloads a share link may grant when no explicit cap
/// is given.
pub(crate) const MAX_LINK_DOWNLOADS: i64 = 10_000;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_VAULT_VIEW: &str = "vault.view";

pub(crate) const PERM_VAULT_MANAGE: &str = "vault.manage";

/// Access-log action names. The log is append-only: rows are never
/// updated or deleted by this module.
pub const ACTION_VIEW: &str = "view";

pub const ACTION_DOWNLOAD: &str = "download";

pub const ACTION_SHARE: &str = "share";

pub const ACTION_REQUEST: &str = "request";

pub const ACTION_APPROVE: &str = "approve";

pub const ACTION_DENY: &str = "deny";

pub const ACTION_RESTORE: &str = "restore";

pub const ACTION_DELETE: &str = "delete";

pub const ACTION_UPLOAD: &str = "upload";

pub const ACTION_LINK_REVOKE: &str = "link_revoke";

pub const ACTION_FLAG_REVIEW: &str = "flag_review";
