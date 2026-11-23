//! Planner - Capacity forecasts and overcommit reports.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, helpers::*, types::*, state::*, handlers_pools::*, handlers_plans::*, events::*, module::*};

// ---------------------------------------------------------------------------
// Forecast.
// ---------------------------------------------------------------------------

/// Least-squares linear trend over (days, value) points. Returns
/// (slope per day, intercept, R-squared), or `None` when fewer than two
/// points were given or all x values are identical.
pub(crate) fn linear_trend(points: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let mean_x = points.iter().map(|point| point.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|point| point.1).sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut ss_tot = 0.0;
    for (x, y) in points {
        sxx += (x - mean_x) * (x - mean_x);
        sxy += (x - mean_x) * (y - mean_y);
        ss_tot += (y - mean_y) * (y - mean_y);
    }
    if sxx <= f64::EPSILON {
        return None;
    }
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    let mut ss_res = 0.0;
    for (x, y) in points {
        let predicted = intercept + slope * x;
        ss_res += (y - predicted) * (y - predicted);
    }
    let r_squared = if ss_tot <= f64::EPSILON {
        1.0
    } else {
        (1.0 - ss_res / ss_tot).clamp(0.0, 1.0)
    };
    Some((slope, intercept, r_squared))
}

pub(crate) fn confidence_for(samples: usize, r_squared: f64) -> &'static str {
    if samples >= 14 && r_squared >= 0.7 {
        "high"
    } else if samples >= 7 && r_squared >= 0.4 {
        "medium"
    } else {
        "low"
    }
}

/// Project one resource forward `horizon_days` days from the last
/// sample, using the linear trend fitted to the samples.
///
/// `exhaustion_date` is the date the trend reaches capacity: a future
/// date while usage is still below capacity, and the last sample date
/// when usage has already reached or passed capacity while still
/// rising (`current >= capacity` with a positive slope) — exhaustion
/// has already happened by then, so there is no future date to
/// report. It stays `None` when the trend is flat or falling, or when
/// the projected crossing is unreasonably far out (> 10 years).
pub(crate) fn project_resource(
    resource: &'static str,
    samples: &[UsageSample],
    extract: fn(&Resources) -> f64,
    capacity: f64,
    horizon_days: f64,
    last_at: DateTime<Utc>,
) -> ResourceProjection {
    let first_at = samples.first().map(|sample| sample.recorded_at);
    let points: Vec<(f64, f64)> = samples
        .iter()
        .map(|sample| {
            let days = sample
                .recorded_at
                .signed_duration_since(first_at.unwrap_or(sample.recorded_at))
                .num_seconds() as f64
                / 86_400.0;
            (days, extract(&sample.used))
        })
        .collect();
    let current = samples.last().map(|sample| extract(&sample.used)).unwrap_or(0.0);
    let last_days = points.last().map(|point| point.0).unwrap_or(0.0);
    let (projected, confidence, exhaustion_date) = match linear_trend(&points) {
        Some((slope, intercept, r_squared)) => {
            let projected = (intercept + slope * (last_days + horizon_days)).max(0.0);
            let exhaustion = if slope > 1e-9 {
                if current >= capacity {
                    // Already exhausted while still rising: the last
                    // recorded sample is the exhaustion date, since
                    // usage reached capacity on or before it.
                    Some(last_at.to_rfc3339())
                } else {
                    let days_to_exhaust = (capacity - current) / slope;
                    if days_to_exhaust >= 0.0 && days_to_exhaust <= 3650.0 {
                        let date = last_at + chrono::Duration::seconds((days_to_exhaust * 86_400.0) as i64);
                        Some(date.to_rfc3339())
                    } else {
                        None
                    }
                }
            } else {
                None
            };
            (
                projected,
                confidence_for(samples.len(), r_squared),
                exhaustion,
            )
        }
        None => (current, "low", None),
    };
    let utilisation = if capacity > 0.0 {
        Some((projected / capacity * 100.0 * 100.0).round() / 100.0)
    } else {
        None
    };
    ResourceProjection {
        resource,
        current: (current * 100.0).round() / 100.0,
        projected: (projected * 100.0).round() / 100.0,
        capacity,
        projected_utilisation_pct: utilisation,
        exhaustion_date,
        confidence,
    }
}

