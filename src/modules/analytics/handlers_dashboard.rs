//! analytics - handlers dashboard.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use serde_json::{ json, Value };
use std::collections::HashMap;
use std::sync::Arc;
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, hooks::*, reports::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*, module::*};
use super::helpers::{matches_search};



// ---------------------------------------------------------------------------
// Dashboard and summary handlers.
// ---------------------------------------------------------------------------
pub(crate) async fn get_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let end = now();
    let start = end - chrono::Duration::days(30);
    // Money sums are integer minor units; the view maps carry
    // major units, converted per currency at the boundary below.
    let mut revenue_30d: HashMap<String, i64> = HashMap::new();
    let mut refunds_30d: HashMap<String, i64> = HashMap::new();
    let mut orders = 0u64;
    for entry in state.sales.iter() {
        let sale = entry.value();
        // Tenant scope: only the caller's own company's sales feed
        // this dashboard (an unresolved caller sees global sales
        // only). Licenses, health, tasks, and pools carry no company
        // field and stay gate-only (see the pilot-3 results note).
        if !in_company_scope(&sale.company_id, &_scope) {
            continue;
        }
        if sale.recorded_at < start || sale.recorded_at > end {
            continue;
        }
        if sale.kind == "sale" {
            *revenue_30d.entry(sale.currency.clone()).or_insert(0) += sale.total;
            orders += 1;
        } else if sale.kind == "refund" {
            *refunds_30d.entry(sale.currency.clone()).or_insert(0) += sale.total;
        }
    }
    let revenue_30d: HashMap<String, f64> = revenue_30d
        .into_iter()
        .map(|(currency, minor)| (currency.clone(), minor_to_major(minor, &currency)))
        .collect();
    let refunds_30d: HashMap<String, f64> = refunds_30d
        .into_iter()
        .map(|(currency, minor)| (currency.clone(), minor_to_major(minor, &currency)))
        .collect();
    let mut licenses_by_status: HashMap<String, u64> = HashMap::new();
    let mut active_licenses = 0u64;
    for entry in state.licenses.iter() {
        let license = entry.value();
        *licenses_by_status.entry(license.status.clone()).or_insert(0) += 1;
        if license.status == "active" {
            active_licenses += 1;
        }
    }
    let mut checks = 0u64;
    let mut healthy = 0u64;
    for entry in state.health.iter() {
        let sample = entry.value();
        if sample.checked_at < start || sample.checked_at > end {
            continue;
        }
        checks += 1;
        if sample.status == "healthy" {
            healthy += 1;
        }
    }
    let uptime_pct_30d = if checks > 0 {
        Some((healthy as f64 / checks as f64 * 10000.0).round() / 100.0)
    } else {
        None
    };
    let open_tasks = state
        .tasks
        .iter()
        .filter(|entry| {
            matches!(
                entry.value().status.as_str(),
                "todo" | "in_progress" | "review" | "blocked"
            )
        })
        .count() as u64;
    let mut pool_views: Vec<PoolHeadroomView> = Vec::new();
    let mut pools_overcommitted = 0u64;
    let mut lowest: Option<f64> = None;
    for entry in state.pools.iter() {
        let pool = entry.value();
        let allocations = pool.allocations.lock().expect("pool allocations lock poisoned");
        let allocated = PoolResources::default().total_allocated(&allocations);
        let mut worst: Option<f64> = None;
        let mut overcommitted = false;
        for (used, capacity) in [
            (allocated.cpu_cores, pool.capacity.cpu_cores),
            (allocated.ram_gb, pool.capacity.ram_gb),
            (allocated.disk_gb, pool.capacity.disk_gb),
            (allocated.bandwidth_tb, pool.capacity.bandwidth_tb),
        ] {
            if capacity > 0.0 {
                let headroom = ((capacity - used) / capacity * 10000.0).round() / 100.0;
                worst = Some(worst.map_or(headroom, |current: f64| current.min(headroom)));
                if used > capacity {
                    overcommitted = true;
                }
            }
        }
        if overcommitted {
            pools_overcommitted += 1;
        }
        if let Some(headroom) = worst {
            lowest = Some(lowest.map_or(headroom, |current: f64| current.min(headroom)));
        }
        pool_views.push(PoolHeadroomView {
            pool_id: pool.pool_id.clone(),
            pool_name: pool.pool_name.clone(),
            headroom_pct: worst,
            overcommitted,
        });
    }
    pool_views.sort_by(|a, b| a.pool_name.cmp(&b.pool_name));
    let dashboard = DashboardView {
        revenue_30d,
        revenue_30d_orders: orders,
        refunds_30d,
        active_licenses,
        licenses_by_status,
        uptime_pct_30d,
        uptime_checks_30d: checks,
        open_tasks,
        capacity_headroom: CapacityHeadroomView {
            pools_tracked: pool_views.len() as u64,
            lowest_headroom_pct: lowest,
            pools_overcommitted,
            pools: pool_views,
        },
        generated_at: now(),
    };
    json::success_response(dashboard, 200)
}


