//! manufacturing - store.

use unified_core::error::{UnifiedError, UnifiedResult};
use super::{constants::*, types::*, inputs::*, helpers::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// Loaders
pub(crate) fn load_bom(state: &ManufacturingState, id: &str) -> UnifiedResult<Bom> {
    state
        .boms
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BOM not found: {id}")))
}


pub(crate) fn load_bom_version(state: &ManufacturingState, id: &str) -> UnifiedResult<BomVersion> {
    state
        .bom_versions
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BOM version not found: {id}")))
}


pub(crate) fn load_work_center(state: &ManufacturingState, id: &str) -> UnifiedResult<WorkCenter> {
    state
        .work_centers
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Work center not found: {id}")))
}


pub(crate) fn load_work_order(state: &ManufacturingState, id: &str) -> UnifiedResult<WorkOrder> {
    state
        .work_orders
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Work order not found: {id}")))
}


pub(crate) fn load_scrap_reason(state: &ManufacturingState, id: &str) -> UnifiedResult<ScrapReason> {
    state
        .scrap_reasons
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Scrap reason not found: {id}")))
}


pub(crate) fn active_bom_for_product(state: &ManufacturingState, product_id: &str) -> Option<Bom> {
    state
        .boms
        .iter()
        .map(|entry| entry.value().clone())
        .find(|bom| bom.product_id == product_id && bom.status == "active")
}


pub(crate) fn save_work_order(state: &ManufacturingState, order: &WorkOrder) {
    state.work_orders.insert(order.id.clone(), order.clone());
}


pub(crate) fn save_bom(state: &ManufacturingState, bom: &Bom) {
    state.boms.insert(bom.id.clone(), bom.clone());
}
