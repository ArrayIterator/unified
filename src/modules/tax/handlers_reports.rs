// Tax module — settings and report handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<TaxSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        Ok(state.settings_for(company_id))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<UpsertTaxSettings>,
) -> HttpResponse {
    let result: UnifiedResult<TaxSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        let mut settings = state.settings_for(company_id);
        if let Some(month) = input.fiscal_year_start_month {
            if !(1..=12).contains(&month) {
                return Err(UnifiedError::bad_request(
                    "fiscal_year_start_month must be between 1 and 12",
                ));
            }
            settings.fiscal_year_start_month = month;
        }
        if let Some(day) = input.fiscal_year_start_day {
            if !(1..=31).contains(&day) {
                return Err(UnifiedError::bad_request(
                    "fiscal_year_start_day must be between 1 and 31",
                ));
            }
            settings.fiscal_year_start_day = day;
        }
        if let Some(frequency) = input.filing_frequency {
            if !FILING_FREQUENCIES.contains(&frequency.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "filing_frequency must be one of: {}",
                    FILING_FREQUENCIES.join(", ")
                )));
            }
            settings.filing_frequency = frequency;
        }
        if let Some(currency) = input.default_currency {
            let currency = currency.trim().to_uppercase();
            if !valid_currency(&currency) {
                return Err(UnifiedError::bad_request(
                    "default_currency must be a 3-letter code",
                ));
            }
            settings.default_currency = currency;
        }
        if let Some(country) = input.default_country_code {
            let country = country.trim().to_uppercase();
            if !country.is_empty() && !valid_country_code(&country) {
                return Err(UnifiedError::bad_request(
                    "default_country_code must be a 2-letter ISO code",
                ));
            }
            settings.default_country_code = if country.is_empty() {
                None
            } else {
                Some(country)
            };
        }
        if let Some(region) = input.default_region_code {
            let region = region.trim().to_string();
            if !region.is_empty() && !valid_region_code(&region) {
                return Err(UnifiedError::bad_request(
                    "default_region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
            settings.default_region_code = if region.is_empty() {
                None
            } else {
                Some(region)
            };
        }
        if let Some(days) = input.reminder_days_before {
            if days > 90 {
                return Err(UnifiedError::bad_request(
                    "reminder_days_before must be between 0 and 90",
                ));
            }
            settings.reminder_days_before = days;
        }
        if let Some(days) = input.filing_due_days_after_period {
            if days > 365 {
                return Err(UnifiedError::bad_request(
                    "filing_due_days_after_period must be between 0 and 365",
                ));
            }
            settings.filing_due_days_after_period = days;
        }
        if let Some(user_id) = input.notify_user_id {
            settings.notify_user_id = if user_id.trim().is_empty() {
                None
            } else {
                Some(user_id.trim().to_string())
            };
        }
        settings.updated_at = now();
        state.settings.insert(company_id, settings.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "settings_updated",
            "tax_settings",
            &settings.id,
            "tax.settings_updated",
            json!({
                "filing_frequency": settings.filing_frequency,
                "default_currency": settings.default_currency,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Handlers: reports
// ---------------------------------------------------------------------------

/// Parse the `period_start` / `period_end` query filters. Both are
/// optional; when absent the range is open-ended.
pub(crate) fn report_range(
    params: &PaginationParams,
) -> UnifiedResult<(Option<NaiveDate>, Option<NaiveDate>)> {
    let start = params
        .filters
        .get("period_start")
        .map(|value| parse_date(value, "period_start"))
        .transpose()?;
    let end = params
        .filters
        .get("period_end")
        .map(|value| parse_date(value, "period_end"))
        .transpose()?;
    if let (Some(start), Some(end)) = (start, end) {
        if end < start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
    }
    Ok((start, end))
}

pub(crate) fn in_report_range(
    day: NaiveDate,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> bool {
    start.is_none_or(|from| day >= from) && end.is_none_or(|until| day <= until)
}

pub(crate) fn report_computations(
    state: &TaxState,
    company_id: Uuid,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Vec<TaxComputation> {
    state
        .computations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|computation| {
            computation.company_id == company_id
                && in_report_range(computation.computed_at.date_naive(), start, end)
        })
        .collect()
}

pub(crate) fn summary_value(
    state: &TaxState,
    company_id: Uuid,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Value {
    let computations = report_computations(state, company_id, start, end);
    // Money sums are integer minor units. The rows below hold
    // minor units while accumulating and are converted to major
    // units with the row's currency at the end, so the JSON and
    // CSV shapes stay major units, as before. The top-level
    // totals span currencies, as before, and convert with the
    // standard two-decimal fallback.
    let mut base_total: i64 = 0;
    let mut tax_total: i64 = 0;
    let mut grand_total: i64 = 0;
    let mut by_jurisdiction: Vec<Value> = Vec::new();
    let mut by_period: Vec<Value> = Vec::new();
    for computation in &computations {
        base_total += computation.base_amount;
        tax_total += computation.tax_total;
        grand_total += computation.grand_total;
        let jurisdiction = format!(
            "{}-{}",
            computation.country_code.clone().unwrap_or_default(),
            computation.region_code.clone().unwrap_or_default()
        );
        let position = by_jurisdiction.iter().position(|entry| {
            entry.get("jurisdiction").and_then(|v| v.as_str()) == Some(&jurisdiction)
        });
        let row = match position {
            Some(index) => &mut by_jurisdiction[index],
            None => {
                by_jurisdiction.push(json!({
                    "jurisdiction": jurisdiction,
                    "country_code": computation.country_code,
                    "region_code": computation.region_code,
                    "currency": computation.currency,
                    "computation_count": 0,
                    "base_total": 0,
                    "tax_total": 0,
                    "grand_total": 0,
                }));
                by_jurisdiction.last_mut().expect("row just pushed")
            }
        };
        let object = row.as_object_mut().expect("summary row is an object");
        let count = object
            .get("computation_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        object.insert("computation_count".to_string(), json!(count + 1));
        for (field, amount) in [
            ("base_total", computation.base_amount),
            ("tax_total", computation.tax_total),
            ("grand_total", computation.grand_total),
        ] {
            let current = object.get(field).and_then(|v| v.as_i64()).unwrap_or(0);
            object.insert(field.to_string(), json!(current + amount));
        }
        let period = computation.computed_at.format("%Y-%m").to_string();
        let position = by_period
            .iter()
            .position(|entry| entry.get("period").and_then(|v| v.as_str()) == Some(&period));
        let row = match position {
            Some(index) => &mut by_period[index],
            None => {
                by_period.push(json!({
                    "period": period,
                    "computation_count": 0,
                    "base_total": 0,
                    "tax_total": 0,
                    "grand_total": 0,
                }));
                by_period.last_mut().expect("row just pushed")
            }
        };
        let object = row.as_object_mut().expect("summary row is an object");
        let count = object
            .get("computation_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        object.insert("computation_count".to_string(), json!(count + 1));
        for (field, amount) in [
            ("base_total", computation.base_amount),
            ("tax_total", computation.tax_total),
            ("grand_total", computation.grand_total),
        ] {
            let current = object.get(field).and_then(|v| v.as_i64()).unwrap_or(0);
            object.insert(field.to_string(), json!(current + amount));
        }
    }
    // Boundary conversion: minor units become the major-unit
    // numbers the JSON and CSV outputs have always carried.
    for row in &mut by_jurisdiction {
        let currency = row
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_string();
        convert_summary_row_to_major(row, &currency);
    }
    for row in &mut by_period {
        convert_summary_row_to_major(row, "USD");
    }
    by_jurisdiction.sort_by(|a, b| {
        a.get("jurisdiction")
            .and_then(|v| v.as_str())
            .cmp(&b.get("jurisdiction").and_then(|v| v.as_str()))
    });
    by_period.sort_by(|a, b| {
        a.get("period")
            .and_then(|v| v.as_str())
            .cmp(&b.get("period").and_then(|v| v.as_str()))
    });
    json!({
        "company_id": company_id.to_string(),
        "period_start": start,
        "period_end": end,
        "computation_count": computations.len(),
        "base_total": minor_to_major(base_total, "USD"),
        "tax_total": minor_to_major(tax_total, "USD"),
        "grand_total": minor_to_major(grand_total, "USD"),
        "by_jurisdiction": by_jurisdiction,
        "by_period": by_period,
    })
}

/// Convert one summary row's minor-unit totals to major units in
/// place, for the row's currency.
fn convert_summary_row_to_major(row: &mut Value, currency: &str) {
    let Some(object) = row.as_object_mut() else {
        return;
    };
    for field in ["base_total", "tax_total", "grand_total"] {
        let minor = object
            .get(field)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        object.insert(field.to_string(), json!(minor_to_major(minor, currency)));
    }
}

pub(crate) async fn report_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let (start, end) = report_range(&query.into_inner())?;
        Ok(summary_value(&state, company_id, start, end))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn report_jurisdictions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let (start, end) = report_range(&query.into_inner())?;
        let summary = summary_value(&state, company_id, start, end);
        Ok(summary
            .get("by_jurisdiction")
            .cloned()
            .unwrap_or(Value::Null))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn report_periods(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let (start, end) = report_range(&query.into_inner())?;
        let summary = summary_value(&state, company_id, start, end);
        Ok(summary.get("by_period").cloned().unwrap_or(Value::Null))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn report_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<(Uuid, Option<NaiveDate>, Option<NaiveDate>, String)> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let params = query.into_inner();
        let format = params
            .filters
            .get("format")
            .cloned()
            .unwrap_or_else(|| "json".to_string());
        if !matches!(format.as_str(), "json" | "csv") {
            return Err(UnifiedError::bad_request(
                "format must be one of: json, csv",
            ));
        }
        let (start, end) = report_range(&params)?;
        Ok((company_id, start, end, format))
    })();
    let (company_id, start, end, format) = match result {
        Ok(values) => values,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let summary = summary_value(&state, company_id, start, end);
    if format == "csv" {
        let mut csv = String::from(
            "period,country_code,region_code,currency,computation_count,base_total,tax_total,grand_total\n",
        );
        if let Some(rows) = summary.get("by_jurisdiction").and_then(|v| v.as_array()) {
            for row in rows {
                let period = summary
                    .get("period_start")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{}\n",
                    escape_csv(period),
                    escape_csv(
                        row.get("country_code")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    ),
                    escape_csv(
                        row.get("region_code")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    ),
                    escape_csv(row.get("currency").and_then(|v| v.as_str()).unwrap_or("")),
                    row.get("computation_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    row.get("base_total")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    row.get("tax_total").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    row.get("grand_total")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                ));
            }
        }
        return HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                "attachment; filename=\"tax-report.csv\"",
            ))
            .body(csv);
    }
    json::success_response(summary, 200)
}