pub(crate) async fn list_sales(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<SaleRecord> = state
        .sales
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sale| {
            in_company_scope(&sale.company_id, &_scope)
                && matches_search(
                &format!("{} {}", sale.products.join(" "), sale.order_id.clone().unwrap_or_default()),
                &params,
            ) && params
                .filters
                .get("kind")
                .is_none_or(|kind| sale.kind == *kind)
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| sale.currency.eq_ignore_ascii_case(currency))
                && params.filters.get("product").is_none_or(|product| {
                    sale.products.iter().any(|name| name == product)
                })
        })
        .collect();
    items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn record_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    body: web::Json<RecordSaleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SaleRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        // Tenant scope: a body company must match the caller's
        // company (an unresolved caller may not claim one); an
        // omitted body company adopts the caller scope, so a sale
        // is never stored unscoped under a scoped caller.
        let body_company_id = input
            .company_id
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let company_id = require_company_match_for(&_scope, &body_company_id)?;
        if !matches!(input.kind.as_str(), "sale" | "refund") {
            return Err(UnifiedError::bad_request("kind must be sale or refund"));
        }
        // `total` arrives as integer minor units (the request
        // JSON carries major units, converted by `serde_major`);
        // it only needs the non-negative check here.
        if input.total < 0 {
            return Err(UnifiedError::bad_request("total must be zero or greater"));
        }
        let currency = input
            .currency
            .unwrap_or_else(|| "USD".to_string())
            .trim()
            .to_uppercase();
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request("currency must be a 3-letter code"));
        }
        let order_id = input
            .order_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(order_id) = &order_id {
            if state.sales.iter().any(|entry| {
                entry.value().order_id.as_deref() == Some(order_id.as_str())
                    && entry.value().kind == input.kind
                    && entry.value().company_id == company_id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A {} record for order \"{order_id}\" already exists",
                    input.kind
                )));
            }
        }
        let recorded_at = match input.recorded_at {
            Some(value) => parse_datetime(&value, "recorded_at")?,
            None => now(),
        };
        if recorded_at > now() {
            return Err(UnifiedError::bad_request("recorded_at must not be in the future"));
        }
        let record = SaleRecord {
            id: new_id(),
            order_id,
            kind: input.kind,
            total: input.total,
            currency,
            products: input.products.unwrap_or_default(),
            company_id,
            recorded_at,
            created_at: now(),
        };
        state.sales.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    respond_created(result, ctx.is_debug())
}


pub(crate) async fn get_sales_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let definition = ReportDefinition {
            id: String::new(),
            name: String::new(),
            description: None,
            metrics: vec!["revenue".to_string()],
            dimensions: vec!["day".to_string()],
            range_preset: None,
            start: Some(start),
            end: Some(end),
            filters: Value::Null,
            created_at: now(),
            updated_at: now(),
        };
        let rows = revenue_rows(&state, &definition, &_scope, start, end, false);
        Ok(json!({
            "from": start,
            "to": end,
            "rows": rows,
        }))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn get_license_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let mut by_status: HashMap<String, u64> = HashMap::new();
    let mut by_product: HashMap<String, ProductLicenseCount> = HashMap::new();
    for entry in state.licenses.iter() {
        let license = entry.value();
        *by_status.entry(license.status.clone()).or_insert(0) += 1;
        let group = by_product
            .entry(license.product.clone())
            .or_insert_with(|| ProductLicenseCount {
                product: license.product.clone(),
                total: 0,
                active: 0,
                suspended: 0,
                revoked: 0,
                expired: 0,
            });
        group.total += 1;
        match license.status.as_str() {
            "active" => group.active += 1,
            "suspended" => group.suspended += 1,
            "revoked" => group.revoked += 1,
            "expired" => group.expired += 1,
            _ => {}
        }
    }
    let mut products: Vec<ProductLicenseCount> = by_product.into_values().collect();
    products.sort_by(|a, b| a.product.cmp(&b.product));
    let total = state.licenses.len() as u64;
    json::success_response(
        LicenseSummaryView {
            total,
            by_status,
            by_product: products,
        },
        200,
    )
}


