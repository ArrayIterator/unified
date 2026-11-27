//! HTTP handlers for period reports: the period summary,
//! JSON and CSV export of a period, and the audit list.
//!
//! The summary is a pure computation over the stored rows
//! ([`build_period_summary`]): metrics by category,
//! completion, and target attainment. Values are never summed
//! across scopes - one summary row covers one (metric,
//! scope) combination.

use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;

use super::helpers::{
    caller, err, load_period, ok, paginate, render_csv, require_view, require_tenant, resolve_tenant,
};
use super::state::EsgState;
use super::types::{
    ACHIEVED_BPS, BASIS_POINTS, Category, CategorySummary, Decimal, MetricDataType, MetricProgress,
    Period, PeriodSummary, attainment_bps, attainment_label, completion_bps,
};

/// Build the summary of one reporting period. Pure over the
/// store: no writes, no events. Metrics that were deleted
/// since their entries were recorded are skipped - the
/// summary reports on live definitions only, while the
/// entries themselves stay in the export.
pub(crate) fn build_period_summary(
    state: &EsgState,
    tenant_id: &str,
    period: &Period,
) -> PeriodSummary {
    let metrics = state.metrics_of(tenant_id);
    let active_metrics: Vec<_> = metrics.iter().filter(|metric| metric.active).collect();
    let entries = state.entries_of_period(tenant_id, period.id);
    let targets = state.targets_of_period(tenant_id, period.id);

    // One row per (metric, scope) that has an entry or a
    // target, keyed by (metric_id, scope_type, scope_id).
    // Entries arrive in creation order, so inserting in
    // order leaves the latest value per scope in the map.
    let mut scopes: BTreeMap<(String, String, String), Option<String>> = BTreeMap::new();
    for entry in &entries {
        let key = (
            entry.metric_id.to_string(),
            entry.scope_type.clone(),
            entry.scope_id.clone(),
        );
        scopes.insert(key, Some(entry.value_text.clone()));
    }
    for target in &targets {
        let key = (
            target.metric_id.to_string(),
            target.scope_type.clone(),
            target.scope_id.clone(),
        );
        scopes.entry(key).or_insert(None);
    }

    let mut rows: Vec<MetricProgress> = Vec::new();
    let mut attainment_values: Vec<i64> = Vec::new();
    let mut reported_metric_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut targets_set = 0i64;
    let mut targets_achieved = 0i64;

    for ((metric_id, scope_type, scope_id), actual) in &scopes {
        let Some(metric) = active_metrics
            .iter()
            .find(|metric| metric.id.to_string() == *metric_id)
        else {
            continue;
        };
        if actual.is_some() {
            reported_metric_ids.insert(metric_id.clone());
        }
        let target = targets.iter().find(|target| {
            target.metric_id.to_string() == *metric_id
                && target.scope_type == *scope_type
                && target.scope_id == *scope_id
        });
        if target.is_some() {
            targets_set += 1;
        }
        let mut attainment: Option<i64> = None;
        if let (Some(actual_value), Some(target)) = (actual, target) {
            attainment = match metric.data_type {
                MetricDataType::Number => {
                    let actual_dec = Decimal::parse(actual_value);
                    let target_dec = Decimal::parse(&target.target_value);
                    match (actual_dec, target_dec) {
                        (Some(actual_dec), Some(target_dec)) => {
                            attainment_bps(&actual_dec, &target_dec, target.direction)
                        }
                        _ => None,
                    }
                }
                MetricDataType::Boolean => {
                    if *actual_value == target.target_value {
                        Some(BASIS_POINTS)
                    } else {
                        Some(0)
                    }
                }
                MetricDataType::Text => None,
            };
        }
        if let Some(bps) = attainment {
            attainment_values.push(bps.min(BASIS_POINTS));
            if bps >= ACHIEVED_BPS {
                targets_achieved += 1;
            }
        }
        rows.push(MetricProgress {
            metric_id: metric_id.clone(),
            metric_name: metric.name.clone(),
            category: metric.category,
            unit: metric.unit.clone(),
            data_type: metric.data_type,
            scope_type: scope_type.clone(),
            scope_id: scope_id.clone(),
            actual_value: actual.clone(),
            target_value: target.map(|target| target.target_value.clone()),
            attainment_bps: attainment,
            attainment_label: attainment
                .map(attainment_label)
                .unwrap_or("not_started")
                .to_string(),
        });
    }
    rows.sort_by(|a, b| {
        a.category
            .as_str()
            .cmp(b.category.as_str())
            .then(a.metric_name.cmp(&b.metric_name))
            .then(a.scope_id.cmp(&b.scope_id))
    });

    let metric_count = active_metrics.len() as i64;
    let reported_count = reported_metric_ids.len() as i64;
    let average_attainment_bps = if attainment_values.is_empty() {
        None
    } else {
        let sum: i64 = attainment_values.iter().sum();
        Some(sum / attainment_values.len() as i64)
    };

    let mut by_category: Vec<CategorySummary> = Vec::new();
    for category in [
        Category::Environmental,
        Category::Social,
        Category::Governance,
    ] {
        let category_metrics: Vec<_> = active_metrics
            .iter()
            .filter(|metric| metric.category == category)
            .collect();
        if category_metrics.is_empty() {
            continue;
        }
        let category_metric_ids: std::collections::HashSet<String> = category_metrics
            .iter()
            .map(|metric| metric.id.to_string())
            .collect();
        let reported = reported_metric_ids
            .iter()
            .filter(|id| category_metric_ids.contains(*id))
            .count() as i64;
        let category_rows: Vec<&MetricProgress> =
            rows.iter().filter(|row| row.category == category).collect();
        let category_targets_set = category_rows
            .iter()
            .filter(|row| row.target_value.is_some())
            .count() as i64;
        let category_achieved = category_rows
            .iter()
            .filter(|row| {
                row.attainment_bps
                    .map(|bps| bps >= ACHIEVED_BPS)
                    .unwrap_or(false)
            })
            .count() as i64;
        by_category.push(CategorySummary {
            category,
            metric_count: category_metrics.len() as i64,
            reported_count: reported,
            completion_bps: completion_bps(reported, category_metrics.len() as i64),
            targets_set: category_targets_set,
            targets_achieved: category_achieved,
        });
    }

    PeriodSummary {
        period_id: period.id.to_string(),
        period_code: period.code.clone(),
        status: period.status,
        metric_count,
        reported_count,
        completion_bps: completion_bps(reported_count, metric_count),
        targets_set,
        targets_achieved,
        average_attainment_bps,
        by_category,
        metrics: rows,
    }
}

