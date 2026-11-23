// bi module -- Report execution, aggregation, forecasting, and KPI computation.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

use super::*;

/// Record one mirrored source row. Deduplication is by
/// (source, kind, ref_id): a repeated delivery of the same source
/// event updates the row instead of duplicating it.
pub(crate) fn record_source_row(
    state: &BiState,
    source: &str,
    kind: &str,
    company_id: Option<String>,
    customer_id: Option<String>,
    amount: Option<f64>,
    currency: Option<String>,
    occurred_at: DateTime<Utc>,
    ref_id: Option<String>,
) {
    if let Some(ref_id) = &ref_id {
        let mut existing: Option<Uuid> = None;
        for entry in state.source_records.iter() {
            let row = entry.value();
            if row.source == source && row.kind == kind && row.ref_id.as_deref() == Some(ref_id.as_str()) {
                existing = Some(row.id);
                break;
            }
        }
        if let Some(id) = existing {
            if let Some(mut stored) = state.source_records.get_mut(&id) {
                stored.company_id = company_id;
                stored.customer_id = customer_id;
                stored.amount = amount.map(round_money);
                stored.currency = currency;
                stored.occurred_at = occurred_at;
            }
            return;
        }
    }
    let now = Utc::now();
    let row = SourceRecord {
        id: Uuid::new_v4(),
        source: source.to_string(),
        company_id,
        customer_id,
        kind: kind.to_string(),
        amount: amount.map(round_money),
        currency,
        occurred_at,
        ref_id,
        created_at: now,
    };
    state.source_records.insert(row.id, row);
}

/// Mirror one source event payload into a row. Amount keys cover
/// the billing / store shapes (`amount`, `total_amount`,
/// `paid_amount`, `total`); customer keys cover `customer_id` and
/// `user_id`; time keys cover `paid_at`, `occurred_at`, and
/// `created_at`. A payload with no usable fields is ignored.

pub(crate) fn mirror_event(state: &BiState, source: &str, kind: &str, data: &Value) {
    let company_id = payload_string(data, &["company_id"]);
    let customer_id = payload_string(data, &["customer_id", "user_id"]);
    let amount = payload_number(data, &["amount", "total_amount", "paid_amount", "total"]);
    let currency = payload_string(data, &["currency"])
        .and_then(|value| valid_currency(&value).ok());
    let occurred_at = payload_time(data, &["paid_at", "occurred_at", "created_at"])
        .unwrap_or_else(Utc::now);
    // `license_id` is the license events' identity key (license
    // payloads carry no plain `id`); without it every license row
    // mirrors with no ref_id and license lifecycles cannot be
    // correlated by ref_id.
    let ref_id = payload_string(
        data,
        &["id", "order_id", "invoice_id", "ref_id", "entity_id", "license_id"],
    );
    record_source_row(
        state,
        source,
        kind,
        company_id,
        customer_id,
        amount,
        currency,
        occurred_at,
        ref_id,
    );
}