pub(crate) async fn get_revenue_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<RevenueSummaryView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let mut by_currency: HashMap<String, CurrencyRevenue> = HashMap::new();
        let mut orders = 0u64;
        let mut refunds = 0u64;
        for entry in state.sales.iter() {
            let sale = entry.value();
            if sale.recorded_at < start || sale.recorded_at > end {
                continue;
            }
            // Tenant scope: only the caller's own company's sales
            // feed this summary (an unresolved caller sees global
            // sales only).
            if !in_company_scope(&sale.company_id, &_scope) {
                continue;
            }
            let group = by_currency
                .entry(sale.currency.clone())
                .or_insert_with(|| CurrencyRevenue {
                    currency: sale.currency.clone(),
                    gross: 0,
                    refunds: 0,
                    net: 0,
                });
            if sale.kind == "sale" {
                group.gross += sale.total;
                orders += 1;
            } else if sale.kind == "refund" {
                group.refunds += sale.total;
                refunds += 1;
            }
        }
        let mut currencies: Vec<CurrencyRevenue> = by_currency
            .into_values()
            .map(|mut group| {
                // Integer minor units: the net is exact, no
                // rounding step is needed.
                group.net = group.gross - group.refunds;
                group
            })
            .collect();
        currencies.sort_by(|a, b| a.currency.cmp(&b.currency));
        Ok(RevenueSummaryView {
            from: start,
            to: end,
            orders,
            refunds,
            by_currency: currencies,
        })
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn list_metrics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<MetricSample> = state
        .metrics
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sample| {
            params
                .filters
                .get("kind")
                .is_none_or(|kind| sample.kind == *kind)
                && params.filters.get("source").is_none_or(|source| {
                    sample.source.as_deref() == Some(source.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn record_metric_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    body: web::Json<RecordMetricRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MetricSample> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        if !matches!(input.kind.as_str(), "api_call" | "download" | "login" | "upload") {
            return Err(UnifiedError::bad_request(
                "kind must be one of: api_call, download, login, upload",
            ));
        }
        let recorded_at = match input.recorded_at {
            Some(value) => parse_datetime(&value, "recorded_at")?,
            None => now(),
        };
        if recorded_at > now() {
            return Err(UnifiedError::bad_request("recorded_at must not be in the future"));
        }
        let sample = MetricSample {
            id: new_id(),
            kind: input.kind,
            source: input
                .source
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            recorded_at,
            created_at: now(),
        };
        state.metrics.insert(sample.id.clone(), sample.clone());
        Ok(sample)
    })();
    respond_created(result, ctx.is_debug())
}


pub(crate) async fn get_usage_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<UsageSummaryView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let mut by_kind: HashMap<String, u64> = HashMap::new();
        let mut by_day_map: HashMap<String, u64> = HashMap::new();
        let mut total = 0u64;
        for entry in state.metrics.iter() {
            let sample = entry.value();
            if sample.recorded_at < start || sample.recorded_at > end {
                continue;
            }
            if let Some(kind) = &query.kind {
                if sample.kind != *kind {
                    continue;
                }
            }
            *by_kind.entry(sample.kind.clone()).or_insert(0) += 1;
            *by_day_map.entry(day_key(sample.recorded_at)).or_insert(0) += 1;
            total += 1;
        }
        let mut by_day: Vec<DayCount> = by_day_map
            .into_iter()
            .map(|(day, count)| DayCount { day, count })
            .collect();
        by_day.sort_by(|a, b| a.day.cmp(&b.day));
        Ok(UsageSummaryView {
            from: start,
            to: end,
            total,
            by_kind,
            by_day,
        })
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn list_health(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<HealthSample> = state
        .health
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sample| {
            params
                .filters
                .get("status")
                .is_none_or(|status| sample.status == *status)
                && params.filters.get("node_id").is_none_or(|node_id| sample.node_id == *node_id)
                && params.filters.get("cluster_id").is_none_or(|cluster_id| {
                    sample.cluster_id.as_deref() == Some(cluster_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn record_health(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    body: web::Json<RecordHealthRequest>,
) -> HttpResponse {
    let result: UnifiedResult<HealthSample> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        let node_id = input.node_id.trim().to_string();
        if node_id.is_empty() {
            return Err(UnifiedError::bad_request("node_id must not be empty"));
        }
        if !matches!(input.status.as_str(), "healthy" | "degraded" | "unhealthy" | "unknown") {
            return Err(UnifiedError::bad_request(
                "status must be one of: healthy, degraded, unhealthy, unknown",
            ));
        }
        let checked_at = match input.checked_at {
            Some(value) => parse_datetime(&value, "checked_at")?,
            None => now(),
        };
        if checked_at > now() {
            return Err(UnifiedError::bad_request("checked_at must not be in the future"));
        }
        let sample = HealthSample {
            id: new_id(),
            node_id,
            cluster_id: input
                .cluster_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            status: input.status,
            latency_ms: input.latency_ms,
            checked_at,
            created_at: now(),
        };
        state.health.insert(sample.id.clone(), sample.clone());
        Ok(sample)
    })();
    respond_created(result, ctx.is_debug())
}


pub(crate) async fn get_uptime_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<UptimeSummaryView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let mut checks = 0u64;
        let mut healthy = 0u64;
        let mut degraded = 0u64;
        let mut unhealthy = 0u64;
        let mut unknown = 0u64;
        let mut by_day_map: HashMap<String, (u64, u64)> = HashMap::new();
        for entry in state.health.iter() {
            let sample = entry.value();
            if sample.checked_at < start || sample.checked_at > end {
                continue;
            }
            if let Some(node_id) = &query.node_id {
                if sample.node_id != *node_id {
                    continue;
                }
            }
            if let Some(cluster_id) = &query.cluster_id {
                if sample.cluster_id.as_deref() != Some(cluster_id.as_str()) {
                    continue;
                }
            }
            checks += 1;
            match sample.status.as_str() {
                "healthy" => healthy += 1,
                "degraded" => degraded += 1,
                "unhealthy" => unhealthy += 1,
                _ => unknown += 1,
            }
            let group = by_day_map
                .entry(day_key(sample.checked_at))
                .or_insert((0, 0));
            group.0 += 1;
            if sample.status == "healthy" {
                group.1 += 1;
            }
        }
        let uptime_pct = if checks > 0 {
            Some((healthy as f64 / checks as f64 * 10000.0).round() / 100.0)
        } else {
            None
        };
        let mut by_day: Vec<DayUptime> = by_day_map
            .into_iter()
            .map(|(day, (day_checks, day_healthy))| DayUptime {
                day,
                checks: day_checks,
                uptime_pct: if day_checks > 0 {
                    Some((day_healthy as f64 / day_checks as f64 * 10000.0).round() / 100.0)
                } else {
                    None
                },
            })
            .collect();
        by_day.sort_by(|a, b| a.day.cmp(&b.day));
        Ok(UptimeSummaryView {
            from: start,
            to: end,
            checks,
            healthy,
            degraded,
            unhealthy,
            unknown,
            uptime_pct,
            by_day,
        })
    })();
    json::respond(result, ctx.is_debug())
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        // Tenant gate (C3 pilot 3): handlers resolve the tenant
        // context first, so test requests carry one (no company).
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn ctx_data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn state_data() -> web::Data<Arc<AnalyticsState>> {
        web::Data::new(Arc::new(AnalyticsState::new()))
    }

    fn sale_body() -> web::Json<RecordSaleRequest> {
        web::Json(RecordSaleRequest {
            order_id: None,
            kind: "sale".to_string(),
            total: 10000,
            currency: Some("USD".to_string()),
            products: None,
            company_id: None,
            recorded_at: None,
        })
    }

    #[tokio::test]
    async fn get_dashboard_rejects_anonymous_callers() {
        let response = get_dashboard(
            ctx_data(test_context()),
            state_data(),
            TestRequest::default().to_http_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_dashboard_rejects_callers_without_analytics_view() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = get_dashboard(ctx_data(test_context()), state_data(), authed_request()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_dashboard_accepts_callers_with_analytics_view() {
        let response =
            get_dashboard(ctx_data(allowing_context()), state_data(), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn record_sale_rejects_anonymous_callers() {
        let response = record_sale(
            ctx_data(test_context()),
            state_data(),
            TestRequest::default().to_http_request(),
            sale_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn record_sale_rejects_callers_without_analytics_manage() {
        let response =
            record_sale(ctx_data(test_context()), state_data(), authed_request(), sale_body())
                .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn record_sale_accepts_callers_with_analytics_manage() {
        let response = record_sale(
            ctx_data(allowing_context()),
            state_data(),
            authed_request(),
            sale_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }
}