/// Get the summary of one reporting period: metrics by
/// category, completion, and target attainment.
pub(crate) async fn get_period_summary(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let period = load_period(&state, &tenant_id, &path.into_inner())?;
        let summary = build_period_summary(&state, &tenant_id, &period);
        Ok(serde_json::to_value(&summary).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Export one reporting period as JSON: the period, its
/// summary, and every entry and target of the period
/// (including rows whose metric was deleted since).
pub(crate) async fn export_period_json(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let period = load_period(&state, &tenant_id, &path.into_inner())?;
        let summary = build_period_summary(&state, &tenant_id, &period);
        let entries = state.entries_of_period(&tenant_id, period.id);
        let targets = state.targets_of_period(&tenant_id, period.id);
        Ok(json!({
            "period": period,
            "summary": summary,
            "entries": entries,
            "targets": targets,
            "export_format": "esg-period-json-v1",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Export one reporting period as CSV: one row per (metric,
/// scope) summary row, with the recorded value, the target,
/// and the attainment.
pub(crate) async fn export_period_csv(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(String, String)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let period = load_period(&state, &tenant_id, &path.into_inner())?;
        let summary = build_period_summary(&state, &tenant_id, &period);
        let mut rows: Vec<Vec<String>> = vec![vec![
            "period_code".to_string(),
            "metric_name".to_string(),
            "category".to_string(),
            "unit".to_string(),
            "data_type".to_string(),
            "scope_type".to_string(),
            "scope_id".to_string(),
            "actual_value".to_string(),
            "target_value".to_string(),
            "attainment_bps".to_string(),
            "attainment_label".to_string(),
        ]];
        for row in &summary.metrics {
            rows.push(vec![
                period.code.clone(),
                row.metric_name.clone(),
                row.category.as_str().to_string(),
                row.unit.clone(),
                row.data_type.as_str().to_string(),
                row.scope_type.clone(),
                row.scope_id.clone(),
                row.actual_value.clone().unwrap_or_default(),
                row.target_value.clone().unwrap_or_default(),
                row.attainment_bps
                    .map(|bps| bps.to_string())
                    .unwrap_or_default(),
                row.attainment_label.clone(),
            ]);
        }
        let filename = format!("esg-{}.csv", period.code);
        Ok((render_csv(&rows), filename))
    })();
    match result {
        Ok((csv, filename)) => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            ))
            .body(csv),
        Err(error) => err(error, debug),
    }
}

/// List the audit trail (paginated, filter by entity type,
/// entity id, action, actor; newest first). Audit rows are
/// never updated or deleted through the API.
pub(crate) async fn list_audit(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let entity_type_filter = params.filters.get("entity_type").cloned();
        let entity_filter = params.filters.get("entity_id").cloned();
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor").cloned();
        let mut items = state.audit_of(&tenant_id);
        items.retain(|row| {
            if let Some(entity_type) = &entity_type_filter {
                if row.entity_type != *entity_type {
                    return false;
                }
            }
            if let Some(entity_id) = &entity_filter {
                if row.entity_id != *entity_id {
                    return false;
                }
            }
            if let Some(action) = &action_filter {
                if row.action != *action {
                    return false;
                }
            }
            if let Some(actor) = &actor_filter {
                if row.actor != *actor {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};
    use uuid::Uuid;

    fn fixture() -> (EsgState, Period, Uuid, Uuid) {
        let state = EsgState::default();
        let tenant_id = "tenant-a";
        let now = Utc::now();
        let emissions = super::super::types::Metric {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            name: "Emissions".to_string(),
            description: None,
            category: Category::Environmental,
            unit: "tCO2e".to_string(),
            data_type: MetricDataType::Number,
            frequency: "quarterly".to_string(),
            framework_ids: vec![],
            active: true,
            created_by: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        };
        let training = super::super::types::Metric {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            name: "Training hours".to_string(),
            description: None,
            category: Category::Social,
            unit: "hours".to_string(),
            data_type: MetricDataType::Number,
            frequency: "quarterly".to_string(),
            framework_ids: vec![],
            active: true,
            created_by: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        };
        let period = Period {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            code: "2026-Q1".to_string(),
            name: None,
            starts_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            ends_on: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            status: super::super::types::PeriodStatus::Open,
            created_by: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.metrics.insert(emissions.id, emissions.clone());
        state.metrics.insert(training.id, training.clone());
        state.periods.insert(period.id, period.clone());
        // Emissions: 80 recorded against a 100 target, lower
        // is better -> 125% attainment, achieved.
        state.entries.insert(
            Uuid::new_v4(),
            super::super::types::Entry {
                id: Uuid::new_v4(),
                tenant_id: tenant_id.to_string(),
                metric_id: emissions.id,
                period_id: period.id,
                scope_type: "site".to_string(),
                scope_id: "site-1".to_string(),
                value_text: "80".to_string(),
                source_note: None,
                document_ids: vec![],
                entered_by: "user-1".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        state.targets.insert(
            Uuid::new_v4(),
            super::super::types::Target {
                id: Uuid::new_v4(),
                tenant_id: tenant_id.to_string(),
                metric_id: emissions.id,
                period_id: period.id,
                scope_type: "site".to_string(),
                scope_id: "site-1".to_string(),
                target_value: "100".to_string(),
                direction: super::super::types::TargetDirection::Decrease,
                deadline: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
                created_by: "user-1".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        (state, period, emissions.id, training.id)
    }

    #[test]
    fn period_summary_counts_completion_and_attainment() {
        let (state, period, emissions_id, training_id) = fixture();
        let summary = build_period_summary(&state, "tenant-a", &period);
        assert_eq!(summary.period_code, "2026-Q1");
        assert_eq!(summary.metric_count, 2);
        assert_eq!(summary.reported_count, 1);
        assert_eq!(summary.completion_bps, 5000);
        assert_eq!(summary.targets_set, 1);
        assert_eq!(summary.targets_achieved, 1);
        assert_eq!(summary.average_attainment_bps, Some(10_000));
        assert_eq!(summary.metrics.len(), 1);
        let row = &summary.metrics[0];
        assert_eq!(row.metric_id, emissions_id.to_string());
        assert_eq!(row.actual_value.as_deref(), Some("80"));
        assert_eq!(row.target_value.as_deref(), Some("100"));
        assert_eq!(row.attainment_bps, Some(12_500));
        assert_eq!(row.attainment_label, "achieved");
        assert_eq!(summary.by_category.len(), 2);
        let environmental = summary
            .by_category
            .iter()
            .find(|category| category.category == Category::Environmental)
            .unwrap();
        assert_eq!(environmental.metric_count, 1);
        assert_eq!(environmental.reported_count, 1);
        assert_eq!(environmental.completion_bps, 10_000);
        let social = summary
            .by_category
            .iter()
            .find(|category| category.category == Category::Social)
            .unwrap();
        assert_eq!(social.metric_count, 1);
        assert_eq!(social.reported_count, 0);
        assert_eq!(social.completion_bps, 0);
        let _ = training_id;
    }

    #[test]
    fn period_summary_with_no_metrics_is_complete() {
        let state = EsgState::default();
        let period = Period {
            id: Uuid::new_v4(),
            tenant_id: "tenant-a".to_string(),
            code: "2026-Q1".to_string(),
            name: None,
            starts_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            ends_on: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            status: super::super::types::PeriodStatus::Open,
            created_by: "user-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let summary = build_period_summary(&state, "tenant-a", &period);
        assert_eq!(summary.metric_count, 0);
        assert_eq!(summary.completion_bps, 10_000);
        assert_eq!(summary.average_attainment_bps, None);
        assert!(summary.by_category.is_empty());
        assert!(summary.metrics.is_empty());
    }
}