/// Read rows for one source: the local mirror first, then any rows
/// a source module contributes through its `{source}.data.requested`
/// read hook. Hook rows are objects with optional `amount`,
/// `currency`, `occurred_at`, `customer_id`, `kind`, and `ref_id`
/// keys; malformed rows are skipped, never trusted blindly.
///
/// Company filter (C3 pilot 3): `Some(company)` reads only that
/// company's rows; `None` reads global rows only (no company or an
/// empty company) -- never every company's rows. The old `None`
/// arm meant "no filter", which let an unresolved caller forecast
/// over all companies' data; both production call sites pass
/// `Some(...)`, so this only closes the forecast path.
pub(crate) fn read_source_rows(
    state: &BiState,
    source: &str,
    company_id: Option<&str>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<Value> {
    let mut rows: Vec<Value> = state
        .source_records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.source == source)
        .filter(|row| match company_id {
            Some(company) => row.company_id.as_deref() == Some(company),
            None => row.company_id.as_deref().is_none_or(|value| value.is_empty()),
        })
        .filter(|row| match from {
            Some(from) => row.occurred_at >= from,
            None => true,
        })
        .filter(|row| match to {
            Some(to) => row.occurred_at <= to,
            None => true,
        })
        .filter_map(|row| serde_json::to_value(row).ok())
        .collect();
    if let Some(bus) = state.bus() {
        let context = bus.emit(
            &format!("{source}.data.requested"),
            json!({
                "source": source,
                "company_id": company_id,
                "from": from.map(|value| value.to_rfc3339()),
                "to": to.map(|value| value.to_rfc3339()),
                "limit": MAX_EXPORT_ROWS,
            }),
        );
        for result in &context.results {
            if let Some(hook_rows) = result.get("rows").and_then(Value::as_array) {
                for hook_row in hook_rows {
                    if !hook_row.is_object() {
                        continue;
                    }
                    let mut row = hook_row.clone();
                    if let Value::Object(map) = &mut row {
                        map.insert("source".to_string(), Value::String(source.to_string()));
                        if !map.contains_key("kind") {
                            map.insert(
                                "kind".to_string(),
                                Value::String("record".to_string()),
                            );
                        }
                    }
                    rows.push(row);
                }
            }
        }
    }
    rows
}

/// Apply a report's exact-match filters plus the optional
/// `from` / `to` bounds to one row. Unknown filter keys never
/// match a row, so a typo fails closed instead of returning
/// unfiltered data. Date bounds fail closed too: an unparseable
/// bound, an unparseable `occurred_at`, or a row with no
/// `occurred_at` at all is a non-match whenever a bound is set
/// (bounds are validated as RFC 3339 at write time, so an
/// unparseable bound here means stored or hook-supplied data that
/// bypassed validation -- it must not silently disable filtering).

pub(crate) fn row_matches_filters(row: &Value, filters: &Value) -> bool {
    let Some(filter_map) = filters.as_object() else {
        return true;
    };
    for (key, expected) in filter_map {
        if key == "from" || key == "to" {
            continue;
        }
        match row.get(key) {
            Some(actual) => {
                if actual != expected {
                    return false;
                }
            }
            None => return false,
        }
    }
    for (bound, is_from) in [("from", true), ("to", false)] {
        let Some(raw) = filter_map.get(bound) else {
            continue;
        };
        let Some(bound_text) = raw.as_str() else {
            return false;
        };
        let Ok(bound_dt) = DateTime::parse_from_rfc3339(bound_text) else {
            return false;
        };
        let Some(occurred_raw) = row.get("occurred_at").and_then(Value::as_str) else {
            return false;
        };
        let Ok(occurred) = DateTime::parse_from_rfc3339(occurred_raw) else {
            return false;
        };
        if is_from && occurred < bound_dt {
            return false;
        }
        if !is_from && occurred > bound_dt {
            return false;
        }
    }
    true
}

/// Project one row down to the report's selected fields. When no
/// fields are selected, the whole row is returned.
pub(crate) fn project_row(row: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return row.clone();
    }
    let mut out = serde_json::Map::new();
    for field in fields {
        if let Some(value) = row.get(field) {
            out.insert(field.clone(), value.clone());
        }
    }
    Value::Object(out)
}

