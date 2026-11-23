//! manufacturing - handlers work orders.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// ---------------------------------------------------------------------------
// Work order handlers
// ---------------------------------------------------------------------------
/// Build the component requirements for a work order from its
/// pinned BOM version and planned quantity (one level; the scrap
/// allowance is added on top of the nominal quantity).
pub(crate) fn build_requirements(version: &BomVersion, quantity: f64) -> Vec<WorkOrderRequirement> {
    version
        .lines
        .iter()
        .map(|line| {
            let allowance = 1.0 + line.scrap_allowance_percent / 100.0;
            WorkOrderRequirement {
                component_product_id: line.component_product_id.clone(),
                quantity_required: quantity * line.quantity * allowance,
                unit: line.unit.clone(),
                quantity_reserved: 0.0,
                quantity_consumed: 0.0,
                quantity_short: 0.0,
            }
        })
        .collect()
}


pub(crate) async fn list_work_orders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let bom_filter = params.filters.get("bom_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WorkOrder> = state
            .work_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|order| {
                if let Some(status) = &status_filter {
                    if order.status != *status {
                        return false;
                    }
                }
                if let Some(product_id) = &product_filter {
                    if order.product_id != *product_id {
                        return false;
                    }
                }
                if let Some(bom_id) = &bom_filter {
                    if order.bom_id != *bom_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", order.number, order.product_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["number", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "number" => a.number.cmp(&b.number),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let bom = load_bom(&state, &input.bom_id)?;
        if bom.status == "archived" {
            return Err(UnifiedError::conflict(
                "Archived BOMs cannot start new work orders",
            ));
        }
        let version_id = input
            .bom_version_id
            .clone()
            .or(bom.active_version_id.clone())
            .ok_or_else(|| UnifiedError::conflict("BOM has no active version"))?;
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom.id {
            return Err(UnifiedError::bad_request(
                "bom_version_id does not belong to bom_id",
            ));
        }
        let quantity = valid_quantity(input.quantity, "quantity")?;
        if let (Some(start), Some(end)) = (input.planned_start, input.planned_end) {
            if end < start {
                return Err(UnifiedError::bad_request(
                    "planned_end must not be earlier than planned_start",
                ));
            }
        }
        let company_id = optional_text(&input.company_id, "company_id", MAX_CODE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let order = WorkOrder {
            id: new_id(),
            number: state.next_order_number(),
            bom_id: bom.id.clone(),
            bom_version_id: version.id.clone(),
            bom_version: version.version,
            product_id: bom.product_id.clone(),
            quantity_planned: quantity,
            quantity_produced: 0.0,
            quantity_scrapped: 0.0,
            status: "draft".to_string(),
            requirements: build_requirements(&version, quantity),
            planned_start: input.planned_start,
            planned_end: input.planned_end,
            actual_start: None,
            actual_end: None,
            company_id,
            notes,
            inspection_status: None,
            inspection_notes: None,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        save_work_order(&state, &order);
        state.emit(
            EVENT_WORK_ORDER_CREATED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "bom_id": order.bom_id,
                "bom_version_id": order.bom_version_id,
                "product_id": order.product_id,
                "quantity_planned": order.quantity_planned,
                "created_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        load_work_order(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only draft work orders can be edited (current status: {})",
                order.status
            )));
        }
        if let Some(quantity) = input.quantity {
            order.quantity_planned = valid_quantity(quantity, "quantity")?;
            let version = load_bom_version(&state, &order.bom_version_id)?;
            order.requirements = build_requirements(&version, order.quantity_planned);
        }
        if let Some(start) = input.planned_start {
            order.planned_start = Some(start);
        }
        if let Some(end) = input.planned_end {
            order.planned_end = Some(end);
        }
        if let (Some(start), Some(end)) = (order.planned_start, order.planned_end) {
            if end < start {
                return Err(UnifiedError::bad_request(
                    "planned_end must not be earlier than planned_start",
                ));
            }
        }
        if let Some(notes) = &input.notes {
            order.notes = optional_text(&Some(notes.clone()), "notes", MAX_NOTES_LEN)?;
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        Ok(order)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Checked transition: the target must appear in the settings
/// transition map for the current status, otherwise 409 with the
/// allowed list. Confirming also runs the capacity check.
pub(crate) async fn transition_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut order = load_work_order(&state, &path.into_inner())?;
        let target = input.to_status.trim().to_lowercase();
        let settings = state.settings();
        let allowed = settings
            .work_order_transitions
            .get(&order.status)
            .cloned()
            .unwrap_or_default();
        if !allowed.contains(&target) {
            return Err(UnifiedError::conflict(format!(
                "Illegal transition from \"{}\" to \"{}\"",
                order.status, target
            ))
            .with_metadata("allowed", json!(allowed))
            .with_metadata("current_status", json!(order.status)));
        }
        if target == "in_progress" && order.actual_start.is_none() {
            order.actual_start = Some(now());
        }
        if target == "done" {
            if order.quantity_produced <= 0.0 {
                return Err(UnifiedError::conflict(
                    "A work order with no produced quantity cannot be marked done; record a production run first",
                ));
            }
            order.actual_end = Some(now());
        }
        if target == "cancelled" {
            release_reservations(&state, &order.id, &caller.actor);
        }
        let mut warnings: Vec<Value> = Vec::new();
        if target == "confirmed" {
            warnings = capacity_warnings(&state, &order);
            if settings.enforce_capacity_on_confirm && !warnings.is_empty() {
                return Err(UnifiedError::conflict(
                    "Work order cannot be confirmed: routing load exceeds work center capacity",
                )
                .with_metadata("capacity_warnings", json!(warnings)));
            }
        }
        let previous = order.status.clone();
        order.status = target.clone();
        if let Some(notes) = &input.notes {
            order.notes = optional_text(&Some(notes.clone()), "notes", MAX_NOTES_LEN)?;
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        state.emit(
            EVENT_WORK_ORDER_STATUS_CHANGED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "from_status": previous,
                "to_status": target,
                "changed_by": caller.actor,
            }),
        );
        Ok(json!({ "work_order": order, "capacity_warnings": warnings }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Release every open reservation of a work order (used on
/// cancel and by the release endpoint). Emits a release request
/// per reservation.
pub(crate) fn release_reservations(state: &ManufacturingState, work_order_id: &str, actor: &str) {
    let mut released: Vec<String> = Vec::new();
    for mut entry in state.reservations.iter_mut() {
        let reservation = entry.value_mut();
        if reservation.work_order_id == work_order_id
            && (reservation.status == "reserved"
                || reservation.status == "pending_confirmation"
                || reservation.status == "short")
        {
            reservation.status = "released".to_string();
            reservation.updated_at = now();
            released.push(reservation.id.clone());
            state.emit(
                EVENT_STOCK_RELEASE_REQUESTED,
                json!({
                    "reservation_id": reservation.id,
                    "work_order_id": work_order_id,
                    "component_product_id": reservation.component_product_id,
                    "quantity": reservation.quantity_reserved,
                    "unit": reservation.unit,
                    "released_by": actor,
                }),
            );
        }
    }
    if !released.is_empty() {
        state.emit(
            EVENT_RESERVATION_RELEASED,
            json!({
                "work_order_id": work_order_id,
                "reservation_ids": released,
                "released_by": actor,
            }),
        );
    }
}


/// Shared stock check: asks the inventory side, compares with
/// the order's requirements, emits a purchase request per
/// shortfall, and reports the per-component picture.
pub(crate) fn check_stock_for_order(
    state: &ManufacturingState,
    order: &WorkOrder,
    actor: &str,
) -> Value {
    let product_ids: Vec<String> = order
        .requirements
        .iter()
        .map(|req| req.component_product_id.clone())
        .collect();
    let (levels, answered) = query_stock(state, &product_ids);
    let mut rows: Vec<Value> = Vec::new();
    let mut shortfalls: Vec<Value> = Vec::new();
    for req in &order.requirements {
        let available = levels.get(&req.component_product_id).map(|l| l.available);
        let remaining = req.quantity_required - req.quantity_consumed - req.quantity_reserved;
        let short = match available {
            Some(available) => (remaining - available).max(0.0),
            None => 0.0,
        };
        if short > 0.0 {
            shortfalls.push(json!({
                "component_product_id": req.component_product_id,
                "quantity_short": short,
                "unit": req.unit,
            }));
            state.emit(
                EVENT_PURCHASE_REQUESTED,
                json!({
                    "work_order_id": order.id,
                    "product_id": req.component_product_id,
                    "quantity_short": short,
                    "unit": req.unit,
                    "requested_by": actor,
                }),
            );
        }
        rows.push(json!({
            "component_product_id": req.component_product_id,
            "unit": req.unit,
            "quantity_required": req.quantity_required,
            "quantity_reserved": req.quantity_reserved,
            "quantity_consumed": req.quantity_consumed,
            "quantity_remaining": remaining.max(0.0),
            "available": available,
            "quantity_short": short,
            // Per-product knowledge, same rule as the reservation
            // path: a component with no level in the answer is
            // `unknown` even when the inventory side answered for
            // another component.
            "stock_status": if levels.contains_key(&req.component_product_id) {
                "known"
            } else {
                "unknown"
            },
        }));
    }
    json!({
        "work_order_id": order.id,
        "number": order.number,
        "stock_answered": answered,
        "stock_status": if answered { "known" } else { "unknown" },
        "components": rows,
        "shortfalls": shortfalls,
    })
}


pub(crate) async fn check_work_order_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        Ok(check_stock_for_order(&state, &order, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn reserve_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "confirmed" && order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only confirmed or in-progress work orders can reserve stock (current status: {})",
                order.status
            )));
        }
        let product_ids: Vec<String> = order
            .requirements
            .iter()
            .map(|req| req.component_product_id.clone())
            .collect();
        let (levels, answered) = query_stock(&state, &product_ids);
        let mut created: Vec<MaterialReservation> = Vec::new();
        let mut shortfalls: Vec<Value> = Vec::new();
        for req in order.requirements.iter_mut() {
            let remaining = req.quantity_required - req.quantity_reserved;
            if remaining <= 0.0 {
                continue;
            }
            // Do not double-reserve a component that already has
            // an open reservation on this order.
            let already_open = state.reservations.iter().any(|entry| {
                let r = entry.value();
                r.work_order_id == order.id
                    && r.component_product_id == req.component_product_id
                    && matches!(
                        r.status.as_str(),
                        "pending_confirmation" | "reserved" | "partially_consumed" | "short"
                    )
            });
            if already_open {
                continue;
            }
            let available = levels.get(&req.component_product_id).map(|l| l.available);
            // Per-product knowledge: only a component the inventory
            // side actually reported a level for flips to
            // `reserved`. A component with no level in the answer
            // stays `pending_confirmation` with `stock_unknown`,
            // even when another component was answered — the old
            // code flipped every component once any handler
            // answered at all.
            let known = levels.contains_key(&req.component_product_id);
            let mut status = if known { "reserved" } else { "pending_confirmation" };
            if let Some(available) = available {
                if available < remaining {
                    status = "short";
                    shortfalls.push(json!({
                        "component_product_id": req.component_product_id,
                        "quantity_short": remaining - available,
                        "unit": req.unit,
                    }));
                    state.emit(
                        EVENT_PURCHASE_REQUESTED,
                        json!({
                            "work_order_id": order.id,
                            "product_id": req.component_product_id,
                            "quantity_short": remaining - available,
                            "unit": req.unit,
                            "requested_by": caller.actor,
                        }),
                    );
                }
            }
            let reservation = MaterialReservation {
                id: new_id(),
                work_order_id: order.id.clone(),
                component_product_id: req.component_product_id.clone(),
                quantity_reserved: remaining,
                quantity_consumed: 0.0,
                unit: req.unit.clone(),
                status: status.to_string(),
                stock_unknown: !known,
                confirmation_reference: None,
                created_by: caller.actor.clone(),
                created_at: now(),
                updated_at: now(),
            };
            if status == "reserved" {
                req.quantity_reserved += remaining;
            }
            state.emit(
                EVENT_STOCK_RESERVE_REQUESTED,
                json!({
                    "reservation_id": reservation.id,
                    "work_order_id": order.id,
                    "component_product_id": reservation.component_product_id,
                    "quantity": reservation.quantity_reserved,
                    "unit": reservation.unit,
                    "requested_by": caller.actor,
                }),
            );
            state
                .reservations
                .insert(reservation.id.clone(), reservation.clone());
            created.push(reservation);
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        state.emit(
            EVENT_RESERVATION_CREATED,
            json!({
                "work_order_id": order.id,
                "reservation_ids": created.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
                "created_by": caller.actor,
            }),
        );
        Ok(json!({
            "work_order_id": order.id,
            "stock_answered": answered,
            "stock_status": if answered { "known" } else { "unknown" },
            "reservations": created,
            "shortfalls": shortfalls,
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn release_work_order_reservations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        release_reservations(&state, &order.id, &caller.actor);
        for req in order.requirements.iter_mut() {
            req.quantity_reserved = 0.0;
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        Ok(json!({ "work_order_id": order.id, "released": true }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// One consumption item that passed validation, ready to apply:
/// the position of its requirement on the work order, the
/// component, the quantity, and the requirement's unit.
#[derive(Debug, Clone)]
pub(crate) struct PlannedConsumption {
    pub position: usize,
    pub component_product_id: String,
    pub quantity: f64,
    pub unit: String,
}


/// Validate every consumption item before any mutation happens:
/// each component must belong to the work order, quantities must
/// be positive and (summed per component, so repeated lines for
/// the same component cannot slip past the remaining quantity)
/// within what remains, and any stated unit must match the
/// requirement's unit. Returns the validated plan in request
/// order. Because this runs as a first pass, a failure here
/// leaves reservations, consumed quantities, and emitted events
/// untouched; the caller applies the plan only after it succeeds.
pub(crate) fn plan_consumption(
    order: &WorkOrder,
    items: &[ConsumeItemInput],
) -> UnifiedResult<Vec<PlannedConsumption>> {
    let mut plan: Vec<PlannedConsumption> = Vec::with_capacity(items.len());
    let mut totals: HashMap<String, f64> = HashMap::new();
    for item in items {
        let component = require_text(
            &item.component_product_id,
            "component_product_id",
            MAX_CODE_LEN,
        )?;
        let quantity = valid_quantity(item.quantity, "quantity")?;
        let position = order
            .requirements
            .iter()
            .position(|req| req.component_product_id == component)
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Component is not part of this work order: {component}"
                ))
            })?;
        let req = &order.requirements[position];
        let remaining = req.quantity_required - req.quantity_consumed;
        let total = totals
            .get(&component)
            .copied()
            .unwrap_or(0.0)
            + quantity;
        if total > remaining {
            return Err(UnifiedError::conflict(format!(
                "Cannot consume {total} of {component}: only {remaining} remains on this work order"
            )));
        }
        totals.insert(component.clone(), total);
        if let Some(unit) = &item.unit {
            let unit = require_text(unit, "unit", MAX_UNIT_LEN)?;
            if unit != req.unit {
                return Err(UnifiedError::bad_request(format!(
                    "Unit mismatch for {component}: work order uses {}",
                    req.unit
                )));
            }
        }
        plan.push(PlannedConsumption {
            position,
            component_product_id: component,
            quantity,
            unit: req.unit.clone(),
        });
    }
    Ok(plan)
}


pub(crate) async fn consume_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ConsumeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let input = body.into_inner();
        if input.items.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one consumption item is required",
            ));
        }
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only in-progress work orders can consume components (current status: {})",
                order.status
            )));
        }
        // First pass: validate every item before mutating
        // anything, so a failure on a later item cannot leave
        // earlier items half-consumed (see plan_consumption).
        let plan = plan_consumption(&order, &input.items)?;
        // Second pass: apply the validated plan. No validation
        // failure can occur here, so reservations, consumed
        // quantities, and events stay consistent with the saved
        // work order.
        let mut consumed_rows: Vec<Value> = Vec::new();
        for planned in &plan {
            let component = planned.component_product_id.clone();
            let quantity = planned.quantity;
            let position = planned.position;
            let req = &mut order.requirements[position];
            req.quantity_consumed += quantity;
            let unit = req.unit.clone();
            // Draw down the matching reservations first.
            let mut left = quantity;
            let mut reservation_ids: Vec<String> = state
                .reservations
                .iter()
                .filter(|entry| {
                    let r = entry.value();
                    r.work_order_id == order.id
                        && r.component_product_id == component
                        && (r.status == "reserved" || r.status == "partially_consumed")
                })
                .map(|entry| entry.key().clone())
                .collect();
            reservation_ids.sort();
            for reservation_id in reservation_ids {
                if left <= 0.0 {
                    break;
                }
                if let Some(mut reservation) = state.reservations.get_mut(&reservation_id) {
                    let open = reservation.quantity_reserved - reservation.quantity_consumed;
                    let take = open.min(left);
                    if take > 0.0 {
                        reservation.quantity_consumed += take;
                        reservation.status = if reservation.quantity_consumed
                            >= reservation.quantity_reserved
                        {
                            "consumed".to_string()
                        } else {
                            "partially_consumed".to_string()
                        };
                        reservation.updated_at = now();
                        left -= take;
                    }
                }
            }
            state.emit(
                EVENT_COMPONENTS_CONSUMED,
                json!({
                    "work_order_id": order.id,
                    "number": order.number,
                    "component_product_id": component,
                    "quantity": quantity,
                    "unit": unit,
                    "consumed_by": caller.actor,
                }),
            );
            state.emit(
                EVENT_STOCK_DECREMENT_REQUESTED,
                json!({
                    "asset_id": component,
                    "quantity": quantity,
                    "reason": "manufacturing_consumption",
                    "work_order_id": order.id,
                    "product_id": order.product_id,
                }),
            );
            consumed_rows.push(json!({
                "component_product_id": component,
                "quantity": quantity,
                "unit": unit,
            }));
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        Ok(json!({ "work_order": order, "consumed": consumed_rows }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn produce_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ProduceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductionRun> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let input = body.into_inner();
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only in-progress work orders can record production (current status: {})",
                order.status
            )));
        }
        let produced = valid_quantity(input.quantity_produced, "quantity_produced")?;
        let scrapped = valid_non_negative(input.quantity_scrapped, "quantity_scrapped")?;
        let scrap_reason_id = match (scrapped > 0.0, &input.scrap_reason_code_id) {
            (true, Some(raw)) => {
                let reason = load_scrap_reason(&state, raw.trim())?;
                if !reason.active {
                    return Err(UnifiedError::bad_request(
                        "Scrap reason code is inactive",
                    ));
                }
                Some(reason.id)
            }
            (true, None) => {
                return Err(UnifiedError::bad_request(
                    "quantity_scrapped above zero requires scrap_reason_code_id",
                ));
            }
            (false, Some(_)) => {
                return Err(UnifiedError::bad_request(
                    "scrap_reason_code_id requires quantity_scrapped above zero",
                ));
            }
            (false, None) => None,
        };
        if let Some(center_id) = &input.work_center_id {
            load_work_center(&state, center_id.trim())?;
        }
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        if produced + scrapped > order.quantity_planned - order.quantity_produced - order.quantity_scrapped {
            return Err(UnifiedError::conflict(format!(
                "Produced plus scrapped exceeds the remaining planned quantity ({})",
                order.quantity_planned - order.quantity_produced - order.quantity_scrapped
            )));
        }
        let started = input.started_at.unwrap_or_else(now);
        let run = ProductionRun {
            id: new_id(),
            work_order_id: order.id.clone(),
            quantity_produced: produced,
            quantity_scrapped: scrapped,
            scrap_reason_code_id: scrap_reason_id.clone(),
            work_center_id: input.work_center_id.clone().map(|v| v.trim().to_string()),
            operator_id: caller.actor.clone(),
            status: "completed".to_string(),
            notes,
            started_at: started,
            completed_at: now(),
            created_at: now(),
        };
        order.quantity_produced += produced;
        order.quantity_scrapped += scrapped;
        order.updated_at = now();
        save_work_order(&state, &order);
        state
            .production_runs
            .insert(run.id.clone(), run.clone());
        state.emit(
            EVENT_GOODS_PRODUCED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "production_run_id": run.id,
                "product_id": order.product_id,
                "quantity": produced,
                "quantity_scrapped": scrapped,
                "operator_id": caller.actor,
            }),
        );
        if scrapped > 0.0 {
            state.emit(
                EVENT_SCRAP_RECORDED,
                json!({
                    "work_order_id": order.id,
                    "production_run_id": run.id,
                    "product_id": order.product_id,
                    "quantity_scrapped": scrapped,
                    "scrap_reason_code_id": scrap_reason_id,
                    "recorded_by": caller.actor,
                }),
            );
        }
        state.emit(
            EVENT_PRODUCTION_RUN_COMPLETED,
            json!({
                "production_run_id": run.id,
                "work_order_id": order.id,
                "quantity_produced": produced,
                "quantity_scrapped": scrapped,
            }),
        );
        Ok(run)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn complete_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only in-progress work orders can be completed (current status: {})",
                order.status
            )));
        }
        if order.quantity_produced <= 0.0 {
            return Err(UnifiedError::conflict(
                "A work order with no produced quantity cannot be completed; record a production run first",
            ));
        }
        // Quality blocking is advisory but binding here: a work
        // order a blocking quality alert covers must not complete
        // until the alert is resolved. The consultation is the
        // hook form of quality's `GET /blocks`; the refusal names
        // the blocking alert so the caller knows what to resolve.
        let blocks = quality_blocks_for(&state, &order.id, QUALITY_TARGET_KIND_WORK_ORDER);
        if let Some(block) = blocks.first() {
            let alert_id = block
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            let message = block
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("blocking quality alert");
            return Err(UnifiedError::conflict(format!(
                "Work order {} is blocked by quality alert {alert_id}: {message}",
                order.id
            ))
            .with_metadata("quality_block", json!(block)));
        }
        let settings = state.settings();
        let allowed = settings
            .work_order_transitions
            .get(&order.status)
            .cloned()
            .unwrap_or_default();
        if !allowed.contains(&"done".to_string()) {
            return Err(UnifiedError::conflict("Transition to done is not allowed by the configured transition map")
                .with_metadata("allowed", json!(allowed)));
        }
        let cost = compute_cost(&state, &order);
        order.status = "done".to_string();
        order.actual_end = Some(now());
        order.updated_at = now();
        save_work_order(&state, &order);
        state.emit(
            EVENT_WORK_ORDER_STATUS_CHANGED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "from_status": "in_progress",
                "to_status": "done",
                "changed_by": caller.actor,
            }),
        );
        state.emit(
            EVENT_INSPECTION_REQUESTED,
            json!({
                "work_order_id": order.id,
                "target_ref": order.id,
                "target_kind": QUALITY_TARGET_KIND_WORK_ORDER,
                "number": order.number,
                "product_id": order.product_id,
                "quantity": order.quantity_produced,
                "requested_by": caller.actor,
            }),
        );
        state.emit(
            EVENT_FINISHED_GOODS_COSTED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "product_id": order.product_id,
                "quantity_produced": order.quantity_produced,
                "cost": cost,
            }),
        );
        Ok(json!({ "work_order": order, "cost": cost }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_work_order_cost(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        Ok(compute_cost(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn list_work_order_reservations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<MaterialReservation> = state
            .reservations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reservation| reservation.work_order_id == order.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn list_work_order_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<ProductionRun> = state
            .production_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| run.work_order_id == order.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_order() -> WorkOrder {
        WorkOrder {
            id: "wo-1".to_string(),
            number: "WO-000001".to_string(),
            bom_id: "bom-1".to_string(),
            bom_version_id: "ver-1".to_string(),
            bom_version: 1,
            product_id: "prod-1".to_string(),
            quantity_planned: 10.0,
            quantity_produced: 0.0,
            quantity_scrapped: 0.0,
            status: "in_progress".to_string(),
            requirements: vec![
                WorkOrderRequirement {
                    component_product_id: "comp-a".to_string(),
                    quantity_required: 8.0,
                    unit: "piece".to_string(),
                    quantity_reserved: 0.0,
                    quantity_consumed: 2.0,
                    quantity_short: 0.0,
                },
                WorkOrderRequirement {
                    component_product_id: "comp-b".to_string(),
                    quantity_required: 5.0,
                    unit: "kg".to_string(),
                    quantity_reserved: 0.0,
                    quantity_consumed: 0.0,
                    quantity_short: 0.0,
                },
            ],
            planned_start: None,
            planned_end: None,
            actual_start: None,
            actual_end: None,
            company_id: None,
            notes: None,
            inspection_status: None,
            inspection_notes: None,
            created_by: "test".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn item(component: &str, quantity: f64, unit: Option<&str>) -> ConsumeItemInput {
        ConsumeItemInput {
            component_product_id: component.to_string(),
            quantity,
            unit: unit.map(|u| u.to_string()),
        }
    }

    #[test]
    fn plan_consumption_accepts_valid_items() {
        let order = test_order();
        let plan = plan_consumption(
            &order,
            &[item("comp-a", 3.0, Some("piece")), item("comp-b", 5.0, None)],
        )
        .expect("valid consumption");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].position, 0);
        assert_eq!(plan[0].quantity, 3.0);
        assert_eq!(plan[0].unit, "piece");
        assert_eq!(plan[1].position, 1);
        // The plan is a pure validation result: it must not mutate
        // the order it was planned from.
        assert_eq!(order.requirements[0].quantity_consumed, 2.0);
    }

    #[test]
    fn plan_consumption_rejects_unknown_component() {
        let order = test_order();
        assert!(plan_consumption(&order, &[item("comp-a", 1.0, None), item("comp-x", 1.0, None)])
            .is_err());
    }

    #[test]
    fn plan_consumption_rejects_over_consumption() {
        let order = test_order();
        // comp-a has 8 required and 2 already consumed: 6 remain.
        assert!(plan_consumption(&order, &[item("comp-a", 7.0, None)]).is_err());
        assert!(plan_consumption(&order, &[item("comp-a", 6.0, None)]).is_ok());
    }

    #[test]
    fn plan_consumption_rejects_repeated_lines_that_together_exceed_remaining() {
        let order = test_order();
        // Each line alone fits in the 6 remaining, but together
        // they ask for 7. Validating lines one at a time against
        // the unmutated remaining would accept both.
        assert!(
            plan_consumption(&order, &[item("comp-a", 4.0, None), item("comp-a", 3.0, None)])
                .is_err()
        );
        assert!(
            plan_consumption(&order, &[item("comp-a", 4.0, None), item("comp-a", 2.0, None)])
                .is_ok()
        );
    }

    #[test]
    fn plan_consumption_rejects_unit_mismatch_and_non_positive_quantity() {
        let order = test_order();
        assert!(
            plan_consumption(&order, &[item("comp-a", 1.0, Some("kg"))]).is_err()
        );
        assert!(plan_consumption(&order, &[item("comp-a", 0.0, None)]).is_err());
        assert!(plan_consumption(&order, &[item("comp-a", -1.0, None)]).is_err());
    }

    #[test]
    fn plan_consumption_failure_leaves_the_order_untouched() {
        // Atomicity contract: a plan that fails (here on the
        // second item) must not have consumed anything, because
        // the caller only applies a plan after it validates.
        let order = test_order();
        let result = plan_consumption(
            &order,
            &[item("comp-a", 1.0, None), item("comp-x", 1.0, None)],
        );
        assert!(result.is_err());
        assert_eq!(order.requirements[0].quantity_consumed, 2.0);
        assert_eq!(order.requirements[1].quantity_consumed, 0.0);
    }

    #[test]
    fn stock_check_reports_per_product_knowledge() {
        use unified_core::hooks::{HookBus, HookContext};
        let state = ManufacturingState::default();
        let bus = Arc::new(HookBus::new());
        // The inventory side answers for comp-a only; comp-b has
        // no level in the answer.
        bus.register(
            EVENT_STOCK_REQUESTED,
            "test",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({
                    "stock": { "comp-a": { "available": 10.0, "reserved": 0.0 } },
                }));
            }),
        );
        *state.hooks.write().expect("hooks lock poisoned") = Some(bus);
        let order = test_order();
        let view = check_stock_for_order(&state, &order, "test");
        assert_eq!(
            view.get("stock_answered").and_then(|v| v.as_bool()),
            Some(true)
        );
        let components = view
            .get("components")
            .and_then(|v| v.as_array())
            .expect("components array");
        assert_eq!(components.len(), 2);
        assert_eq!(
            components[0]
                .get("component_product_id")
                .and_then(|v| v.as_str()),
            Some("comp-a")
        );
        assert_eq!(
            components[0].get("stock_status").and_then(|v| v.as_str()),
            Some("known")
        );
        assert_eq!(
            components[1]
                .get("component_product_id")
                .and_then(|v| v.as_str()),
            Some("comp-b")
        );
        // comp-b was never answered for: it stays `unknown` even
        // though the inventory side answered for comp-a.
        assert_eq!(
            components[1].get("stock_status").and_then(|v| v.as_str()),
            Some("unknown")
        );
    }
}
