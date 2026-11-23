//! manufacturing - constants.

use super::{types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};


/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_CODE_LEN: usize = 64;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_LABEL_LEN: usize = 120;

pub(crate) const MAX_UNIT_LEN: usize = 32;

/// Hard ceiling on BOM explosion depth, applied on top of the
/// configurable `max_bom_depth` setting so a pathological BOM
/// tree cannot recurse without bound.
pub(crate) const MAX_BOM_DEPTH_HARD_CAP: u32 = 50;

/// Hard ceiling on the number of lines a single BOM version may
/// carry, so one request cannot allocate an unbounded list.
pub(crate) const MAX_BOM_LINES: usize = 500;

/// Hard ceiling on routing steps per BOM version.
pub(crate) const MAX_ROUTING_STEPS: usize = 200;


/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_MFG_VIEW: &str = "manufacturing.view";

pub(crate) const PERM_MFG_MANAGE: &str = "manufacturing.manage";

pub(crate) const PERM_MFG_PRODUCE: &str = "manufacturing.produce";

pub(crate) const PERM_MFG_ADMIN: &str = "manufacturing.admin";


/// Events this module emits.
pub(crate) const EVENT_BOM_CREATED: &str = "manufacturing.bom.created";

pub(crate) const EVENT_BOM_VERSION_CREATED: &str = "manufacturing.bom.version.created";

pub(crate) const EVENT_WORK_ORDER_CREATED: &str = "manufacturing.work_order.created";

pub(crate) const EVENT_WORK_ORDER_STATUS_CHANGED: &str = "manufacturing.work_order.status_changed";

pub(crate) const EVENT_COMPONENTS_CONSUMED: &str = "manufacturing.components.consumed";

pub(crate) const EVENT_GOODS_PRODUCED: &str = "manufacturing.goods.produced";

pub(crate) const EVENT_PURCHASE_REQUESTED: &str = "manufacturing.purchase.requested";

pub(crate) const EVENT_INSPECTION_REQUESTED: &str = "manufacturing.inspection.requested";

pub(crate) const EVENT_FINISHED_GOODS_COSTED: &str = "manufacturing.finished_goods.costed";

pub(crate) const EVENT_STOCK_REQUESTED: &str = "manufacturing.stock.requested";

pub(crate) const EVENT_STOCK_RESERVE_REQUESTED: &str = "manufacturing.stock.reserve_requested";

pub(crate) const EVENT_STOCK_RELEASE_REQUESTED: &str = "manufacturing.stock.release_requested";

pub(crate) const EVENT_STOCK_DECREMENT_REQUESTED: &str = "assets.stock.decrement_requested";

pub(crate) const EVENT_COST_REQUESTED: &str = "manufacturing.cost.requested";

pub(crate) const EVENT_RESERVATION_CREATED: &str = "manufacturing.reservation.created";

pub(crate) const EVENT_RESERVATION_RELEASED: &str = "manufacturing.reservation.released";

pub(crate) const EVENT_PRODUCTION_RUN_COMPLETED: &str = "manufacturing.production_run.completed";

pub(crate) const EVENT_SCRAP_RECORDED: &str = "manufacturing.scrap.recorded";


/// Events this module listens to.
pub(crate) const EVENT_STOCK_CONFIRMED: &str = "manufacturing.stock.confirmed";

pub(crate) const EVENT_STOCK_SHORT: &str = "manufacturing.stock.short";

/// Canonical quality-completion event (emitted by the quality
/// module when an inspection completes). This module used to listen
/// for `manufacturing.inspection.completed`, which quality never
/// emitted; the canonical name is `quality.inspection.completed`.
pub(crate) const EVENT_INSPECTION_COMPLETED: &str = "quality.inspection.completed";

/// Quality block query (hook form of quality's `GET /blocks`):
/// ask the quality module which blocking alerts cover a target
/// before letting that target proceed.
pub(crate) const QUALITY_BLOCKS_QUERY: &str = "quality.blocks.query";

/// Target kind this module uses for work orders in quality
/// payloads (`target_ref` = the work order id).
pub(crate) const QUALITY_TARGET_KIND_WORK_ORDER: &str = "work_order";
