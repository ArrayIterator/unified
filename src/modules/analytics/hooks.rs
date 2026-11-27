//! analytics - hooks.

use actix_web::{ http::header };
use chrono::{ DateTime, Utc };
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{ Arc, Mutex };
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*, module::*};



// Hook listeners: ingest events from other modules into local records.
pub(crate) fn record_sale_from_order(state: &Arc<AnalyticsState>, order: &Value, kind: &str) {
    let Some(order_id) = order.get("id").and_then(|value| value.as_str()) else {
        return;
    };
    if state.sales.iter().any(|entry| {
        entry.value().order_id.as_deref() == Some(order_id) && entry.value().kind == kind
    }) {
        return;
    }
    let currency = order
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    // The order event carries major units; the record stores
    // integer minor units, converted currency-aware at this
    // boundary (zero-decimal currencies keep whole units).
    let total = major_to_minor(
        order.get("total").and_then(|value| value.as_f64()).unwrap_or(0.0),
        &currency,
    );
    let products: Vec<String> = order
        .get("items")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(|value| value.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let company_id = order
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let recorded_at = order
        .get("updated_at")
        .and_then(|value| value.as_str())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or_else(now);
    let record = SaleRecord {
        id: new_id(),
        order_id: Some(order_id.to_string()),
        kind: kind.to_string(),
        total,
        currency,
        products,
        company_id,
        recorded_at,
        created_at: now(),
    };
    state.sales.insert(record.id.clone(), record);
}


pub(crate) fn upsert_license(state: &Arc<AnalyticsState>, data: &Value, status: &str) {
    let Some(license_id) = data
        .get("license_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            data.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
    else {
        return;
    };
    let product = data
        .get("product")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let license_key = data
        .get("license_key")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match state.licenses.get_mut(&license_id) {
        Some(mut entry) => {
            entry.status = status.to_string();
            if !product.is_empty() {
                entry.product = product;
            }
            if license_key.is_some() {
                entry.license_key = license_key;
            }
            entry.last_event_at = now();
        }
        None => {
            let snapshot = LicenseSnapshot {
                license_id: license_id.clone(),
                license_key,
                product,
                status: status.to_string(),
                last_event_at: now(),
                created_at: now(),
            };
            state.licenses.insert(license_id, snapshot);
        }
    }
}


pub(crate) fn record_metric(state: &Arc<AnalyticsState>, kind: &str, source: Option<String>) {
    let sample = MetricSample {
        id: new_id(),
        kind: kind.to_string(),
        source,
        recorded_at: now(),
        created_at: now(),
    };
    state.metrics.insert(sample.id.clone(), sample);
}


pub(crate) fn on_store_order_completed(state: Arc<AnalyticsState>, data: &Value) {
    record_sale_from_order(&state, data, "sale");
}


pub(crate) fn on_store_order_refunded(state: Arc<AnalyticsState>, data: &Value) {
    if let Some(order) = data.get("order") {
        record_sale_from_order(&state, order, "refund");
    }
}


pub(crate) fn on_cluster_health_changed(state: Arc<AnalyticsState>, data: &Value) {
    let Some(node_id) = data.get("node_id").and_then(|value| value.as_str()) else {
        return;
    };
    let status = data
        .get("health")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    if !matches!(status.as_str(), "healthy" | "degraded" | "unhealthy" | "unknown") {
        return;
    }
    let sample = HealthSample {
        id: new_id(),
        node_id: node_id.to_string(),
        cluster_id: data
            .get("cluster_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        status,
        latency_ms: data.get("latency_ms").and_then(|value| value.as_u64()),
        checked_at: now(),
        created_at: now(),
    };
    state.health.insert(sample.id.clone(), sample);
}


pub(crate) fn on_task_event(state: Arc<AnalyticsState>, data: &Value) {
    let Some(task_id) = data
        .get("task_id")
        .and_then(|value| value.as_str())
        .or_else(|| data.get("id").and_then(|value| value.as_str()))
    else {
        return;
    };
    let status = data
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("todo")
        .to_string();
    state.tasks.insert(
        task_id.to_string(),
        TaskMirror {
            task_id: task_id.to_string(),
            status,
            updated_at: now(),
        },
    );
}


pub(crate) fn on_planner_pool_created(state: Arc<AnalyticsState>, data: &Value) {
    let Some(pool_id) = data.get("id").and_then(|value| value.as_str()) else {
        return;
    };
    let capacity = data
        .get("capacity")
        .map(PoolResources::from_value)
        .unwrap_or_default();
    let pool_name = data
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    match state.pools.get_mut(pool_id) {
        Some(mut entry) => {
            entry.capacity = capacity;
            entry.pool_name = pool_name;
        }
        None => {
            state.pools.insert(
                pool_id.to_string(),
                PoolMirror {
                    pool_id: pool_id.to_string(),
                    pool_name,
                    capacity,
                    allocations: Arc::new(Mutex::new(HashMap::new())),
                },
            );
        }
    }
}


pub(crate) fn on_planner_plan_event(state: Arc<AnalyticsState>, data: &Value, active: bool) {
    let Some(pool_id) = data.get("pool_id").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(plan_id) = data.get("id").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(pool) = state.pools.get(pool_id) else {
        return;
    };
    let mut allocations = pool.allocations.lock().expect("pool allocations lock poisoned");
    if active {
        let reserved = data
            .get("reserved")
            .map(PoolResources::from_value)
            .unwrap_or_default();
        allocations.insert(plan_id.to_string(), reserved);
    } else {
        allocations.remove(plan_id);
    }
}


/// Run a report when `analytics.report.run_requested` arrives (the hook
/// the scheduler dispatches `report.generate` jobs through). Emits
/// `analytics.report.generated` with the finished run.
pub(crate) fn on_report_run_requested(state: Arc<AnalyticsState>, data: &Value) {
    let Some(definition_id) = data.get("definition_id").and_then(|value| value.as_str()) else {
        return;
    };
    // No caller on a scheduler trigger: the run aggregates
    // global (unscoped) sales only -- an unresolved scope never
    // reads another company's sales (C3 pilot 3).
    if let Ok(run) = run_report(&state, definition_id, &None) {
        state.bus().emit(
            ANALYTICS_REPORT_GENERATED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
    }
}
