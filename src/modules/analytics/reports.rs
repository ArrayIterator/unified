//! analytics - reports.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{ http::header };
use chrono::{ DateTime, Utc };
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, hooks::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*, module::*};



// ---------------------------------------------------------------------------
// Report computation.
// ---------------------------------------------------------------------------
pub(crate) const VALID_METRICS: [&str; 6] = [

    "revenue",
    "refunds",
    "licenses",
    "usage",
    "uptime",
    "sales_by_product",
];


pub(crate) const VALID_DIMENSIONS: [&str; 5] = ["day", "product", "currency", "status", "kind"];


pub(crate) fn validate_metrics(metrics: &[String]) -> UnifiedResult<()> {
    if metrics.is_empty() {
        return Err(UnifiedError::bad_request("metrics must not be empty"));
    }
    for metric in metrics {
        if !VALID_METRICS.contains(&metric.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown metric \"{metric}\". Known metrics: {}",
                VALID_METRICS.join(", ")
            )));
        }
    }
    Ok(())
}


pub(crate) fn validate_dimensions(dimensions: &[String]) -> UnifiedResult<()> {
    for dimension in dimensions {
        if !VALID_DIMENSIONS.contains(&dimension.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown dimension \"{dimension}\". Known dimensions: {}",
                VALID_DIMENSIONS.join(", ")
            )));
        }
    }
    Ok(())
}


pub(crate) fn resolve_definition_range(definition: &ReportDefinition) -> UnifiedResult<(DateTime<Utc>, DateTime<Utc>)> {
    match (definition.start, definition.end) {
        (Some(start), Some(end)) => {
            if start >= end {
                return Err(UnifiedError::bad_request("start must be before end"));
            }
            Ok((start, end))
        }
        _ => {
            let preset = definition.range_preset.as_deref().unwrap_or("30d");
            let end = now();
            let days: i64 = match preset {
                "7d" => 7,
                "30d" => 30,
                "90d" => 90,
                "365d" => 365,
                _ => {
                    return Err(UnifiedError::bad_request(
                        "range_preset must be one of: 7d, 30d, 90d, 365d",
                    ))
                }
            };
            Ok((end - chrono::Duration::days(days), end))
        }
    }
}


pub(crate) fn filter_currency(definition: &ReportDefinition) -> Option<String> {
    definition
        .filters
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}


pub(crate) fn filter_product(definition: &ReportDefinition) -> Option<String> {
    definition
        .filters
        .get("product")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}


pub(crate) fn revenue_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    company: &Option<String>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    refunds_only: bool,
) -> Vec<Value> {
    let by_day = definition.dimensions.iter().any(|dimension| dimension == "day");
    let by_currency = definition.dimensions.iter().any(|dimension| dimension == "currency");
    let by_product = definition.dimensions.iter().any(|dimension| dimension == "product");
    let currency_filter = filter_currency(definition);
    let product_filter = filter_product(definition);
    // Group key -> (gross, refunds, orders, refund_count), all
    // money in integer minor units. `currencies` remembers the
    // currency of each group when the `currency` dimension is on
    // (it is part of the key then, so every sale in the group
    // shares it); row values are converted to major units with
    // it at the boundary. Groups without a currency dimension
    // span currencies, as before, and convert with the standard
    // two-decimal fallback.
    let mut groups: HashMap<String, (i64, i64, u64, u64)> = HashMap::new();
    let mut currencies: HashMap<String, String> = HashMap::new();
    let mut labels: HashMap<String, Value> = HashMap::new();
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.recorded_at < start || sale.recorded_at > end {
            continue;
        }
        // Tenant scope (C3 pilot 3): a company caller aggregates
        // only their own company's sales, an unresolved caller only
        // global (unscoped) sales -- never every company's sales.
        if !in_company_scope(&sale.company_id, company) {
            continue;
        }
        if let Some(currency) = &currency_filter {
            if sale.currency != *currency {
                continue;
            }
        }
        if let Some(product) = &product_filter {
            if !sale.products.iter().any(|name| name == product) {
                continue;
            }
        }
        if refunds_only && sale.kind != "refund" {
            continue;
        }
        if !refunds_only && sale.kind != "sale" && sale.kind != "refund" {
            continue;
        }
        let product = sale.products.first().cloned().unwrap_or_default();
        let mut key_parts: Vec<String> = Vec::new();
        let mut label = Map::new();
        if by_day {
            key_parts.push(day_key(sale.recorded_at));
            label.insert("day".to_string(), Value::String(day_key(sale.recorded_at)));
        }
        if by_currency {
            key_parts.push(sale.currency.clone());
            label.insert("currency".to_string(), Value::String(sale.currency.clone()));
        }
        if by_product {
            key_parts.push(product.clone());
            label.insert("product".to_string(), Value::String(product));
        }
        if key_parts.is_empty() {
            key_parts.push("all".to_string());
        }
        let key = key_parts.join("|");
        labels.entry(key.clone()).or_insert_with(|| Value::Object(label));
        if by_currency {
            currencies
                .entry(key.clone())
                .or_insert_with(|| sale.currency.clone());
        }
        let group = groups.entry(key).or_insert((0, 0, 0, 0));
        if sale.kind == "sale" {
            group.0 += sale.total;
            group.2 += 1;
        } else {
            group.1 += sale.total;
            group.3 += 1;
        }
    }
    let mut rows: Vec<Value> = Vec::new();
    for (key, (gross, refunds, orders, refund_count)) in groups {
        let currency = currencies.get(&key).map(String::as_str).unwrap_or("USD");
        let mut row = labels.remove(&key).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut row {
            map.insert("metric".to_string(), Value::String(if refunds_only { "refunds".to_string() } else { "revenue".to_string() }));
            map.insert("gross".to_string(), Value::from(minor_to_major(gross, currency)));
            map.insert("refunds".to_string(), Value::from(minor_to_major(refunds, currency)));
            map.insert("net".to_string(), Value::from(minor_to_major(gross - refunds, currency)));
            map.insert("orders".to_string(), Value::from(orders));
            map.insert("refund_count".to_string(), Value::from(refund_count));
        }
        rows.push(row);
    }
    rows.sort_by(|a, b| {
        a.get("day")
            .and_then(|value| value.as_str())
            .cmp(&b.get("day").and_then(|value| value.as_str()))
    });
    rows
}


