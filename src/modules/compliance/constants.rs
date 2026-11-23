//! Compliance Center module — GDPR / ISO support: data export — Module constants.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

/// Maximum lengths enforced on user supplied text.
pub(super) const MAX_NAME_LEN: usize = 200;
pub(super) const MAX_TITLE_LEN: usize = 200;
pub(super) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(super) const MAX_REASON_LEN: usize = 2000;
pub(super) const MAX_PURPOSE_LEN: usize = 500;
pub(super) const MAX_CATEGORY_LEN: usize = 120;
pub(super) const MAX_STORAGE_KEY_LEN: usize = 512;
pub(super) const MAX_BASIS_LEN: usize = 120;
pub(super) const MAX_SCOPE_LEN: usize = 500;
/// Default download-link lifetime for a completed export, in hours.
pub(super) const DEFAULT_DOWNLOAD_LINK_HOURS: i64 = 72;

/// Capability names checked through the `role.check` hook.
pub(super) const PERM_COMPLIANCE_VIEW: &str = "compliance.view";
pub(super) const PERM_COMPLIANCE_MANAGE: &str = "compliance.manage";

/// Scheduler job kind for retention reviews, and the event the
/// scheduler must emit when that job fires.
pub const COMPLIANCE_RETENTION_JOB_KIND: &str = "compliance.retention.review";
/// Event the scheduler emits when a queued retention-review job
/// fires. Reserved for scheduler dispatch only: requesting a
/// review must never emit this, or the request would complete its
/// own review before the scheduler ever ran it.
pub(super) const RETENTION_REVIEW_REQUESTED: &str = "compliance.retention.review_requested";
/// Event emitted when a retention review run is created by a
/// review request (announcement only — it carries no dispatch).
pub(super) const RETENTION_REVIEW_CREATED: &str = "compliance.retention.review.created";
pub(super) const DELETION_ITEM_EXECUTED: &str = "compliance.deletion.item.executed";
