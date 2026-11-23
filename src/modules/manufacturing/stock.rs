//! manufacturing - stock.

use chrono::{ DateTime, Datelike, NaiveDate, Utc };
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



#[derive(Debug, Clone, Default, Serialize, Deserialize)]
// ---------------------------------------------------------------------------
// Stock and cost hooks (request / response over the bus)
// ---------------------------------------------------------------------------
/// One product's stock level as answered by the inventory side.
pub struct StockLevel {
    #[serde(default)]
    pub available: f64,
    #[serde(default)]
    pub reserved: f64,
    #[serde(default)]
    pub unit: Option<String>,
}


/// Ask the inventory side for stock levels. Returns the levels
/// plus whether any handler answered at all; when nothing
/// answers, the caller must treat availability as unknown.
pub(crate) fn query_stock(
    state: &ManufacturingState,
    product_ids: &[String],
) -> (HashMap<String, StockLevel>, bool) {
    let Some(bus) = state.bus() else {
        return (HashMap::new(), false);
    };
    let context = bus.emit(
        EVENT_STOCK_REQUESTED,
        json!({ "product_ids": product_ids }),
    );
    let mut levels: HashMap<String, StockLevel> = HashMap::new();
    let mut answered = false;
    if let Some(stock) = context.get("stock").and_then(|v| v.as_object()) {
        answered = true;
        for (product_id, value) in stock {
            if let Ok(level) = serde_json::from_value::<StockLevel>(value.clone()) {
                levels.insert(product_id.clone(), level);
            }
        }
    }
    for result in &context.results {
        if let Some(stock) = result.get("stock").and_then(|v| v.as_object()) {
            answered = true;
            for (product_id, value) in stock {
                if let Ok(level) = serde_json::from_value::<StockLevel>(value.clone()) {
                    levels.insert(product_id.clone(), level);
                }
            }
        }
        if let Some(product_id) = result.get("product_id").and_then(|v| v.as_str()) {
            if let Ok(level) = serde_json::from_value::<StockLevel>(result.clone()) {
                answered = true;
                levels.insert(product_id.to_string(), level);
            }
        }
    }
    (levels, answered)
}


/// Ask the calculator side for per-product unit costs. Returns
/// the answered costs plus whether any handler answered.
pub(crate) fn query_unit_costs(
    state: &ManufacturingState,
    items: &[(String, f64, String)],
    currency: &str,
) -> (HashMap<String, f64>, bool) {
    let Some(bus) = state.bus() else {
        return (HashMap::new(), false);
    };
    let payload_items: Vec<Value> = items
        .iter()
        .map(|(product_id, quantity, unit)| {
            json!({ "product_id": product_id, "quantity": quantity, "unit": unit })
        })
        .collect();
    let context = bus.emit(
        EVENT_COST_REQUESTED,
        json!({ "items": payload_items, "currency": currency }),
    );
    let mut costs: HashMap<String, f64> = HashMap::new();
    let mut answered = false;
    let read_costs = |value: &Value, costs: &mut HashMap<String, f64>, answered: &mut bool| {
        if let Some(map) = value.get("unit_costs").and_then(|v| v.as_object()) {
            *answered = true;
            for (product_id, cost) in map {
                if let Some(number) = cost.as_f64() {
                    costs.insert(product_id.clone(), number);
                }
            }
        }
        if let Some(product_id) = value.get("product_id").and_then(|v| v.as_str()) {
            if let Some(number) = value.get("unit_cost").and_then(|v| v.as_f64()) {
                *answered = true;
                costs.insert(product_id.to_string(), number);
            }
        }
    };
    read_costs(&context.data.clone(), &mut costs, &mut answered);
    for result in &context.results {
        read_costs(result, &mut costs, &mut answered);
    }
    (costs, answered)
}