pub(crate) fn license_rows(state: &AnalyticsState, definition: &ReportDefinition) -> Vec<Value> {
    let by_status = definition.dimensions.iter().any(|dimension| dimension == "status");
    let product_filter = filter_product(definition);
    let status_filter = definition
        .filters
        .get("status")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut groups: HashMap<String, (String, String, u64)> = HashMap::new();
    for entry in state.licenses.iter() {
        let license = entry.value();
        if let Some(product) = &product_filter {
            if license.product != *product {
                continue;
            }
        }
        if let Some(status) = &status_filter {
            if license.status != *status {
                continue;
            }
        }
        let key = if by_status {
            format!("{}|{}", license.product, license.status)
        } else {
            license.product.clone()
        };
        let group = groups
            .entry(key)
            .or_insert_with(|| (license.product.clone(), license.status.clone(), 0));
        group.2 += 1;
    }
    let mut rows: Vec<Value> = groups
        .into_values()
        .map(|(product, status, count)| {
            let mut row = json!({
                "metric": "licenses",
                "product": product,
                "count": count,
            });
            if by_status {
                row["status"] = Value::String(status);
            }
            row
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("product")
            .and_then(|value| value.as_str())
            .cmp(&b.get("product").and_then(|value| value.as_str()))
    });
    rows
}


pub(crate) fn usage_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Value> {
    let by_day = definition.dimensions.iter().any(|dimension| dimension == "day");
    let by_kind = definition.dimensions.iter().any(|dimension| dimension == "kind");
    let kind_filter = definition
        .filters
        .get("kind")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut groups: HashMap<String, (Option<String>, Option<String>, u64)> = HashMap::new();
    for entry in state.metrics.iter() {
        let sample = entry.value();
        if sample.recorded_at < start || sample.recorded_at > end {
            continue;
        }
        if let Some(kind) = &kind_filter {
            if sample.kind != *kind {
                continue;
            }
        }
        let mut key_parts: Vec<String> = Vec::new();
        let mut day: Option<String> = None;
        let mut kind: Option<String> = None;
        if by_day {
            day = Some(day_key(sample.recorded_at));
            key_parts.push(day.clone().unwrap_or_default());
        }
        if by_kind {
            kind = Some(sample.kind.clone());
            key_parts.push(sample.kind.clone());
        }
        if key_parts.is_empty() {
            key_parts.push("all".to_string());
        }
        let group = groups
            .entry(key_parts.join("|"))
            .or_insert((day, kind, 0));
        group.2 += 1;
    }
    let mut rows: Vec<Value> = groups
        .into_values()
        .map(|(day, kind, count)| {
            let mut row = json!({ "metric": "usage", "count": count });
            if let Some(day) = day {
                row["day"] = Value::String(day);
            }
            if let Some(kind) = kind {
                row["kind"] = Value::String(kind);
            }
            row
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("day")
            .and_then(|value| value.as_str())
            .cmp(&b.get("day").and_then(|value| value.as_str()))
    });
    rows
}


