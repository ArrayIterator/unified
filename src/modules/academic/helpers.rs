//! Academic - caller checks, validation and pagination.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::constants::MAX_CODE;
use super::constants::MAX_SCORE;
use super::constants::PERM_ACADEMIC_ADVISE;
use super::constants::PERM_ACADEMIC_GRADE;
use super::constants::PERM_ACADEMIC_MANAGE;
use super::constants::PERM_ACADEMIC_READ_ALL;
use super::state::AcademicState;
use super::types::Enrollment;
use super::types::Student;

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// Tenant edge guard (C3 pilot 4).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
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
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) - the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
}

/// True when the caller holds any staff capability for academic data.
pub(crate) fn is_staff(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_ACADEMIC_READ_ALL)
        || check_capability(ctx, role, PERM_ACADEMIC_MANAGE)
        || check_capability(ctx, role, PERM_ACADEMIC_ADVISE)
}

/// True when `caller` may read `student`'s records: staff, the
/// student themself, or the student's assigned advisor.
pub(crate) fn can_read_student(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    student: &Student,
) -> bool {
    if check_capability(ctx, &caller.role, PERM_ACADEMIC_READ_ALL) {
        return true;
    }
    if student.user_id == caller.actor {
        return true;
    }
    if let Some(advisor_id) = &student.advisor_id {
        if let Some(lecturer) = state.lecturer_by_user(&caller.actor) {
            if lecturer.id == *advisor_id {
                return true;
            }
        }
    }
    false
}

/// Enforce read access for one student record, hiding the not-found
/// case: a caller who may not read the record gets 404, so student
/// ids cannot be probed.
pub(crate) fn require_read_student(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    student: &Student,
) -> UnifiedResult<()> {
    if can_read_student(state, ctx, caller, student) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Student not found"))
    }
}

pub(crate) fn load_student_for_read(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    student_id: &str,
) -> UnifiedResult<Student> {
    let student = state
        .students
        .get(student_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
    require_read_student(state, ctx, caller, &student)?;
    Ok(student)
}

/// True when `caller` teaches `subject` (through an active schedule
/// for the subject).
pub(crate) fn teaches_subject(state: &AcademicState, caller: &Caller, subject_id: &str) -> bool {
    let Some(lecturer) = state.lecturer_by_user(&caller.actor) else {
        return false;
    };
    state.schedules.iter().any(|entry| {
        let schedule = entry.value();
        schedule.subject_id == subject_id
            && schedule.lecturer_id == lecturer.id
            && schedule.status == "active"
    })
}

/// True when `caller` may grade `enrollment`: staff with the grade
/// capability who teaches the subject, or a manager.
pub(crate) fn can_grade(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    enrollment: &Enrollment,
) -> bool {
    if check_capability(ctx, &caller.role, PERM_ACADEMIC_MANAGE) {
        return true;
    }
    check_capability(ctx, &caller.role, PERM_ACADEMIC_GRADE)
        && teaches_subject(state, caller, &enrollment.subject_id)
}

/// True when `caller` may approve `enrollment`: a manager, or the
/// student's advisor holding the advise capability.
pub(crate) fn can_approve_enrollment(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    enrollment: &Enrollment,
) -> bool {
    if check_capability(ctx, &caller.role, PERM_ACADEMIC_MANAGE) {
        return true;
    }
    if !check_capability(ctx, &caller.role, PERM_ACADEMIC_ADVISE) {
        return false;
    }
    let Some(student) = state.students.get(&enrollment.student_id) else {
        return false;
    };
    let Some(advisor_id) = &student.advisor_id else {
        return false;
    };
    state
        .lecturer_by_user(&caller.actor)
        .is_some_and(|lecturer| lecturer.id == *advisor_id)
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn require_code(value: &str, field: &str) -> UnifiedResult<String> {
    let code = require_text(value, field, MAX_CODE)?;
    if !code
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(UnifiedError::bad_request(format!(
            "{field} must use only letters, digits, '-', '_' and '.'"
        )));
    }
    Ok(code)
}

pub(crate) fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

pub(crate) fn parse_optional_date(
    raw: &Option<String>,
    field: &str,
) -> UnifiedResult<Option<NaiveDate>> {
    match raw {
        None => Ok(None),
        Some(value) => Ok(Some(parse_date(value, field)?)),
    }
}

/// Parse `HH:MM` (24-hour clock) into minutes since midnight.
pub(crate) fn time_to_minutes(raw: &str) -> Option<i32> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let hour: i32 = hour.parse().ok()?;
    let minute: i32 = minute.parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    Some(hour * 60 + minute)
}

pub(crate) fn parse_time(raw: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = raw.trim();
    if time_to_minutes(trimmed).is_none() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a time (HH:MM, 24-hour clock)"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn parse_credits(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 || value > 60.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be greater than 0 and at most 60 credits"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

pub(crate) fn parse_weight(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 || value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be greater than 0 and at most 100"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

pub(crate) fn parse_score(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > MAX_SCORE {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and {MAX_SCORE}"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

pub(crate) fn parse_year(value: i32, field: &str) -> UnifiedResult<i32> {
    if !(1990..=2100).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a year between 1990 and 2100"
        )));
    }
    Ok(value)
}

