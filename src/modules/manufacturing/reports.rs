//! manufacturing - reports.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{ Datelike, NaiveDate };
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{ atomic::{ Ordering }, Arc };
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*};



// Reports
pub(crate) async fn report_work_orders_by_status(
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
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        let mut groups: HashMap<String, (u64, f64, f64, f64)> = HashMap::new();
        for entry in state.work_orders.iter() {
            let order = entry.value();
            if !in_period(order.created_at, from, to) {
                continue;
            }
            let slot = groups.entry(order.status.clone()).or_insert((0, 0.0, 0.0, 0.0));
            slot.0 += 1;
            slot.1 += order.quantity_planned;
            slot.2 += order.quantity_produced;
            slot.3 += order.quantity_scrapped;
        }
        let mut rows: Vec<Value> = groups
            .into_iter()
            .map(|(status, (count, planned, produced, scrapped))| {
                json!({
                    "status": status,
                    "work_order_count": count,
                    "quantity_planned": planned,
                    "quantity_produced": produced,
                    "quantity_scrapped": scrapped,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.get("status")
                .and_then(|v| v.as_str())
                .cmp(&b.get("status").and_then(|v| v.as_str()))
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) fn period_key(date: NaiveDate, group_by: &str) -> String {
    match group_by {
        "week" => {
            let weekday = date.weekday().num_days_from_monday();
            let monday = date - chrono::Duration::days(i64::from(weekday));
            format!("{}-W{}", monday.iso_week().year(), monday.iso_week().week())
        }
        "month" => date.format("%Y-%m").to_string(),
        _ => date.to_string(),
    }
}


pub(crate) async fn report_production_output(
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
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        let group_by = params
            .filters
            .get("group_by")
            .map(|raw| raw.trim().to_lowercase())
            .unwrap_or_else(|| "day".to_string());
        if !["day", "week", "month"].contains(&group_by.as_str()) {
            return Err(UnifiedError::bad_request(
                "group_by must be day, week, or month",
            ));
        }
        // (period, product) -> (produced, scrapped, runs)
        let mut groups: HashMap<(String, String), (f64, f64, u64)> = HashMap::new();
        for entry in state.production_runs.iter() {
            let run = entry.value();
            if !in_period(run.completed_at, from, to) {
                continue;
            }
            let order = match state.work_orders.get(&run.work_order_id) {
                Some(order) => order.value().clone(),
                None => continue,
            };
            let key = (
                period_key(run.completed_at.date_naive(), &group_by),
                order.product_id.clone(),
            );
            let slot = groups.entry(key).or_insert((0.0, 0.0, 0));
            slot.0 += run.quantity_produced;
            slot.1 += run.quantity_scrapped;
            slot.2 += 1;
        }
        let mut rows: Vec<Value> = groups
            .into_iter()
            .map(|((period, product_id), (produced, scrapped, runs))| {
                json!({
                    "period": period,
                    "product_id": product_id,
                    "quantity_produced": produced,
                    "quantity_scrapped": scrapped,
                    "production_runs": runs,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            let by_period = a
                .get("period")
                .and_then(|v| v.as_str())
                .cmp(&b.get("period").and_then(|v| v.as_str()));
            if by_period == std::cmp::Ordering::Equal {
                a.get("product_id")
                    .and_then(|v| v.as_str())
                    .cmp(&b.get("product_id").and_then(|v| v.as_str()))
            } else {
                by_period
            }
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn report_component_usage(
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
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        let mut totals: HashMap<String, (f64, String, u64)> = HashMap::new();
        for entry in state.work_orders.iter() {
            let order = entry.value();
            if !in_period(order.updated_at, from, to) {
                continue;
            }
            for req in &order.requirements {
                if req.quantity_consumed <= 0.0 {
                    continue;
                }
                let slot = totals
                    .entry(req.component_product_id.clone())
                    .or_insert((0.0, req.unit.clone(), 0));
                slot.0 += req.quantity_consumed;
                slot.2 += 1;
            }
        }
        let mut rows: Vec<Value> = totals
            .into_iter()
            .map(|(product_id, (consumed, unit, orders))| {
                json!({
                    "component_product_id": product_id,
                    "unit": unit,
                    "quantity_consumed": consumed,
                    "work_orders": orders,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.get("component_product_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("component_product_id").and_then(|v| v.as_str()))
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn report_scrap_rate(
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
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        // Per product: (produced, scrapped).
        let mut per_product: HashMap<String, (f64, f64)> = HashMap::new();
        let mut total_produced = 0.0;
        let mut total_scrapped = 0.0;
        for entry in state.production_runs.iter() {
            let run = entry.value();
            if !in_period(run.completed_at, from, to) {
                continue;
            }
            let order = match state.work_orders.get(&run.work_order_id) {
                Some(order) => order.value().clone(),
                None => continue,
            };
            let slot = per_product
                .entry(order.product_id.clone())
                .or_insert((0.0, 0.0));
            slot.0 += run.quantity_produced;
            slot.1 += run.quantity_scrapped;
            total_produced += run.quantity_produced;
            total_scrapped += run.quantity_scrapped;
        }
        let overall_total = total_produced + total_scrapped;
        let overall_rate = if overall_total > 0.0 {
            total_scrapped / overall_total * 100.0
        } else {
            0.0
        };
        let mut rows: Vec<Value> = per_product
            .into_iter()
            .map(|(product_id, (produced, scrapped))| {
                let total = produced + scrapped;
                json!({
                    "product_id": product_id,
                    "quantity_produced": produced,
                    "quantity_scrapped": scrapped,
                    "scrap_rate_percent": if total > 0.0 {
                        (scrapped / total * 100.0 * 100.0).round() / 100.0
                    } else {
                        0.0
                    },
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.get("product_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("product_id").and_then(|v| v.as_str()))
        });
        Ok(json!({
            "from": from.map(|d| d.to_string()),
            "to": to.map(|d| d.to_string()),
            "quantity_produced": total_produced,
            "quantity_scrapped": total_scrapped,
            "scrap_rate_percent": (overall_rate * 100.0).round() / 100.0,
            "by_product": paginate(rows, &params).to_json(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