pub(crate) fn uptime_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Value> {
    let node_filter = definition
        .filters
        .get("node_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let cluster_filter = definition
        .filters
        .get("cluster_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut groups: HashMap<String, (u64, u64)> = HashMap::new();
    for entry in state.health.iter() {
        let sample = entry.value();
        if sample.checked_at < start || sample.checked_at > end {
            continue;
        }
        if let Some(node_id) = &node_filter {
            if sample.node_id != *node_id {
                continue;
            }
        }
        if let Some(cluster_id) = &cluster_filter {
            if sample.cluster_id.as_deref() != Some(cluster_id.as_str()) {
                continue;
            }
        }
        let group = groups.entry(day_key(sample.checked_at)).or_insert((0, 0));
        group.0 += 1;
        if sample.status == "healthy" {
            group.1 += 1;
        }
    }
    let mut rows: Vec<Value> = groups
        .into_iter()
        .map(|(day, (checks, healthy))| {
            let uptime = if checks > 0 {
                Some((healthy as f64 / checks as f64 * 10000.0).round() / 100.0)
            } else {
                None
            };
            json!({
                "metric": "uptime",
                "day": day,
                "checks": checks,
                "healthy": healthy,
                "uptime_pct": uptime,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("day")
            .and_then(|value| value.as_str())
            .cmp(&b.get("day").and_then(|value| value.as_str()))
    });
    rows
}


pub(crate) fn sales_by_product_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    company: &Option<String>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Value> {
    let currency_filter = filter_currency(definition);
    // Product -> (gross in integer minor units, orders). Rows
    // convert to major units at the boundary with the filtered
    // currency when one is set, else the two-decimal fallback
    // (groups span currencies, as before).
    let mut groups: HashMap<String, (i64, u64)> = HashMap::new();
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.kind != "sale" || sale.recorded_at < start || sale.recorded_at > end {
            continue;
        }
        // Tenant scope (C3 pilot 3): see `revenue_rows`.
        if !in_company_scope(&sale.company_id, company) {
            continue;
        }
        if let Some(currency) = &currency_filter {
            if sale.currency != *currency {
                continue;
            }
        }
        for product in &sale.products {
            let group = groups.entry(product.clone()).or_insert((0, 0));
            group.0 += sale.total;
            group.1 += 1;
        }
    }
    let currency = currency_filter.as_deref().unwrap_or("USD");
    let mut rows: Vec<Value> = groups
        .into_iter()
        .map(|(product, (gross, orders))| {
            json!({
                "metric": "sales_by_product",
                "product": product,
                "gross": minor_to_major(gross, currency),
                "orders": orders,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.get("gross")
            .and_then(|value| value.as_f64())
            .partial_cmp(&a.get("gross").and_then(|value| value.as_f64()))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}


pub(crate) fn compute_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    company: &Option<String>,
) -> UnifiedResult<Vec<Value>> {
    let (start, end) = resolve_definition_range(definition)?;
    let mut rows: Vec<Value> = Vec::new();
    for metric in &definition.metrics {
        match metric.as_str() {
            "revenue" => rows.extend(revenue_rows(state, definition, company, start, end, false)),
            "refunds" => rows.extend(revenue_rows(state, definition, company, start, end, true)),
            "licenses" => rows.extend(license_rows(state, definition)),
            "usage" => rows.extend(usage_rows(state, definition, start, end)),
            "uptime" => rows.extend(uptime_rows(state, definition, start, end)),
            "sales_by_product" => rows.extend(sales_by_product_rows(state, definition, company, start, end)),
            other => {
                return Err(UnifiedError::bad_request(format!("Unknown metric \"{other}\"")));
            }
        }
    }
    Ok(rows)
}


/// Run a report definition, aggregating only the sales in
/// `company` scope (see `revenue_rows`): a company caller sees
/// their own company's sales, an unresolved caller (`None`) sees
/// global sales only. License, usage, and uptime rows carry no
/// company field, so those metrics are gate-only (see the pilot-3
/// results note).
pub(crate) fn run_report(
    state: &AnalyticsState,
    definition_id: &str,
    company: &Option<String>,
) -> UnifiedResult<ReportRun> {
    let definition = state.definition(definition_id)?;
    let started_at = now();
    match compute_rows(state, &definition, company) {
        Ok(rows) => {
            let run = ReportRun {
                id: new_id(),
                definition_id: definition.id.clone(),
                status: "completed".to_string(),
                started_at,
                finished_at: Some(now()),
                row_count: rows.len(),
                error: None,
                rows,
            };
            state.runs.insert(run.id.clone(), run.clone());
            Ok(run)
        }
        Err(error) => {
            let run = ReportRun {
                id: new_id(),
                definition_id: definition.id.clone(),
                status: "failed".to_string(),
                started_at,
                finished_at: Some(now()),
                row_count: 0,
                error: Some(error.message.clone()),
                rows: Vec::new(),
            };
            state.runs.insert(run.id.clone(), run.clone());
            Err(error)
        }
    }
}
