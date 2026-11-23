// bi module -- Validation helpers for inputs handled by this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{json, Value};
use std::collections::HashSet;

use super::*;

/// Money: finite, non-negative, rounded to 4 decimal places.
pub(crate) fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

/// A schedule is either an interval in seconds or five cron-like
/// fields where each field is `*`, `*/N`, or a plain number -- the
/// same shape the scheduler module validates. Every cron field is
/// range-checked (minute 0-59, hour 0-23, day-of-month 1-31, month
/// 1-12, day-of-week 0-7); a step `*/N` must also fit the field's
/// range, so `*/60` in the minute field is rejected.
pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() || schedule.len() > MAX_SCHEDULE_LEN {
        return false;
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        return seconds > 0;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    // (minimum, maximum) per cron field, in field order.
    const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields.iter().zip(RANGES).all(|(field, (min, max))| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0 && n <= max);
        }
        field.parse::<u32>().is_ok_and(|n| (min..=max).contains(&n))
    })
}

pub(crate) fn validate_source(source: &str) -> UnifiedResult<String> {
    let source = source.trim().to_lowercase();
    if !ALLOWED_SOURCES.contains(&source.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "source must be one of: {}",
            ALLOWED_SOURCES.join(", ")
        )));
    }
    Ok(source)
}

pub(crate) fn validate_chart_type(chart_type: &str) -> UnifiedResult<String> {
    let chart_type = chart_type.trim().to_lowercase();
    if !ALLOWED_CHART_TYPES.contains(&chart_type.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "chart_type must be one of: {}",
            ALLOWED_CHART_TYPES.join(", ")
        )));
    }
    Ok(chart_type)
}

pub(crate) fn validate_fields(source: &str, fields: &[String]) -> UnifiedResult<Vec<String>> {
    if fields.len() > MAX_FIELDS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_FIELDS} fields per report"
        )));
    }
    let allowed = allowed_fields(source);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for field in fields {
        let field = field.trim().to_lowercase();
        if field.is_empty() || field.len() > MAX_FIELD_LEN {
            return Err(UnifiedError::bad_request(
                "field names must not be empty and must be at most 128 characters",
            ));
        }
        if !allowed.contains(&field.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "field \"{field}\" is not available for source \"{source}\""
            )));
        }
        if !seen.insert(field.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "duplicate field \"{field}\""
            )));
        }
        out.push(field);
    }
    Ok(out)
}

pub(crate) fn validate_filters(filters: &Value) -> UnifiedResult<Value> {
    if filters.is_null() {
        return Ok(json!({}));
    }
    if !filters.is_object() {
        return Err(UnifiedError::bad_request(
            "filters must be a JSON object of exact-match field/value pairs",
        ));
    }
    // `from` / `to` bounds must be valid RFC 3339 timestamps at
    // write time. An unparseable bound used to be stored and then
    // silently ignored at read time (fail-open filtering), so it is
    // rejected here instead.
    for bound in ["from", "to"] {
        if let Some(raw) = filters.get(bound) {
            let Some(text) = raw.as_str() else {
                return Err(UnifiedError::bad_request(format!(
                    "filter \"{bound}\" must be an RFC 3339 timestamp string"
                )));
            };
            if DateTime::parse_from_rfc3339(text).is_err() {
                return Err(UnifiedError::bad_request(format!(
                    "filter \"{bound}\" must be a valid RFC 3339 timestamp, got \"{text}\""
                )));
            }
        }
    }
    Ok(filters.clone())
}

pub(crate) fn validate_kpis(kpis: &[KpiDefinition]) -> UnifiedResult<Vec<KpiDefinition>> {
    if kpis.len() > MAX_KPIS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_KPIS} KPIs per dashboard"
        )));
    }
    let mut out: Vec<KpiDefinition> = Vec::new();
    for kpi in kpis {
        let metric = kpi.metric.trim().to_lowercase();
        if !ALLOWED_METRICS.contains(&metric.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "metric must be one of: {}",
                ALLOWED_METRICS.join(", ")
            )));
        }
        let source = validate_source(&kpi.source)?;
        let period = kpi.period.trim().to_lowercase();
        if !ALLOWED_PERIODS.contains(&period.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "period must be one of: {}",
                ALLOWED_PERIODS.join(", ")
            )));
        }
        if metric.len() > MAX_METRIC_LEN || period.len() > MAX_PERIOD_LEN {
            return Err(UnifiedError::bad_request("metric or period too long"));
        }
        out.push(KpiDefinition {
            metric,
            source,
            period,
        });
    }
    Ok(out)
}