/// Cost roll-up for a work order: component cost from hook
/// answers plus labor from work-center rates. Anything without
/// a price is listed in `gaps` and excluded from the totals;
/// the totals therefore cover only priced inputs and are
/// labelled as partial whenever gaps exist.
pub(crate) fn compute_cost(state: &ManufacturingState, order: &WorkOrder) -> Value {
    let settings = state.settings();
    let items: Vec<(String, f64, String)> = order
        .requirements
        .iter()
        .map(|req| {
            (
                req.component_product_id.clone(),
                req.quantity_required,
                req.unit.clone(),
            )
        })
        .collect();
    let (unit_costs, cost_answered) = query_unit_costs(state, &items, &settings.currency);
    let mut component_lines: Vec<Value> = Vec::new();
    let mut gaps: Vec<Value> = Vec::new();
    let mut component_total = 0.0;
    for req in &order.requirements {
        match unit_costs.get(&req.component_product_id) {
            Some(unit_cost) => {
                let line_total = round_money(req.quantity_required * unit_cost);
                component_total += line_total;
                component_lines.push(json!({
                    "component_product_id": req.component_product_id,
                    "quantity": req.quantity_required,
                    "unit": req.unit,
                    "unit_cost": round_money(*unit_cost),
                    "line_total": line_total,
                    "currency": settings.currency,
                }));
            }
            None => {
                gaps.push(json!({
                    "kind": "missing_component_cost",
                    "component_product_id": req.component_product_id,
                    "quantity": req.quantity_required,
                    "unit": req.unit,
                    "reason": if cost_answered {
                        "No cost handler answered for this product"
                    } else {
                        "No cost handler is registered for manufacturing.cost.requested"
                    },
                }));
            }
        }
    }
    let version = load_bom_version(state, &order.bom_version_id).ok();
    let mut labor_lines: Vec<Value> = Vec::new();
    let mut labor_total = 0.0;
    if let Some(version) = version {
        for step in &version.routing {
            let center = load_work_center(state, &step.work_center_id).ok();
            let hours = step.duration_minutes / 60.0 * order.quantity_planned;
            match center.and_then(|c| c.cost_per_hour) {
                Some(rate) => {
                    let line_total = round_money(hours * rate);
                    labor_total += line_total;
                    labor_lines.push(json!({
                        "work_center_id": step.work_center_id,
                        "step": step.name,
                        "hours": (hours * 100.0).round() / 100.0,
                        "rate_per_hour": round_money(rate),
                        "line_total": line_total,
                        "currency": settings.currency,
                    }));
                }
                None => {
                    gaps.push(json!({
                        "kind": "missing_labor_rate",
                        "work_center_id": step.work_center_id,
                        "step": step.name,
                        "hours": (hours * 100.0).round() / 100.0,
                        "reason": "Work center has no cost_per_hour configured",
                    }));
                }
            }
        }
    }
    let total = round_money(component_total + labor_total);
    let produced = order.quantity_produced;
    let unit_cost_value: Value = if produced > 0.0 {
        json!(round_money(total / produced))
    } else {
        Value::Null
    };
    json!({
        "work_order_id": order.id,
        "number": order.number,
        "currency": settings.currency,
        "quantity_planned": order.quantity_planned,
        "quantity_produced": produced,
        "component_cost": round_money(component_total),
        "labor_cost": round_money(labor_total),
        "total_cost": total,
        "unit_cost": unit_cost_value,
        "partial": !gaps.is_empty(),
        "components": component_lines,
        "labor": labor_lines,
        "gaps": gaps,
    })
}


/// Total minutes a routing step books for an order: the
/// per-unit duration scaled by the planned quantity. The
/// per-unit figure alone understates multi-unit orders, so
/// both the booked load and the scheduling cursor use this
/// scaled total.
pub(crate) fn scaled_step_minutes(step: &RoutingStep, quantity_planned: f64) -> f64 {
    step.duration_minutes * quantity_planned
}


/// Advance the scheduling cursor by a step's total minutes.
/// Milliseconds keep fractional durations exact; for whole
/// minutes this is identical to advancing by whole minutes.
fn advance_cursor(cursor: DateTime<Utc>, total_minutes: f64) -> DateTime<Utc> {
    cursor + chrono::Duration::milliseconds((total_minutes * 60_000.0).round() as i64)
}


/// Split a step's total minutes across the calendar days it
/// spans, starting at `start`. A step that crosses midnight
/// books the minutes before midnight on the start day and the
/// rest on the following day(s), so daily load and overload
/// flags reflect when the work actually happens instead of
/// piling a multi-day step onto its start day.
pub(crate) fn split_minutes_across_days(
    start: DateTime<Utc>,
    total_minutes: f64,
) -> Vec<(NaiveDate, f64)> {
    let mut segments: Vec<(NaiveDate, f64)> = Vec::new();
    if total_minutes <= 0.0 {
        return segments;
    }
    let mut remaining = total_minutes;
    let mut cursor = start;
    while remaining > 0.0 {
        let day = cursor.date_naive();
        let midnight = day
            .succ_opt()
            .and_then(|next| next.and_hms_opt(0, 0, 0))
            .map(|naive| naive.and_utc());
        let Some(midnight) = midnight else {
            break;
        };
        let until_midnight = (midnight - cursor).num_milliseconds() as f64 / 60_000.0;
        if until_midnight <= 0.0 {
            cursor = midnight;
            continue;
        }
        let take = remaining.min(until_midnight);
        segments.push((day, take));
        remaining -= take;
        if remaining <= 1e-9 {
            break;
        }
        cursor = midnight;
    }
    segments
}


