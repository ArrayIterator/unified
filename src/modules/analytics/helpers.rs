//! analytics - helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{ http::header, HttpRequest, HttpResponse };
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;



// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------
/// True when `haystack` matches the free-text search in `params`
/// (case-insensitive substring); always true when no search is set.
pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}


pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks), the
/// same shape as the settings and school-fees modules.
pub(crate) struct AnalyticsCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn analytics_caller(req: &HttpRequest) -> UnifiedResult<AnalyticsCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(AnalyticsCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn analytics_check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn analytics_require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if analytics_check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authorize one request: caller identity plus one capability.
/// Returns the caller on success; 401 when an identity header is
/// missing, 403 when the role does not hold the permission.
pub(crate) fn analytics_authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<AnalyticsCaller> {
    let found = analytics_caller(req)?;
    analytics_require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `analytics_authorize` or any
/// capability check.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
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
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3 task 315):
/// a write requires a resolved company scope.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// True when a stored record belongs to the caller's company scope.
/// With no caller scope (an unresolved caller) only global records
/// match: unscoped records are visible, every company-owned record
/// is denied (the shared `CallerScope::Unresolved` rule). With a
/// scope, records carrying exactly that company match, plus unscoped
/// (global/legacy) records, which are shared content visible to any
/// resolved scope (C3 task 320, rule A).
/// Only `SaleRecord` carries a company in this module; every other
/// record type is gate-only (see the pilot-3 results note).
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}



pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}


/// Money: amounts inside the server are integer minor units
/// (`i64` cents for two-decimal currencies, whole units for
/// zero-decimal ones). JSON and hook payloads still carry major
/// units, converted at the boundary by `major_to_minor` /
/// `minor_to_major` (same shape as the store and billing modules).
/// Struct fields use `unified_core::money::serde_major` instead,
/// which assumes two decimal places; see that module's docs.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert a major-unit amount to integer minor units, rounding
/// half away from zero — the same rounding the old `round_money`
/// float math applied in cents.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
}

/// Convert integer minor units to major units, as JSON and hook
/// payloads carry them. This is the boundary conversion out of the
/// integer representation; business logic keeps minor units.
/// The round trip through cents is exact for minor-unit values.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        ((amount_minor as f64 / 100.0) * 100.0).round() / 100.0
    }
}


pub(crate) fn require_name(name: &str) -> UnifiedResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if name.chars().count() > 200 {
        return Err(UnifiedError::bad_request("name must be at most 200 characters"));
    }
    Ok(name)
}


pub(crate) fn valid_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic())
}


pub(crate) fn parse_datetime(value: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 date-time")))
}


/// Resolve a date range from `from`/`to` (RFC 3339) or a `range` preset
/// (`7d`, `30d`, `90d`, `365d`). Defaults to the last 30 days.
pub(crate) fn resolve_range(from: Option<&str>, to: Option<&str>, range: Option<&str>) -> UnifiedResult<(DateTime<Utc>, DateTime<Utc>)> {
    let end = match to {
        Some(value) => parse_datetime(value, "to")?,
        None => now(),
    };
    let start = match (from, range) {
        (Some(value), _) => parse_datetime(value, "from")?,
        (None, Some(preset)) => {
            let days: i64 = match preset {
                "7d" => 7,
                "30d" => 30,
                "90d" => 90,
                "365d" => 365,
                _ => {
                    return Err(UnifiedError::bad_request(
                        "range must be one of: 7d, 30d, 90d, 365d",
                    ))
                }
            };
            end - chrono::Duration::days(days)
        }
        (None, None) => end - chrono::Duration::days(30),
    };
    if start >= end {
        return Err(UnifiedError::bad_request("from must be before to"));
    }
    Ok((start, end))
}


pub(crate) fn day_key(moment: DateTime<Utc>) -> String {
    moment.format("%Y-%m-%d").to_string()
}