pub(crate) fn load_report(state: &BiState, raw_id: &str) -> UnifiedResult<BiReport> {
    let id = parse_uuid(raw_id, "report")?;
    state
        .reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Report not found: {id}")))
}

pub(crate) fn load_schedule(state: &BiState, raw_id: &str) -> UnifiedResult<BiReportSchedule> {
    let id = parse_uuid(raw_id, "schedule")?;
    state
        .schedules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Schedule not found: {id}")))
}

pub(crate) fn load_dashboard(state: &BiState, raw_id: &str) -> UnifiedResult<BiKpiDashboard> {
    let id = parse_uuid(raw_id, "dashboard")?;
    state
        .dashboards
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Dashboard not found: {id}")))
}

/// A report is visible to its owner, or to anyone in the same
/// company when it is shared. Manage capability bypasses the
/// ownership check.
///
/// `company_id` must be the caller's company resolved server-side
/// (see `caller_company` in `state.rs`); `None` means the caller has no
/// resolvable company, i.e. an unresolved caller, who may read only
/// global (empty-`company_id`) shared reports — never another
/// company's shared report (the shared `CallerScope::Unresolved`
/// rule; platform access is never the default for a missing
/// company). Never pass `None` for a caller whose company could
/// be resolved.
pub(crate) fn can_read_report(report: &BiReport, actor: &str, company_id: Option<&str>) -> bool {
    if report.owner_id == actor {
        return true;
    }
    if report.visibility == "shared" {
        return match company_id {
            Some(company) => report.company_id == company,
            None => report.company_id.is_empty(),
        };
    }
    false
}

pub(crate) fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

// ---------------------------------------------------------------------------
// Source data: mirror ingestion + read hooks
// ---------------------------------------------------------------------------