/// Aggregate rows: when `group_by` is set, one bucket per distinct
/// group value with `count`, `total_amount`, and `average_amount`;
/// otherwise a single totals row. Rows are sorted by group value
/// for deterministic output.
pub(crate) fn aggregate_rows(rows: &[Value], group_by: Option<&str>) -> (Vec<Value>, Value) {
    let mut total_amount = 0.0f64;
    let mut amount_count = 0usize;
    for row in rows {
        if let Some(amount) = row.get("amount").and_then(Value::as_f64) {
            total_amount += amount;
            amount_count += 1;
        }
    }
    total_amount = round_money(total_amount);
    let average_amount = if amount_count > 0 {
        Some(round_money(total_amount / amount_count as f64))
    } else {
        None
    };
    let totals = json!({
        "row_count": rows.len(),
        "total_amount": total_amount,
        "amount_rows": amount_count,
        "average_amount": average_amount,
    });
    let Some(group_by) = group_by else {
        return (rows.to_vec(), totals);
    };
    let mut buckets: BTreeMap<String, (usize, f64, usize)> = BTreeMap::new();
    for row in rows {
        let key = row
            .get(group_by)
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| "null".to_string());
        let entry = buckets.entry(key).or_insert((0, 0.0, 0));
        entry.0 += 1;
        if let Some(amount) = row.get("amount").and_then(Value::as_f64) {
            entry.1 += amount;
            entry.2 += 1;
        }
    }
    let grouped: Vec<Value> = buckets
        .into_iter()
        .map(|(group, (count, sum, amount_rows))| {
            json!({
                "group": group,
                "count": count,
                "total_amount": round_money(sum),
                "amount_rows": amount_rows,
                "average_amount": if amount_rows > 0 { Some(round_money(sum / amount_rows as f64)) } else { None },
            })
        })
        .collect();
    (grouped, totals)
}

/// Execute a saved report against the source data: read rows,
/// apply filters, project fields, aggregate. Returns an
/// "insufficient data" error when the source has no rows at all --
/// never an empty success dressed up as a measurement.
///
/// `max_rows` caps the returned rows: `MAX_RUN_ROWS` for an inline
/// run, `MAX_EXPORT_ROWS` for an export. The cap must be chosen by
/// the caller -- truncating every execution to the run cap would
/// silently cut exports short of the documented 10,000-row export
/// cap. Totals are computed over all filtered rows before the cap.
pub(crate) fn execute_report(
    state: &BiState,
    report: &BiReport,
    max_rows: usize,
) -> UnifiedResult<Value> {
    let rows = read_source_rows(state, &report.source, Some(&report.company_id), None, None);
    if rows.is_empty() {
        return Err(UnifiedError::unprocessable(format!(
            "Insufficient data: source \"{}\" has no rows for company \"{}\" yet (no source events received and no module answered \"{}.data.requested\")",
            report.source, report.company_id, report.source
        ))
        .with_metadata("source", json!(report.source))
        .with_metadata("company_id", json!(report.company_id)));
    }
    let filtered: Vec<Value> = rows
        .iter()
        .filter(|row| row_matches_filters(row, &report.filters))
        .map(|row| project_row(row, &report.fields))
        .collect();
    let (mut grouped, totals) = aggregate_rows(&filtered, report.group_by.as_deref());
    grouped.truncate(max_rows);
    Ok(json!({
        "report_id": report.id.to_string(),
        "source": report.source,
        "chart_type": report.chart_type,
        "group_by": report.group_by,
        "rows": grouped,
        "totals": totals,
    }))
}

/// Daily revenue series: one (date, total) pair per day, sorted by
/// date, from mirrored rows of the given sources with an amount.
/// Daily totals for one or more sources over the mirror. `None`
/// for `company_id` reads global rows only (no company or an empty
/// company) -- never every company's rows (C3 pilot 3: the forecast
/// handlers pass the verified caller scope, so an unresolved caller
/// forecasts over global rows only).
pub(crate) fn daily_series(state: &BiState, sources: &[&str], company_id: Option<&str>, currency: Option<&str>) -> Vec<(NaiveDate, f64)> {
    let mut by_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    for entry in state.source_records.iter() {
        let row = entry.value();
        if !sources.contains(&row.source.as_str()) {
            continue;
        }
        match company_id {
            Some(company) => {
                if row.company_id.as_deref() != Some(company) {
                    continue;
                }
            }
            None => {
                if row.company_id.as_deref().is_some_and(|value| !value.is_empty()) {
                    continue;
                }
            }
        }
        if let Some(currency) = currency {
            if row.currency.as_deref() != Some(currency) {
                continue;
            }
        }
        if let Some(amount) = row.amount {
            *by_day.entry(row.occurred_at.date_naive()).or_insert(0.0) += amount;
        }
    }
    by_day
        .into_iter()
        .map(|(date, total)| (date, round_money(total)))
        .collect()
}