/// Linear-trend capacity forecast for a pool. Requires
/// `planner.view`. Accepted `horizon_days`: 7, 30, 60, 90, 180
/// (defaults to 30).
pub(crate) async fn get_forecast(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ForecastQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForecastView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)?;
        let pool = state.pool(path.as_str())?;
        let horizon_days = query.into_inner().horizon_days.unwrap_or(30);
        if !matches!(horizon_days, 7 | 30 | 60 | 90 | 180) {
            return Err(UnifiedError::bad_request(
                "horizon_days must be one of: 7, 30, 60, 90, 180",
            ));
        }
        let samples = state.samples_for_pool(&pool.id);
        if samples.len() < 2 {
            return Err(UnifiedError::unprocessable(format!(
                "Forecast needs at least 2 usage samples for pool \"{}\"; {} recorded so far",
                pool.id,
                samples.len()
            ))
            .with_metadata("samples_recorded", Value::from(samples.len())));
        }
        let last_at = samples.last().map(|sample| sample.recorded_at).unwrap_or_else(now);
        let horizon = horizon_days as f64;
        let projections = vec![
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, pool.capacity.cpu_cores, horizon, last_at),
            project_resource("ram_gb", &samples, |used| used.ram_gb, pool.capacity.ram_gb, horizon, last_at),
            project_resource("disk_gb", &samples, |used| used.disk_gb, pool.capacity.disk_gb, horizon, last_at),
            project_resource("bandwidth_tb", &samples, |used| used.bandwidth_tb, pool.capacity.bandwidth_tb, horizon, last_at),
        ];
        let confidence = projections
            .iter()
            .map(|projection| projection.confidence)
            .min_by_key(|confidence| match *confidence {
                "low" => 0,
                "medium" => 1,
                _ => 2,
            })
            .unwrap_or("low");
        let forecast = ForecastView {
            pool_id: pool.id.clone(),
            pool_name: pool.name.clone(),
            horizon_days,
            generated_at: now(),
            samples_used: samples.len(),
            first_sample_at: samples.first().map(|sample| sample.recorded_at),
            last_sample_at: Some(last_at),
            projections,
            confidence,
            disclaimer: format!(
                "Projection only, not a guarantee: a linear trend fitted to {} recorded usage sample(s) and projected {} day(s) forward. Real usage depends on future demand, scaling events and provider changes; re-check after new samples arrive.",
                samples.len(),
                horizon_days
            ),
        };
        ctx.hooks.emit(
            PLANNER_FORECAST_GENERATED,
            serde_json::to_value(&forecast).unwrap_or(Value::Null),
        );
        Ok(forecast)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Overcommit report.
// ---------------------------------------------------------------------------

pub(crate) fn overcommit_entry(state: &PlannerState, pool: &CapacityPool) -> OvercommitReportEntry {
    let allocated = state.allocated(&pool.id, now());
    let excess = allocated.excess_over(&pool.capacity);
    let mut resources: Vec<&'static str> = Vec::new();
    if allocated.cpu_cores > pool.capacity.cpu_cores {
        resources.push("cpu_cores");
    }
    if allocated.ram_gb > pool.capacity.ram_gb {
        resources.push("ram_gb");
    }
    if allocated.disk_gb > pool.capacity.disk_gb {
        resources.push("disk_gb");
    }
    if allocated.bandwidth_tb > pool.capacity.bandwidth_tb {
        resources.push("bandwidth_tb");
    }
    OvercommitReportEntry {
        headroom: pool.capacity.subtract(&allocated),
        overcommitted: allocated.overcommitted_against(&pool.capacity),
        live_plans: state.live_plan_count(&pool.id, now()),
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        capacity: pool.capacity,
        allocated,
        excess,
        overcommitted_resources: resources,
    }
}

/// Overcommit report for one pool. Requires `planner.view`.
pub(crate) async fn get_pool_overcommit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<OvercommitReportEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)?;
        let pool = state.pool(path.as_str())?;
        Ok(overcommit_entry(&state, &pool))
    })();
    json::respond(result, ctx.is_debug())
}