/// A schedule is either an interval in seconds (`"300"`) or five
/// cron-like fields (`"*/5 * * * *"`), where each field is `*`, `*/N`,
/// a plain number, or a comma list of plain numbers. Same rule as the
/// scheduler module's `valid_schedule`.
///
/// An interval must be positive: `"0"` parses as a number but
/// `next_run_at` can never resolve it, so it is rejected here. Cron
/// fields are range-checked (minute 0-59, hour 0-23, day-of-month
/// 1-31, month 1-12, day-of-week 0-7, where 7 is an alias for Sunday),
/// so a schedule such as `"99 99 99 99 99"` is rejected instead of
/// being created in a state that never runs.
pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if let Ok(seconds) = schedule.parse::<i64>() {
        return seconds > 0;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    const BOUNDS: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields
        .iter()
        .zip(BOUNDS)
        .all(|(field, (min, max))| valid_cron_field(field, min, max))
}


/// Check one cron field against its allowed range (`min..=max`).
/// Accepts `*`, `*/N` with `N > 0`, or a comma list of plain numbers.
/// A step is also rejected when no value in the range could ever match
/// it under the `value % step == 0` rule used by `field_matches`
/// (for ranges starting at 1, the step itself must fall inside the
/// range; ranges starting at 0 always match at 0).
fn valid_cron_field(field: &str, min: u32, max: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        return step
            .parse::<u32>()
            .is_ok_and(|n| n > 0 && (min == 0 || n <= max));
    }
    field
        .split(',')
        .all(|part| part.parse::<u32>().is_ok_and(|n| (min..=max).contains(&n)))
}


pub(crate) fn field_matches(field: &str, value: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        if let Ok(step) = step.parse::<u32>() {
            return step > 0 && value % step == 0;
        }
        return false;
    }
    field
        .split(',')
        .any(|part| part.parse::<u32>().is_ok_and(|number| number == value))
}


/// Day-of-week match for a cron field. `weekday` is 0 (Sunday) .. 6
/// (Saturday), matching standard cron numbering, where both 0 and 7
/// mean Sunday. This is separate from `field_matches` because that
/// helper has no way to know that 7 is an alias for 0.
pub(crate) fn dow_matches(field: &str, weekday: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        if let Ok(step) = step.parse::<u32>() {
            return step > 0 && weekday % step == 0;
        }
        return false;
    }
    field.split(',').any(|part| {
        part.parse::<u32>().is_ok_and(|number| {
            let number = if number == 7 { 0 } else { number };
            number == weekday
        })
    })
}