/// Least-squares linear trend over daily totals: returns the slope
/// (amount change per day) and the intercept at day zero. Pure
/// arithmetic over the observed points -- no hidden model.
pub(crate) fn linear_trend(series: &[(NaiveDate, f64)]) -> (f64, f64) {
    let n = series.len() as f64;
    if series.len() < 2 {
        return (0.0, series.first().map(|(_, total)| *total).unwrap_or(0.0));
    }
    let first = series[0].0;
    let mut sum_x = 0.0f64;
    let mut sum_y = 0.0f64;
    let mut sum_xx = 0.0f64;
    let mut sum_xy = 0.0f64;
    for (date, total) in series {
        let x = (*date - first).num_days() as f64;
        sum_x += x;
        sum_y += total;
        sum_xx += x * x;
        sum_xy += x * total;
    }
    let denominator = n * sum_xx - sum_x * sum_x;
    if denominator.abs() < f64::EPSILON {
        return (0.0, sum_y / n);
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denominator;
    let intercept = (sum_y - slope * sum_x) / n;
    (slope, intercept)
}

/// Build a forecast response from a daily series: 7-day moving
/// average plus linear trend, projected forward `horizon_days`
/// days. Every response names its method and inputs. Returns an
/// "insufficient data" error when fewer than `MIN_FORECAST_DAYS`
/// distinct days are observed.
pub(crate) fn build_forecast(
    state: &BiState,
    kind: &str,
    sources: &[&str],
    company_id: Option<&str>,
    currency: Option<&str>,
    horizon_days: i64,
) -> UnifiedResult<Value> {
    let series = daily_series(state, sources, company_id, currency);
    if series.len() < MIN_FORECAST_DAYS {
        return Err(UnifiedError::unprocessable(format!(
            "Insufficient data: {kind} forecast needs at least {MIN_FORECAST_DAYS} days of observed history, found {} day(s)",
            series.len()
        ))
        .with_metadata("observed_days", json!(series.len()))
        .with_metadata("required_days", json!(MIN_FORECAST_DAYS))
        .with_metadata("sources", json!(sources)));
    }
    let window = series
        .iter()
        .rev()
        .take(FORECAST_WINDOW_DAYS as usize)
        .map(|(_, total)| *total)
        .collect::<Vec<f64>>();
    let moving_average = round_money(window.iter().sum::<f64>() / window.len() as f64);
    let (slope, intercept) = linear_trend(&series);
    let last_date = series.last().expect("series is non-empty").0;
    let first_date = series[0].0;
    let mut projections: Vec<Value> = Vec::new();
    for day in 1..=horizon_days {
        let date = last_date + Duration::days(day);
        let x = (date - first_date).num_days() as f64;
        let trend_value = intercept + slope * x;
        let projected = round_money((moving_average + trend_value) / 2.0);
        projections.push(json!({
            "date": date.format("%Y-%m-%d").to_string(),
            "projected_amount": projected,
            "moving_average_7d": moving_average,
            "trend_value": round_money(trend_value),
        }));
    }
    let total_projected = round_money(
        projections
            .iter()
            .filter_map(|row| row.get("projected_amount").and_then(Value::as_f64))
            .sum::<f64>(),
    );
    let result = json!({
        "kind": kind,
        "method": "moving_average_7d_plus_linear_trend",
        "inputs": {
            "sources": sources,
            "currency": currency,
            "company_id": company_id,
            "observed_days": series.len(),
            "window_days": FORECAST_WINDOW_DAYS,
            "from": series[0].0.format("%Y-%m-%d").to_string(),
            "to": last_date.format("%Y-%m-%d").to_string(),
        },
        "horizon_days": horizon_days,
        "daily_average_7d": moving_average,
        "trend_per_day": round_money(slope),
        "total_projected": total_projected,
        "projections": projections,
    });
    state.emit(
        "bi.forecast.generated",
        json!({
            "kind": kind,
            "sources": sources,
            "currency": currency,
            "horizon_days": horizon_days,
            "observed_days": series.len(),
            "total_projected": total_projected,
        }),
    );
    Ok(result)
}

/// Queue one scheduled delivery through `scheduler.enqueue` with
/// kind `bi.report.deliver`. Mirrors the events module's reminder
/// scheduling: the scheduler answers `{"queued": true, "job_id":
/// ...}` when it knows the kind, and `{"queued": false, "error":
/// "Unknown job kind: ..."}` today, because `bi.report.deliver`
/// is not in `KNOWN_KINDS` yet. The schedule is stored either way;
/// `scheduler_status` records the outcome honestly.

pub(crate) fn schedule_delivery(state: &BiState, schedule: &mut BiReportSchedule) {
    let Some(bus) = state.bus() else {
        schedule.scheduler_job_id = None;
        schedule.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "bi.report.deliver",
            "payload": {
                "report_id": schedule.report_id.to_string(),
                "schedule_id": schedule.id.to_string(),
                "company_id": schedule.company_id,
                "schedule": schedule.schedule,
                "recipients": schedule.recipients,
                "channel": schedule.channel,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            schedule.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            schedule.scheduler_status = "queued".to_string();
        }
        _ => {
            schedule.scheduler_job_id = None;
            schedule.scheduler_status = "declined".to_string();
        }
    }
}

/// Deliver a report now: execute it, emit `bi.report.delivered`,
/// and send one `notification.requested` per recipient. Returns
/// the row count delivered. A report with no source rows fails
/// with the execution error -- delivery never sends an empty
/// report as if it were data.
pub(crate) fn deliver_report_now(
    state: &BiState,
    report: &BiReport,
    schedule: Option<&BiReportSchedule>,
) -> UnifiedResult<usize> {
    let run = execute_report(state, report, MAX_RUN_ROWS)?;
    let row_count = run
        .get("totals")
        .and_then(|totals| totals.get("row_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let recipients: Vec<String> = schedule
        .map(|value| value.recipients.clone())
        .unwrap_or_default();
    let link = format!("/api/v1/bi/reports/{}", report.id);
    for recipient in &recipients {
        state.notify(
            "bi.report.delivered",
            recipient,
            &format!("Report ready: {}", report.name),
            &format!(
                "Report \"{}\" from source \"{}\" delivered with {row_count} row(s).",
                report.name, report.source
            ),
            Some(link.clone()),
        );
    }
    if let Some(schedule) = schedule {
        if let Some(mut stored) = state.schedules.get_mut(&schedule.id) {
            stored.last_delivered_at = Some(Utc::now());
            stored.updated_at = Utc::now();
        }
    }
    state.emit(
        "bi.report.delivered",
        json!({
            "report_id": report.id.to_string(),
            "schedule_id": schedule.map(|value| value.id.to_string()),
            "company_id": report.company_id,
            "recipients": recipients,
            "row_count": row_count,
        }),
    );
    Ok(row_count)
}

// ---------------------------------------------------------------------------
// Source-event listeners (mirror ingestion)
// ---------------------------------------------------------------------------


pub(crate) fn parse_forecast_query(query: &ForecastQuery) -> UnifiedResult<(Option<String>, i64)> {
    let currency = match &query.currency {
        Some(currency) => Some(valid_currency(currency)?),
        None => None,
    };
    let horizon_days = query.horizon_days.unwrap_or(30);
    if !(1..=90).contains(&horizon_days) {
        return Err(UnifiedError::bad_request(
            "horizon_days must be between 1 and 90",
        ));
    }
    Ok((currency, horizon_days))
}

/// Count distinct licenses that are active right now. Each
/// license is identified by its `ref_id`; the latest lifecycle
/// event per license decides its state: `license_issued` /
/// `license_activated` mean active, `license_suspended` /
/// `license_revoked` / `license_deactivated` / `deactivated` mean
/// inactive. Counting events instead double-counts one license
/// (`license_issued` and `license_activated` both fire in a single
/// lifecycle) and never drops deactivated licenses. Rows with no
/// `ref_id` cannot be correlated with any other row, so each one
/// is treated as its own license.
fn count_active_licenses(rows: &[Value]) -> f64 {
    let mut latest: BTreeMap<String, (DateTime<Utc>, String)> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(kind) = row.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(
            kind,
            "license_issued"
                | "license_activated"
                | "license_suspended"
                | "license_revoked"
                | "license_deactivated"
                | "deactivated"
        ) {
            continue;
        }
        let key = row
            .get("ref_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("__no_ref__:{index}"));
        let at = row
            .get("occurred_at")
            .and_then(Value::as_str)
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|parsed| parsed.with_timezone(&Utc))
            .unwrap_or_else(|| {
                DateTime::from_timestamp(0, 0).expect("epoch is a valid timestamp")
            });
        let replace = match latest.get(&key) {
            Some((previous_at, _)) => at >= *previous_at,
            None => true,
        };
        if replace {
            latest.insert(key, (at, kind.to_string()));
        }
    }
    latest
        .values()
        .filter(|(_, kind)| matches!(kind.as_str(), "license_issued" | "license_activated"))
        .count() as f64
}

pub(crate) fn compute_kpi(state: &BiState, kpi: &KpiDefinition, company_id: &str) -> Option<f64> {
    let from = match kpi.period.as_str() {
        "7d" => Some(Utc::now() - Duration::days(7)),
        "30d" => Some(Utc::now() - Duration::days(30)),
        "90d" => Some(Utc::now() - Duration::days(90)),
        _ => None,
    };
    let rows = read_source_rows(state, &kpi.source, Some(company_id), from, None);
    if rows.is_empty() {
        return None;
    }
    match kpi.metric.as_str() {
        "revenue_total" => {
            let total: f64 = rows
                .iter()
                .filter_map(|row| row.get("amount").and_then(Value::as_f64))
                .sum();
            Some(round_money(total))
        }
        "order_count" => Some(
            rows.iter()
                .filter(|row| {
                    row.get("kind").and_then(Value::as_str) == Some("order_completed")
                })
                .count() as f64,
        ),
        "ticket_count" => Some(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row.get("kind").and_then(Value::as_str),
                        Some("ticket_created") | Some("ticket_updated")
                    )
                })
                .count() as f64,
        ),
        "active_licenses" => Some(count_active_licenses(&rows)),
        "tasks_completed" => Some(
            rows.iter()
                .filter(|row| {
                    row.get("kind").and_then(Value::as_str) == Some("task_completed")
                })
                .count() as f64,
        ),
        "payment_count" => Some(
            rows.iter()
                .filter(|row| row.get("amount").and_then(Value::as_f64).is_some())
                .count() as f64,
        ),
        "average_order_value" => {
            let amounts: Vec<f64> = rows
                .iter()
                .filter(|row| {
                    row.get("kind").and_then(Value::as_str) == Some("order_completed")
                })
                .filter_map(|row| row.get("amount").and_then(Value::as_f64))
                .collect();
            if amounts.is_empty() {
                None
            } else {
                Some(round_money(
                    amounts.iter().sum::<f64>() / amounts.len() as f64,
                ))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    #[test]
    fn linear_trend_empty_and_single_point() {
        assert_eq!(linear_trend(&[]), (0.0, 0.0));
        assert_eq!(linear_trend(&[(date(2026, 1, 1), 7.0)]), (0.0, 7.0));
    }

    #[test]
    fn linear_trend_finds_slope_and_intercept() {
        let series = vec![
            (date(2026, 1, 1), 10.0),
            (date(2026, 1, 2), 11.0),
            (date(2026, 1, 3), 12.0),
        ];
        assert_eq!(linear_trend(&series), (1.0, 10.0));
    }

    #[test]
    fn linear_trend_flat_series_has_zero_slope() {
        let series = vec![
            (date(2026, 1, 1), 5.0),
            (date(2026, 1, 2), 5.0),
            (date(2026, 1, 3), 5.0),
        ];
        assert_eq!(linear_trend(&series), (0.0, 5.0));
    }

    #[test]
    fn row_matches_filters_checks_equality() {
        let row = serde_json::json!({"status": "paid", "amount": 10});
        assert!(row_matches_filters(&row, &serde_json::json!({"status": "paid"})));
        assert!(!row_matches_filters(&row, &serde_json::json!({"status": "refunded"})));
        assert!(!row_matches_filters(&row, &serde_json::json!({"missing": "x"})));
        assert!(row_matches_filters(&row, &serde_json::json!("not-an-object")));
    }

    #[test]
    fn row_matches_filters_applies_from_date() {
        let row = serde_json::json!({"occurred_at": "2026-01-10T00:00:00Z"});
        assert!(row_matches_filters(
            &row,
            &serde_json::json!({"from": "2026-01-05T00:00:00Z"})
        ));
        assert!(!row_matches_filters(
            &row,
            &serde_json::json!({"from": "2026-01-15T00:00:00Z"})
        ));
    }

    #[test]
    fn aggregate_rows_totals_amounts() {
        let rows = vec![
            serde_json::json!({"amount": 10.0}),
            serde_json::json!({"amount": 20.5}),
            serde_json::json!({"note": "no amount"}),
        ];
        let (_grouped, totals) = aggregate_rows(&rows, None);
        assert_eq!(totals["row_count"], 3);
        assert_eq!(totals["total_amount"].as_f64().unwrap(), 30.5);
        assert_eq!(totals["amount_rows"], 2);
        assert_eq!(totals["average_amount"].as_f64().unwrap(), 15.25);
    }

    #[test]
    fn aggregate_rows_empty_input_has_zero_totals() {
        let (_grouped, totals) = aggregate_rows(&[], None);
        assert_eq!(totals["row_count"], 0);
        assert_eq!(totals["total_amount"].as_f64().unwrap(), 0.0);
        assert!(totals["average_amount"].is_null());
    }

    #[test]
    fn parse_forecast_query_defaults_horizon() {
        let query = ForecastQuery {
            source: None,
            currency: Some("usd".to_string()),
            horizon_days: None,
        };
        assert_eq!(parse_forecast_query(&query).unwrap(), (Some("USD".to_string()), 30));
    }

    #[test]
    fn row_matches_filters_rejects_unparseable_from_bound() {
        let row = serde_json::json!({"occurred_at": "2026-01-10T00:00:00Z"});
        assert!(!row_matches_filters(&row, &serde_json::json!({"from": "not-a-date"})));
    }

    #[test]
    fn row_matches_filters_requires_occurred_at_when_bound_set() {
        let row = serde_json::json!({"status": "paid"});
        assert!(!row_matches_filters(
            &row,
            &serde_json::json!({"from": "2026-01-05T00:00:00Z"})
        ));
        assert!(!row_matches_filters(
            &row,
            &serde_json::json!({"to": "2026-01-15T00:00:00Z"})
        ));
        // Without any bound, a row without occurred_at still matches.
        assert!(row_matches_filters(&row, &serde_json::json!({"status": "paid"})));
    }

    #[test]
    fn row_matches_filters_rejects_unparseable_occurred_at() {
        let row = serde_json::json!({"occurred_at": "yesterday"});
        assert!(!row_matches_filters(
            &row,
            &serde_json::json!({"from": "2026-01-05T00:00:00Z"})
        ));
    }

    #[test]
    fn row_matches_filters_applies_to_date() {
        let row = serde_json::json!({"occurred_at": "2026-01-10T00:00:00Z"});
        assert!(row_matches_filters(
            &row,
            &serde_json::json!({"to": "2026-01-15T00:00:00Z"})
        ));
        assert!(!row_matches_filters(
            &row,
            &serde_json::json!({"to": "2026-01-05T00:00:00Z"})
        ));
    }

    fn test_report(company_id: &str) -> BiReport {
        BiReport {
            id: uuid::Uuid::new_v4(),
            company_id: company_id.to_string(),
            owner_id: "owner-1".to_string(),
            name: "Report".to_string(),
            description: None,
            source: "billing".to_string(),
            fields: vec![],
            filters: serde_json::json!({}),
            group_by: None,
            chart_type: "table".to_string(),
            visibility: "private".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn execute_report_row_limit_is_caller_chosen() {
        let state = BiState::default();
        for index in 0..5 {
            record_source_row(
                &state,
                "billing",
                "payment",
                Some("co-1".to_string()),
                Some(format!("cust-{index}")),
                Some(10.0),
                Some("USD".to_string()),
                Utc::now(),
                Some(format!("pay-{index}")),
            );
        }
        let report = test_report("co-1");
        // A run cap of 2 returns 2 rows, but totals still cover
        // all 5 filtered rows.
        let capped = execute_report(&state, &report, 2).unwrap();
        assert_eq!(capped["rows"].as_array().unwrap().len(), 2);
        assert_eq!(capped["totals"]["row_count"], 5);
        // The run and export caps both return the full set when
        // the data fits under them.
        let run = execute_report(&state, &report, MAX_RUN_ROWS).unwrap();
        assert_eq!(run["rows"].as_array().unwrap().len(), 5);
        let export = execute_report(&state, &report, MAX_EXPORT_ROWS).unwrap();
        assert_eq!(export["rows"].as_array().unwrap().len(), 5);
    }

    fn license_kpi() -> KpiDefinition {
        KpiDefinition {
            metric: "active_licenses".to_string(),
            source: "licensing".to_string(),
            period: "all".to_string(),
        }
    }

    fn record_license(
        state: &BiState,
        kind: &str,
        ref_id: &str,
        at: DateTime<Utc>,
    ) {
        record_source_row(
            state,
            "licensing",
            kind,
            Some("co-1".to_string()),
            None,
            None,
            None,
            at,
            Some(ref_id.to_string()),
        );
    }

    #[test]
    fn active_licenses_counts_distinct_licenses_by_latest_state() {
        let state = BiState::default();
        let base = Utc::now() - Duration::days(10);
        let kpi = license_kpi();
        // Issued + activated for the same license is one active
        // license, not two.
        record_license(&state, "license_issued", "lic-1", base);
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(1.0));
        record_license(&state, "license_activated", "lic-1", base + Duration::hours(1));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(1.0));
        // Suspended means inactive; re-activation revives it.
        record_license(&state, "license_suspended", "lic-1", base + Duration::hours(2));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(0.0));
        record_license(&state, "license_activated", "lic-1", base + Duration::hours(3));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(1.0));
        // Deactivation drops it again, and revocation keeps it out.
        record_license(&state, "license_deactivated", "lic-1", base + Duration::hours(4));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(0.0));
        record_license(&state, "license_revoked", "lic-1", base + Duration::hours(5));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(0.0));
        // A second license counts separately.
        record_license(&state, "license_issued", "lic-2", base + Duration::hours(6));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(1.0));
        record_license(&state, "license_revoked", "lic-2", base + Duration::hours(7));
        assert_eq!(compute_kpi(&state, &kpi, "co-1"), Some(0.0));
    }

    #[test]
    fn mirror_event_uses_license_id_as_ref_id() {
        let state = BiState::default();
        mirror_event(
            &state,
            "licensing",
            "license_issued",
            &serde_json::json!({"license_id": "lic-9", "company_id": "co-1"}),
        );
        let rows: Vec<SourceRecord> = state
            .source_records
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ref_id.as_deref(), Some("lic-9"));
    }

    #[test]
    fn parse_forecast_query_rejects_bad_horizon() {
        let zero = ForecastQuery {
            source: None,
            currency: None,
            horizon_days: Some(0),
        };
        assert!(parse_forecast_query(&zero).is_err());
        let high = ForecastQuery {
            source: None,
            currency: None,
            horizon_days: Some(91),
        };
        assert!(parse_forecast_query(&high).is_err());
        let ok = ForecastQuery {
            source: None,
            currency: None,
            horizon_days: Some(90),
        };
        assert_eq!(parse_forecast_query(&ok).unwrap(), (None, 90));
    }
}