/// Read one string field from an event payload, trying each key in
/// order. Source events carry the same value under different key
/// names, so the mirror reads defensively instead of assuming one
/// shape.
pub(crate) fn payload_string(data: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = data.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub(crate) fn payload_number(data: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = data.get(*key).and_then(Value::as_f64) {
            if value.is_finite() {
                return Some(value);
            }
        }
        if let Some(value) = data.get(*key).and_then(Value::as_str) {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                if parsed.is_finite() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

pub(crate) fn payload_time(data: &Value, keys: &[&str]) -> Option<DateTime<Utc>> {
    for key in keys {
        if let Some(value) = data.get(*key).and_then(Value::as_str) {
            if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
                return Some(parsed.with_timezone(&Utc));
            }
        }
    }
    None
}

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match super::state::caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_money_keeps_four_decimals() {
        assert_eq!(round_money(1.23456789), 1.2346);
        assert_eq!(round_money(2.5), 2.5);
        assert_eq!(round_money(-1.23456), -1.2346);
    }

    #[test]
    fn valid_currency_normalizes_to_uppercase() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency(" usd ").unwrap(), "USD");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("U5D").is_err());
    }

    #[test]
    fn valid_schedule_accepts_seconds_and_cron() {
        assert!(valid_schedule("3600"));
        assert!(valid_schedule("  60  "));
        assert!(valid_schedule("0 0 * * *"));
        assert!(valid_schedule("*/5 * * * *"));
    }

    #[test]
    fn valid_schedule_rejects_bad_values() {
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("0"));
        assert!(!valid_schedule("not a schedule"));
        assert!(!valid_schedule("0 0 0"));
    }

    #[test]
    fn valid_schedule_rejects_out_of_range_fields() {
        assert!(!valid_schedule("99 99 99 99 99"));
        assert!(!valid_schedule("60 * * * *"));
        assert!(!valid_schedule("0 24 * * *"));
        assert!(!valid_schedule("0 0 32 * *"));
        assert!(!valid_schedule("0 0 0 * *"));
        assert!(!valid_schedule("0 0 * 13 *"));
        assert!(!valid_schedule("0 0 * 0 *"));
        assert!(!valid_schedule("0 0 * * 8"));
        assert!(!valid_schedule("*/60 * * * *"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn valid_schedule_accepts_field_boundaries() {
        assert!(valid_schedule("59 23 31 12 7"));
        assert!(valid_schedule("0 0 1 1 0"));
        assert!(valid_schedule("*/59 * * * *"));
        assert!(valid_schedule("* * * * *"));
    }

    #[test]
    fn validate_filters_validates_from_to_as_rfc3339() {
        assert!(validate_filters(&serde_json::json!({"from": "2026-01-05T00:00:00Z"})).is_ok());
        assert!(validate_filters(&serde_json::json!({"to": "2026-01-05T00:00:00+07:00"})).is_ok());
        assert!(validate_filters(&serde_json::json!({"status": "paid"})).is_ok());
        assert!(validate_filters(&serde_json::json!({"from": "not-a-date"})).is_err());
        assert!(validate_filters(&serde_json::json!({"to": "2026-13-01T00:00:00Z"})).is_err());
        assert!(validate_filters(&serde_json::json!({"from": 123})).is_err());
    }

    #[test]
    fn can_read_report_scopes_shared_reports_to_company() {
        let report = BiReport {
            id: uuid::Uuid::new_v4(),
            company_id: "co-1".to_string(),
            owner_id: "owner-1".to_string(),
            name: "Report".to_string(),
            description: None,
            source: "billing".to_string(),
            fields: vec![],
            filters: serde_json::json!({}),
            group_by: None,
            chart_type: "table".to_string(),
            visibility: "shared".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(can_read_report(&report, "owner-1", Some("co-2")));
        assert!(can_read_report(&report, "someone", Some("co-1")));
        assert!(!can_read_report(&report, "someone", Some("co-2")));
        // No resolvable company means an unresolved caller, who may
        // read only global (empty-company) shared reports, never
        // another company's shared report.
        assert!(!can_read_report(&report, "someone", None));
        let private = BiReport {
            visibility: "private".to_string(),
            ..report.clone()
        };
        assert!(can_read_report(&private, "owner-1", Some("co-2")));
        assert!(!can_read_report(&private, "someone", Some("co-1")));
        assert!(!can_read_report(&private, "someone", None));
    }

    #[test]
    fn validate_source_normalizes_case() {
        assert_eq!(validate_source("billing").unwrap(), "billing");
        assert_eq!(validate_source("BILLING").unwrap(), "billing");
        assert!(validate_source("nope").is_err());
    }

    #[test]
    fn validate_chart_type_accepts_known_types() {
        assert_eq!(validate_chart_type("bar").unwrap(), "bar");
        assert_eq!(validate_chart_type("BAR").unwrap(), "bar");
        assert!(validate_chart_type("radar").is_err());
    }

    #[test]
    fn validate_fields_normalizes_and_rejects_duplicates() {
        let ok = validate_fields(
            "billing",
            &["Amount".to_string(), "currency".to_string()],
        )
        .unwrap();
        assert_eq!(ok, vec!["amount".to_string(), "currency".to_string()]);
        assert!(validate_fields(
            "billing",
            &["amount".to_string(), "amount".to_string()]
        )
        .is_err());
        assert!(validate_fields("billing", &["nope".to_string()]).is_err());
    }

    #[test]
    fn validate_fields_enforces_max_fields() {
        let many: Vec<String> = (0..33).map(|i| format!("amount{i}")).collect();
        assert!(validate_fields("billing", &many).is_err());
    }

    #[test]
    fn escape_csv_field_quotes_only_when_needed() {
        assert_eq!(escape_csv_field("plain"), "plain");
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert!(escape_csv_field("say \"hi\"").starts_with('"'));
        assert!(escape_csv_field("say \"hi\"").contains("\"\""));
        assert_eq!(escape_csv_field("line\nbreak"), "\"line\nbreak\"");
    }
}