/// Next run time for a schedule, searching minute by minute up to 8 days
/// ahead. Interval schedules (`"300"`) resolve exactly; cron-like
/// schedules resolve to the next matching minute. Returns `None` when
/// no match is found inside the search window.
///
/// Day-of-month and day-of-week use the standard cron rule: when both
/// fields are restricted (neither is `*`), a day matches if either
/// field matches; when only one is restricted, that field alone must
/// match. Day-of-week follows standard cron numbering (0 and 7 are
/// Sunday, 1 is Monday, 6 is Saturday).
pub(crate) fn next_run_at(schedule: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let schedule = schedule.trim();
    if let Ok(seconds) = schedule.parse::<i64>() {
        if seconds > 0 {
            return Some(from + chrono::Duration::seconds(seconds));
        }
        return None;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    let mut candidate = from + chrono::Duration::minutes(1);
    candidate = candidate
        .with_second(0)
        .and_then(|moment| moment.with_nanosecond(0))
        .unwrap_or(candidate);
    for _ in 0..(8 * 24 * 60) {
        let minute = candidate.minute();
        let hour = candidate.hour();
        let day = candidate.day();
        let month = candidate.month();
        // Standard cron numbering: Sunday = 0 .. Saturday = 6 (7 is an
        // alias for Sunday, handled by `dow_matches`).
        let weekday = candidate.weekday().num_days_from_sunday();
        let dom_match = field_matches(fields[2], day);
        let dow_match = dow_matches(fields[4], weekday);
        let date_matches = if fields[2] != "*" && fields[4] != "*" {
            dom_match || dow_match
        } else {
            dom_match && dow_match
        };
        if field_matches(fields[0], minute)
            && field_matches(fields[1], hour)
            && date_matches
            && field_matches(fields[3], month)
        {
            return Some(candidate);
        }
        candidate += chrono::Duration::minutes(1);
    }
    None
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
    use crate::modules::analytics::types::{PERM_ANALYTICS_MANAGE, PERM_ANALYTICS_VIEW};

    #[test]
    fn major_to_minor_rounds_half_away_from_zero_to_minor_units() {
        assert_eq!(major_to_minor(10.005, "USD"), 1001);
        assert_eq!(major_to_minor(10.004, "USD"), 1000);
        assert_eq!(major_to_minor(0.125, "USD"), 13);
        assert_eq!(major_to_minor(-10.125, "USD"), -1013);
        assert_eq!(major_to_minor(100.0, "USD"), 10000);
    }

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(10050, "USD"), 100.5);
        assert_eq!(minor_to_major(1001, "USD"), 10.01);
        assert_eq!(minor_to_major(-1013, "USD"), -10.13);
        assert_eq!(minor_to_major(500, "JPY"), 500.0);
        assert_eq!(major_to_minor(500.0, "JPY"), 500);
    }

    #[test]
    fn major_minor_round_trip_is_exact_for_cents() {
        for minor in [0, 1, -1, 99, 100, 1025, 10050, 123456789] {
            assert_eq!(major_to_minor(minor_to_major(minor, "USD"), "USD"), minor);
        }
    }

    #[test]
    fn require_name_trims_and_enforces_limits() {
        assert_eq!(require_name("  Sales  ").unwrap(), "Sales");
        assert!(require_name("").is_err());
        assert!(require_name("   ").is_err());
        assert!(require_name(&"x".repeat(201)).is_err());
        assert!(require_name(&"x".repeat(200)).is_ok());
    }

    #[test]
    fn valid_currency_accepts_three_letter_codes() {
        assert!(valid_currency("USD"));
        assert!(valid_currency("idr"));
        assert!(!valid_currency("US"));
        assert!(!valid_currency("USDD"));
        assert!(!valid_currency("U1D"));
        assert!(!valid_currency(""));
    }

    #[test]
    fn parse_datetime_accepts_rfc3339_only() {
        let parsed = parse_datetime("2026-11-22T10:00:00Z", "from").expect("valid RFC 3339");
        assert_eq!(day_key(parsed), "2026-11-22");
        assert!(parse_datetime("2026-11-22", "from").is_err());
        assert!(parse_datetime("not a date", "from").is_err());
    }

    #[test]
    fn resolve_range_supports_presets_and_rejects_bad_input() {
        let (start, end) = resolve_range(None, None, Some("7d")).expect("7d preset");
        assert_eq!((end - start).num_days(), 7);
        let (start, end) = resolve_range(None, None, None).expect("default range");
        assert_eq!((end - start).num_days(), 30);
        assert!(resolve_range(None, None, Some("5d")).is_err());
        assert!(resolve_range(
            Some("2026-11-22T10:00:00Z"),
            Some("2026-11-21T10:00:00Z"),
            None
        )
        .is_err());
    }

    #[test]
    fn valid_schedule_accepts_intervals_and_cron_fields() {
        assert!(valid_schedule("300"));
        assert!(valid_schedule("*/5 * * * *"));
        assert!(valid_schedule("0 9 * * 1"));
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("every hour"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn next_run_at_resolves_interval_schedules_exactly() {
        let from = parse_datetime("2026-11-22T10:00:00Z", "from").unwrap();
        let next = next_run_at("300", from).expect("interval schedule");
        assert_eq!(next, from + chrono::Duration::seconds(300));
        assert!(next_run_at("0", from).is_none());
        assert!(next_run_at("nonsense", from).is_none());
    }

    #[test]
    fn valid_schedule_rejects_zero_and_negative_intervals() {
        assert!(valid_schedule("1"));
        assert!(valid_schedule("300"));
        // "0" parses as a number but next_run_at("0") is None, so a
        // schedule created with it would never run.
        assert!(!valid_schedule("0"));
        assert!(!valid_schedule("00"));
        assert!(!valid_schedule("-5"));
        assert!(next_run_at("0", Utc::now()).is_none());
    }

    #[test]
    fn valid_schedule_rejects_out_of_range_cron_fields() {
        // The reported case: every field out of range, yet previously accepted.
        assert!(!valid_schedule("99 99 99 99 99"));
        // Minute 0-59.
        assert!(valid_schedule("59 * * * *"));
        assert!(!valid_schedule("60 * * * *"));
        // Hour 0-23.
        assert!(valid_schedule("* 23 * * *"));
        assert!(!valid_schedule("* 24 * * *"));
        // Day-of-month 1-31.
        assert!(valid_schedule("* * 31 * *"));
        assert!(!valid_schedule("* * 0 * *"));
        assert!(!valid_schedule("* * 32 * *"));
        // Month 1-12.
        assert!(valid_schedule("* * * 12 *"));
        assert!(!valid_schedule("* * * 0 *"));
        assert!(!valid_schedule("* * * 13 *"));
        // Day-of-week 0-7 (7 is an alias for Sunday).
        assert!(valid_schedule("* * * * 0"));
        assert!(valid_schedule("* * * * 7"));
        assert!(!valid_schedule("* * * * 8"));
        // Comma lists are range-checked per element.
        assert!(valid_schedule("0 9 15,16 * *"));
        assert!(!valid_schedule("0 9 15,32 * *"));
        assert!(!valid_schedule("0,60 9 * * *"));
        // Steps that no value in the range could ever match are rejected.
        assert!(!valid_schedule("* * */32 * *"));
        assert!(valid_schedule("* * */31 * *"));
        assert!(!valid_schedule("* * * */13 *"));
        assert!(valid_schedule("*/5 * * * *"));
    }

    #[test]
    fn next_run_at_uses_or_when_both_day_fields_restricted() {
        // 2026-11-14 is a Saturday, 2026-11-15 a Sunday, 2026-11-16 a Monday.
        let saturday = parse_datetime("2026-11-14T10:00:00Z", "from").unwrap();
        let monday = parse_datetime("2026-11-16T10:00:00Z", "from").unwrap();
        // The 15th fires first via day-of-month.
        let next = next_run_at("0 9 15 * 1", saturday).expect("15th or Monday");
        assert_eq!(day_key(next), "2026-11-15");
        assert_eq!(next.hour(), 9);
        // After the 15th has passed, the next Monday fires: with AND this
        // would only fire on a Monday that is also the 15th (2026-12-15
        // is a Tuesday, so nothing inside the 8-day window).
        let next = next_run_at("0 9 15 * 1", monday).expect("15th or Monday");
        assert_eq!(day_key(next), "2026-11-23");
        assert_eq!(next.hour(), 9);
        // With only one day field restricted, the old AND behaviour stays:
        // day-of-month only fires on the 15th, nothing inside the window
        // from Monday the 16th.
        assert!(next_run_at("0 9 15 * *", monday).is_none());
        // Day-of-week only still fires on the next Monday.
        let next = next_run_at("0 9 * * 1", monday).expect("next Monday");
        assert_eq!(day_key(next), "2026-11-23");
    }

    #[test]
    fn next_run_at_day_of_week_uses_standard_cron_numbering() {
        // 2026-11-14 is a Saturday, 2026-11-15 a Sunday, 2026-11-22 a
        // Sunday, 2026-11-23 a Monday, 2026-11-28 a Saturday.
        let saturday = parse_datetime("2026-11-14T10:00:00Z", "from").unwrap();
        let sunday = parse_datetime("2026-11-22T10:00:00Z", "from").unwrap();
        // 0 and 7 both mean Sunday.
        for dow in ["0", "7"] {
            let schedule = format!("0 9 * * {dow}");
            let next = next_run_at(&schedule, saturday).expect("next Sunday");
            assert_eq!(day_key(next), "2026-11-15", "dow {dow}");
        }
        // 1 is Monday, not Sunday.
        let next = next_run_at("0 9 * * 1", sunday).expect("next Monday");
        assert_eq!(day_key(next), "2026-11-23");
        // 6 is Saturday.
        let next = next_run_at("0 9 * * 6", sunday).expect("next Saturday");
        assert_eq!(day_key(next), "2026-11-28");
    }

    // Caller authorization: every analytics handler gates on these,
    // so an unauthenticated or unprivileged caller is refused here
    // before any read or write runs.
    fn test_context() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn authed_request() -> HttpRequest {
        use actix_web::test::TestRequest;
        verified_request("user-1", "admin")
    }

    fn allow_all_role_checks(ctx: &AppContext) {
        use unified_core::hooks::HookContext;
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
    }

    #[test]
    fn analytics_caller_accepts_a_verified_caller() {
        let found = analytics_caller(&authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn analytics_caller_rejects_a_request_without_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default().to_http_request();
        let error = analytics_caller(&req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn analytics_caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = analytics_caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn analytics_authorize_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and both reads and
        // writes are refused with 403.
        let ctx = test_context();
        assert!(!analytics_check_capability(&ctx, "admin", PERM_ANALYTICS_VIEW));
        assert!(!analytics_check_capability(&ctx, "admin", PERM_ANALYTICS_MANAGE));
        let view = analytics_authorize(&ctx, &authed_request(), PERM_ANALYTICS_VIEW).unwrap_err();
        assert_eq!(view.code, 403);
        let manage =
            analytics_authorize(&ctx, &authed_request(), PERM_ANALYTICS_MANAGE).unwrap_err();
        assert_eq!(manage.code, 403);
    }

    #[test]
    fn analytics_authorize_rejects_a_request_without_a_verified_caller() {
        use actix_web::test::TestRequest;
        let ctx = test_context();
        allow_all_role_checks(&ctx);
        let req = TestRequest::default().to_http_request();
        let error = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn analytics_authorize_accepts_when_role_check_allows() {
        let ctx = test_context();
        allow_all_role_checks(&ctx);
        let found = analytics_authorize(&ctx, &authed_request(), PERM_ANALYTICS_VIEW).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(analytics_authorize(&ctx, &authed_request(), PERM_ANALYTICS_MANAGE).is_ok());
    }

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    fn tenant_ctx(req: &HttpRequest) -> unified_core::tenancy::TenantContext {
        unified_core::tenancy::TenantContext::from_http_request(req)
            .expect("tenant context must resolve")
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        use actix_web::test::TestRequest;
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = tenant_request("co-1", Some("co-1"));
        let tenant = tenant_ctx(&req);
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("co-1")
        );
    }

    #[test]
    fn caller_company_falls_back_to_verified_caller_company() {
        let req = tenant_request("", Some("co-1"));
        let tenant = tenant_ctx(&req);
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("co-1")
        );
    }

    #[test]
    fn caller_company_rejects_tenant_and_verified_company_mismatch() {
        let req = tenant_request("co-1", Some("co-2"));
        let tenant = tenant_ctx(&req);
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_is_fail_closed_for_unresolved_callers() {
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("co-1".to_string()), &None));
        assert!(in_company_scope(
            &Some("co-1".to_string()),
            &Some("co-1".to_string())
        ));
        assert!(!in_company_scope(
            &Some("co-1".to_string()),
            &Some("co-2".to_string())
        ));
        assert!(in_company_scope(&None, &Some("co-1".to_string())));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        assert!(
            require_company_match_for(&None, &Some("co-1".to_string())).is_err()
        );
        assert!(
            require_company_match_for(&Some("co-1".to_string()), &Some("co-1".to_string()))
                .is_ok()
        );
        assert!(
            require_company_match_for(&Some("co-1".to_string()), &Some("co-2".to_string()))
                .is_err()
        );
        assert_eq!(
            require_company_match_for(&Some("co-1".to_string()), &None).unwrap(),
            Some("co-1".to_string())
        );
        assert!(require_company_match_for(&None, &None).is_err());
    }
}