pub(crate) fn parse_day_of_week(value: i32) -> UnifiedResult<i32> {
    if !(1..=7).contains(&value) {
        return Err(UnifiedError::bad_request(
            "day_of_week must be 1 (Monday) to 7 (Sunday)",
        ));
    }
    Ok(value)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

pub(crate) fn filter_matches(params: &PaginationParams, key: &str, value: &str) -> bool {
    params
        .filters
        .get(key)
        .is_none_or(|expected| expected == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: Default::default(),
        }
    }

    #[test]
    fn require_text_trims_and_rejects_empty_or_long() {
        assert_eq!(require_text("  hello  ", "title", 10).unwrap(), "hello");
        assert!(require_text("   ", "title", 10).is_err());
        assert!(require_text("", "title", 10).is_err());
        assert!(require_text("toolong", "title", 3).is_err());
    }

    #[test]
    fn require_code_allows_codes_and_rejects_punctuation() {
        assert_eq!(require_code("CS-101", "code",).unwrap(), "CS-101");
        assert_eq!(require_code("a.b_c", "code").unwrap(), "a.b_c");
        assert!(require_code("bad code", "code").is_err());
        assert!(require_code("a/b", "code").is_err());
        assert!(require_code("", "code").is_err());
    }

    #[test]
    fn optional_text_passes_none_through() {
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some(" x ".to_string()), "note", 10).unwrap(),
            Some("x".to_string())
        );
        assert!(optional_text(&Some(String::new()), "note", 10).is_err());
    }

    #[test]
    fn parse_date_accepts_iso_dates_only() {
        assert_eq!(
            parse_date("2026-11-22", "date").unwrap().to_string(),
            "2026-11-22"
        );
        assert!(parse_date("22/11/2026", "date").is_err());
        assert!(parse_date("2026-02-30", "date").is_err());
        assert!(parse_date("2026-13-01", "date").is_err());
    }

    #[test]
    fn time_to_minutes_parses_24_hour_clock() {
        assert_eq!(time_to_minutes("09:30"), Some(570));
        assert_eq!(time_to_minutes("00:00"), Some(0));
        assert_eq!(time_to_minutes("23:59"), Some(1439));
        assert_eq!(time_to_minutes("9:05"), Some(545));
        assert_eq!(time_to_minutes("24:00"), None);
        assert_eq!(time_to_minutes("12:60"), None);
        assert_eq!(time_to_minutes("nope"), None);
    }

    #[test]
    fn parse_time_accepts_valid_times_only() {
        assert_eq!(parse_time("09:30", "start").unwrap(), "09:30");
        assert!(parse_time("25:00", "start").is_err());
        assert!(parse_time("9:30am", "start").is_err());
    }

    #[test]
    fn numeric_parsers_enforce_ranges() {
        assert_eq!(parse_credits(3.0, "credits").unwrap(), 3.0);
        assert!(parse_credits(0.0, "credits").is_err());
        assert!(parse_credits(61.0, "credits").is_err());
        assert!(parse_credits(f64::NAN, "credits").is_err());
        assert_eq!(parse_weight(50.0, "weight").unwrap(), 50.0);
        assert!(parse_weight(0.0, "weight").is_err());
        assert!(parse_weight(100.01, "weight").is_err());
        assert_eq!(parse_score(100.0, "score").unwrap(), 100.0);
        assert!(parse_score(100.01, "score").is_err());
        assert!(parse_score(-1.0, "score").is_err());
        assert_eq!(parse_year(2026, "year").unwrap(), 2026);
        assert!(parse_year(1989, "year").is_err());
        assert!(parse_year(2101, "year").is_err());
        assert_eq!(parse_day_of_week(1).unwrap(), 1);
        assert_eq!(parse_day_of_week(7).unwrap(), 7);
        assert!(parse_day_of_week(0).is_err());
        assert!(parse_day_of_week(8).is_err());
    }

    #[test]
    fn parse_credits_and_parse_weight_errors_state_the_actual_bounds() {
        // Both parsers reject 0, so the message must not claim 0 is
        // allowed ("between 0 and ...").
        let credits_zero = parse_credits(0.0, "credits").unwrap_err().to_string();
        assert!(
            credits_zero.contains("greater than 0 and at most 60"),
            "unexpected credits error: {credits_zero}"
        );
        assert!(!credits_zero.contains("between 0 and 60"));
        let credits_over = parse_credits(61.0, "credits").unwrap_err().to_string();
        assert!(credits_over.contains("greater than 0 and at most 60"));
        let weight_zero = parse_weight(0.0, "weight").unwrap_err().to_string();
        assert!(
            weight_zero.contains("greater than 0 and at most 100"),
            "unexpected weight error: {weight_zero}"
        );
        assert!(!weight_zero.contains("between 0 and 100"));
        let weight_over = parse_weight(100.01, "weight").unwrap_err().to_string();
        assert!(weight_over.contains("greater than 0 and at most 100"));
    }

    #[test]
    fn paginate_slices_items_and_reports_total() {
        let result = paginate(vec![1, 2, 3, 4, 5], &params(Some(1), Some(2)));
        assert_eq!(result.items, vec![1, 2]);
        assert_eq!(result.pagination.total, 5);
        let result = paginate(vec![1, 2, 3, 4, 5], &params(Some(3), Some(2)));
        assert_eq!(result.items, vec![5]);
        let result = paginate(vec![1, 2, 3, 4, 5], &params(Some(9), Some(2)));
        assert!(result.items.is_empty());
    }

    #[test]
    fn search_and_filter_matching() {
        let mut with_search = params(None, None);
        with_search.search = Some("world".to_string());
        assert!(matches_search("Hello World", &with_search));
        assert!(!matches_search("Hello there", &with_search));
        assert!(matches_search("anything", &params(None, None)));
        let mut filters = std::collections::HashMap::new();
        filters.insert("status".to_string(), "active".to_string());
        let mut with_filter = params(None, None);
        with_filter.filters = filters;
        assert!(filter_matches(&with_filter, "status", "active"));
        assert!(!filter_matches(&with_filter, "status", "draft"));
        assert!(filter_matches(&with_filter, "other", "whatever"));
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
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
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