/// Daily load of a work center over a date range: routing steps
/// of confirmed and in-progress work orders are laid out in
/// sequence from each order's planned start (or now when none is
/// set). Each step's total minutes (per-unit duration scaled by
/// the order's planned quantity) are split across the days the
/// step spans. Days whose load exceeds capacity are flagged
/// `overloaded`.
pub(crate) fn work_center_schedule(
    state: &ManufacturingState,
    center_id: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<Value> {
    let mut load_minutes: HashMap<NaiveDate, f64> = HashMap::new();
    for entry in state.work_orders.iter() {
        let order = entry.value();
        if order.status != "confirmed" && order.status != "in_progress" {
            continue;
        }
        let Ok(version) = load_bom_version(state, &order.bom_version_id) else {
            continue;
        };
        let mut cursor = order.planned_start.unwrap_or_else(now);
        for step in &version.routing {
            let total_minutes = scaled_step_minutes(step, order.quantity_planned);
            if step.work_center_id == center_id {
                for (day, minutes) in split_minutes_across_days(cursor, total_minutes) {
                    if day >= from && day <= to {
                        *load_minutes.entry(day).or_insert(0.0) += minutes;
                    }
                }
            }
            cursor = advance_cursor(cursor, total_minutes);
        }
    }
    let center = load_work_center(state, center_id).ok();
    let capacity_minutes = center
        .as_ref()
        .map(|c| c.capacity_hours_per_day * 60.0)
        .unwrap_or(0.0);
    let mut days: Vec<Value> = Vec::new();
    let mut day = from;
    while day <= to {
        let minutes = load_minutes.get(&day).copied().unwrap_or(0.0);
        days.push(json!({
            "date": day.to_string(),
            "scheduled_minutes": minutes,
            "capacity_minutes": capacity_minutes,
            "load_percent": if capacity_minutes > 0.0 {
                (minutes / capacity_minutes * 100.0 * 100.0).round() / 100.0
            } else {
                0.0
            },
            "overloaded": capacity_minutes > 0.0 && minutes > capacity_minutes,
        }));
        match day.succ_opt() {
            Some(next) => day = next,
            None => break,
        }
    }
    days
}


/// Overload warnings for confirming a work order: days in its
/// planned window where any routing work center would exceed
/// capacity once this order's steps are added. Each step's
/// total minutes (per-unit duration scaled by the planned
/// quantity) are split across the days the step spans, and
/// every spanned day is checked against capacity.
pub(crate) fn capacity_warnings(state: &ManufacturingState, order: &WorkOrder) -> Vec<Value> {
    let mut warnings: Vec<Value> = Vec::new();
    let Ok(version) = load_bom_version(state, &order.bom_version_id) else {
        return warnings;
    };
    let mut cursor = order.planned_start.unwrap_or_else(now);
    for step in &version.routing {
        let total_minutes = scaled_step_minutes(step, order.quantity_planned);
        for (day, minutes) in split_minutes_across_days(cursor, total_minutes) {
            let existing = work_center_schedule(state, &step.work_center_id, day, day);
            let used = existing
                .first()
                .and_then(|d| d.get("scheduled_minutes"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let capacity = existing
                .first()
                .and_then(|d| d.get("capacity_minutes"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if capacity > 0.0 && used + minutes > capacity {
                warnings.push(json!({
                    "work_center_id": step.work_center_id,
                    "step": step.name,
                    "date": day.to_string(),
                    "scheduled_minutes": used + minutes,
                    "capacity_minutes": capacity,
                    "message": "Routing load exceeds work center capacity on this day",
                }));
            }
        }
        cursor = advance_cursor(cursor, total_minutes);
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    fn start(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("valid test datetime")
    }

    fn step(sequence: u32, name: &str, duration_minutes: f64) -> RoutingStep {
        RoutingStep {
            sequence,
            name: name.to_string(),
            work_center_id: "wc-1".to_string(),
            duration_minutes,
            description: None,
        }
    }

    fn seed_order(state: &ManufacturingState, steps: Vec<RoutingStep>, quantity: f64, status: &str) {
        state.work_centers.insert(
            "wc-1".to_string(),
            WorkCenter {
                id: "wc-1".to_string(),
                name: "Cutting".to_string(),
                code: "WC-1".to_string(),
                capacity_hours_per_day: 8.0,
                cost_per_hour: None,
                status: "active".to_string(),
                description: None,
                created_at: now(),
                updated_at: now(),
            },
        );
        state.bom_versions.insert(
            "ver-1".to_string(),
            BomVersion {
                id: "ver-1".to_string(),
                bom_id: "bom-1".to_string(),
                version: 1,
                lines: vec![],
                routing: steps,
                created_by: "test".to_string(),
                created_at: now(),
            },
        );
        state.work_orders.insert(
            "wo-1".to_string(),
            WorkOrder {
                id: "wo-1".to_string(),
                number: "WO-000001".to_string(),
                bom_id: "bom-1".to_string(),
                bom_version_id: "ver-1".to_string(),
                bom_version: 1,
                product_id: "prod-1".to_string(),
                quantity_planned: quantity,
                quantity_produced: 0.0,
                quantity_scrapped: 0.0,
                status: status.to_string(),
                requirements: vec![],
                planned_start: Some(start(2026, 1, 5, 9, 0)),
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
            },
        );
    }

    fn scheduled_minutes(days: &[Value], date: &str) -> f64 {
        days.iter()
            .find(|d| d.get("date").and_then(|v| v.as_str()) == Some(date))
            .and_then(|d| d.get("scheduled_minutes"))
            .and_then(|v| v.as_f64())
            .expect("day present in schedule")
    }

    #[test]
    fn split_minutes_across_days_splits_at_midnight() {
        let segments = split_minutes_across_days(start(2026, 1, 5, 22, 0), 240.0);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], (day(2026, 1, 5), 120.0));
        assert_eq!(segments[1], (day(2026, 1, 6), 120.0));
    }

    #[test]
    fn split_minutes_across_days_keeps_single_day_step_whole() {
        let segments = split_minutes_across_days(start(2026, 1, 5, 9, 0), 120.0);
        assert_eq!(segments, vec![(day(2026, 1, 5), 120.0)]);
    }

    #[test]
    fn work_center_schedule_scales_cursor_by_quantity_and_splits_across_days() {
        // Two 60-minute steps for 10 units: each books 600 minutes.
        // Step 1 runs 09:00-19:00 on Jan 5 (600). Step 2 starts at
        // 19:00, runs 19:00-24:00 on Jan 5 (300) and 00:00-05:00
        // on Jan 6 (300). An unscaled cursor would lay both steps
        // out from 09:00/10:00 and pile all 1200 minutes on Jan 5.
        let state = ManufacturingState::default();
        seed_order(
            &state,
            vec![step(1, "Cut", 60.0), step(2, "Finish", 60.0)],
            10.0,
            "confirmed",
        );
        let days = work_center_schedule(&state, "wc-1", day(2026, 1, 5), day(2026, 1, 6));
        assert_eq!(scheduled_minutes(&days, "2026-01-05"), 900.0);
        assert_eq!(scheduled_minutes(&days, "2026-01-06"), 300.0);
    }

    #[test]
    fn work_center_schedule_single_step_books_scaled_load() {
        let state = ManufacturingState::default();
        seed_order(&state, vec![step(1, "Cut", 60.0)], 10.0, "confirmed");
        let days = work_center_schedule(&state, "wc-1", day(2026, 1, 5), day(2026, 1, 6));
        assert_eq!(scheduled_minutes(&days, "2026-01-05"), 600.0);
        assert_eq!(scheduled_minutes(&days, "2026-01-06"), 0.0);
    }

    #[test]
    fn capacity_warnings_use_scaled_load_per_spanned_day() {
        // One 60-minute step for 20 units = 1200 minutes from
        // 09:00 Jan 5: 900 on Jan 5 (over the 480 capacity) and
        // 300 on Jan 6 (under it). Only Jan 5 warns, with the
        // split 900 minutes, not the whole 1200.
        let state = ManufacturingState::default();
        seed_order(&state, vec![step(1, "Cut", 60.0)], 20.0, "draft");
        let order = load_work_order(&state, "wo-1").expect("seeded order");
        let warnings = capacity_warnings(&state, &order);
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].get("date").and_then(|v| v.as_str()),
            Some("2026-01-05")
        );
        assert_eq!(
            warnings[0]
                .get("scheduled_minutes")
                .and_then(|v| v.as_f64()),
            Some(900.0)
        );
    }
}
