//! quality module - Limits, permissions, event names and state vocabularies.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 64;

pub(crate) const MAX_REF_LEN: usize = 500;

pub(crate) const MAX_TEXT_LEN: usize = 20000;

pub(crate) const MAX_NOTE_LEN: usize = 2000;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_QUALITY_VIEW: &str = "quality.view";

pub(crate) const PERM_QUALITY_MANAGE: &str = "quality.manage";

pub(crate) const PERM_QUALITY_INSPECT: &str = "quality.inspect";

/// Event names this module listens to.
pub(crate) const MANUFACTURING_INSPECTION_REQUESTED: &str = "manufacturing.inspection.requested";

pub(crate) const INVENTORY_GOODS_RECEIVED: &str = "inventory.goods_received";

pub(crate) const PROCUREMENT_QUALITY_CHECK_REQUESTED: &str =
    "procurement.supplier_quality_check.requested";

pub(crate) const LOGISTICS_SHIPMENT_INSPECTION_REQUESTED: &str =
    "logistics.shipment.inspection.requested";

/// Block query (hook form of `GET /blocks`): owning modules ask
/// whether a target is blocked before letting it proceed.
pub(crate) const QUALITY_BLOCKS_QUERY: &str = "quality.blocks.query";

/// Inspection lifecycle statuses. These are the stored status
/// values; the allowed transitions live in
/// `valid_inspection_transition`.
pub(crate) const INSP_REQUESTED: &str = "requested";

pub(crate) const INSP_COMPLETED: &str = "completed";

pub(crate) const INSP_CANCELLED: &str = "cancelled";

/// Inspection result values. The vocabulary is the live settings
/// list; these constants are the values the evaluation logic
/// produces, and they are only accepted when the settings list
/// contains them (validated at evaluation time).
pub(crate) const RESULT_PASS: &str = "pass";

pub(crate) const RESULT_FAIL: &str = "fail";

pub(crate) const RESULT_CONDITIONAL: &str = "conditional";

/// Alert lifecycle statuses.
pub(crate) const ALERT_OPEN: &str = "open";

pub(crate) const ALERT_ACKNOWLEDGED: &str = "acknowledged";

pub(crate) const ALERT_RESOLVED: &str = "resolved";

/// CAPA states. The vocabulary is the live settings list; the
/// allowed transitions live in `valid_capa_transition`.
pub(crate) const CAPA_OPEN: &str = "open";

pub(crate) const CAPA_IN_PROGRESS: &str = "in_progress";

pub(crate) const CAPA_VERIFIED: &str = "verified";

pub(crate) const CAPA_CLOSED: &str = "closed";