/// Overcommit report for every pool. Requires `planner.view`.
pub(crate) async fn list_overcommit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let guard: UnifiedResult<()> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)
    })();
    if let Err(error) = guard {
        return json::error_response(&error, ctx.is_debug());
    }
    let mut items: Vec<OvercommitReportEntry> = state
        .pools
        .iter()
        .map(|entry| overcommit_entry(&state, entry.value()))
        .filter(|entry| {
            matches_search(&entry.pool_name, &params)
                && params.filters.get("overcommitted").is_none_or(|flag| {
                    entry.overcommitted == (flag == "true" || flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.pool_name.cmp(&b.pool_name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_trend_perfect_fit() {
        let (slope, intercept, r_squared) =
            linear_trend(&[(0.0, 0.0), (1.0, 2.0), (2.0, 4.0)]).expect("fit");
        assert!((slope - 2.0).abs() < 1e-9);
        assert!(intercept.abs() < 1e-9);
        assert!((r_squared - 1.0).abs() < 1e-9);
    }

    #[test]
    fn linear_trend_rejects_degenerate_input() {
        assert!(linear_trend(&[]).is_none());
        assert!(linear_trend(&[(0.0, 1.0)]).is_none());
        assert!(linear_trend(&[(1.0, 0.0), (1.0, 5.0)]).is_none());
        // Constant y: zero slope, perfect fit by definition.
        let (slope, intercept, r_squared) =
            linear_trend(&[(0.0, 5.0), (1.0, 5.0), (2.0, 5.0)]).expect("fit");
        assert!(slope.abs() < 1e-9);
        assert!((intercept - 5.0).abs() < 1e-9);
        assert!((r_squared - 1.0).abs() < 1e-9);
    }

    #[test]
    fn confidence_for_cases() {
        assert_eq!(confidence_for(14, 0.7), "high");
        assert_eq!(confidence_for(100, 1.0), "high");
        assert_eq!(confidence_for(14, 0.69), "medium");
        assert_eq!(confidence_for(13, 0.9), "medium");
        assert_eq!(confidence_for(7, 0.4), "medium");
        assert_eq!(confidence_for(7, 0.39), "low");
        assert_eq!(confidence_for(6, 0.9), "low");
        assert_eq!(confidence_for(0, 0.0), "low");
    }

    fn usage_sample(cpu: f64, recorded_at: DateTime<Utc>) -> UsageSample {
        UsageSample {
            id: new_id(),
            pool_id: "pool-1".to_string(),
            used: Resources {
                cpu_cores: cpu,
                ram_gb: 0.0,
                disk_gb: 0.0,
                bandwidth_tb: 0.0,
            },
            source: "api".to_string(),
            recorded_at,
            created_at: recorded_at,
        }
    }

    /// Rising usage that already reached capacity reports the last
    /// sample date as the exhaustion date (defect: it used to
    /// report no exhaustion date at all).
    #[test]
    fn project_resource_reports_exhaustion_when_already_at_capacity() {
        let base = Utc::now() - chrono::Duration::days(10);
        let samples = vec![
            usage_sample(8.0, base),
            usage_sample(9.0, base + chrono::Duration::days(5)),
            usage_sample(10.0, base + chrono::Duration::days(10)),
        ];
        let last_at = samples.last().unwrap().recorded_at;
        let projection =
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, 10.0, 30.0, last_at);
        assert_eq!(projection.exhaustion_date.as_deref(), Some(last_at.to_rfc3339().as_str()));
    }

    /// Rising usage already past capacity reports the last sample
    /// date too, not a future date.
    #[test]
    fn project_resource_reports_exhaustion_when_already_over_capacity() {
        let base = Utc::now() - chrono::Duration::days(10);
        let samples = vec![
            usage_sample(8.0, base),
            usage_sample(10.0, base + chrono::Duration::days(5)),
            usage_sample(12.0, base + chrono::Duration::days(10)),
        ];
        let last_at = samples.last().unwrap().recorded_at;
        let projection =
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, 10.0, 30.0, last_at);
        assert_eq!(projection.exhaustion_date.as_deref(), Some(last_at.to_rfc3339().as_str()));
    }

    /// Rising usage still below capacity keeps a future exhaustion
    /// date (2 units short at +1/day reaches capacity in 2 days).
    #[test]
    fn project_resource_future_exhaustion_below_capacity() {
        let base = Utc::now() - chrono::Duration::days(2);
        let samples = vec![
            usage_sample(6.0, base),
            usage_sample(7.0, base + chrono::Duration::days(1)),
            usage_sample(8.0, base + chrono::Duration::days(2)),
        ];
        let last_at = samples.last().unwrap().recorded_at;
        let projection =
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, 10.0, 30.0, last_at);
        let expected = last_at + chrono::Duration::days(2);
        assert_eq!(projection.exhaustion_date.as_deref(), Some(expected.to_rfc3339().as_str()));
    }

    /// Flat usage at capacity with no rising trend reports no
    /// exhaustion date: with slope zero there is no trend to
    /// project, and the already-exhausted case is defined for a
    /// positive slope only.
    #[test]
    fn project_resource_no_exhaustion_for_flat_trend() {
        let base = Utc::now() - chrono::Duration::days(2);
        let samples = vec![
            usage_sample(10.0, base),
            usage_sample(10.0, base + chrono::Duration::days(1)),
            usage_sample(10.0, base + chrono::Duration::days(2)),
        ];
        let last_at = samples.last().unwrap().recorded_at;
        let projection =
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, 10.0, 30.0, last_at);
        assert!(projection.exhaustion_date.is_none());
    }

    /// Falling usage below capacity reports no exhaustion date.
    #[test]
    fn project_resource_no_exhaustion_for_falling_trend() {
        let base = Utc::now() - chrono::Duration::days(2);
        let samples = vec![
            usage_sample(8.0, base),
            usage_sample(7.0, base + chrono::Duration::days(1)),
            usage_sample(6.0, base + chrono::Duration::days(2)),
        ];
        let last_at = samples.last().unwrap().recorded_at;
        let projection =
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, 10.0, 30.0, last_at);
        assert!(projection.exhaustion_date.is_none());
    }
}
