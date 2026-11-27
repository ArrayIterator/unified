//! Caller, permission, loading, validation, audit, and CSV
//! helpers.

use super::state::EsgState;
use super::types::{
    AuditRecord, Category, Decimal, Entry, Framework, MAX_CODE_LEN, MAX_DESCRIPTION_LEN,
    MAX_DOCUMENT_REFS, MAX_FRAMEWORK_TAGS, MAX_FREQUENCY_LEN, MAX_NAME_LEN, MAX_NOTE_LEN,
    MAX_REFERENCE_LEN, MAX_SCOPE_ID_LEN, MAX_SCOPE_TYPE_LEN, MAX_UNIT_LEN, Metric, MetricDataType,
    PERM_ESG_MANAGE, PERM_ESG_VIEW, Period, PeriodStatus, Target, TargetDirection,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the sign module.
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

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

pub(crate) fn require_view(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_ESG_VIEW)
}

pub(crate) fn require_manage(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_ESG_MANAGE)
}

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
}

// Tenant edge guard (C3 pilot 5, group B).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field (entries and
/// targets name a `scope_type` / `scope_id` reference instead, which
/// this module does not resolve against the caller scope), so the
/// guard is a fail-closed gate only: per-record company
/// partitioning needs a company scope on the record (a C11
/// follow-up), not a filter here. Tenant partitioning itself is
/// already enforced by the `tenant_id` on every row plus the
/// tenant-matching loaders below.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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
    raw: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(text, field, max)?)),
        None => Ok(None),
    }
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

// Validation

/// Validate a scope reference: both parts are plain
/// reference ids (a company or a site from the company
/// module, or any other scope the tenant uses). No existence
/// check is done here - the owning module answers existence
/// through the `company.exists` hook, and this module must
/// not assume another module's tables.
pub(crate) fn validate_scope(scope_type: &str, scope_id: &str) -> UnifiedResult<(String, String)> {
    let scope_type = require_text(scope_type, "scope_type", MAX_SCOPE_TYPE_LEN)?;
    let scope_id = require_text(scope_id, "scope_id", MAX_SCOPE_ID_LEN)?;
    Ok((scope_type, scope_id))
}

/// Validate document references: plain ids into the documents
/// module, at most the module bound, no repeats, no empties.
pub(crate) fn validate_document_ids(ids: &[String]) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_DOCUMENT_REFS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_DOCUMENT_REFS} document references per entry"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut clean = Vec::new();
    for id in ids {
        let id = require_text(id, "document_id", MAX_REFERENCE_LEN)?;
        if !seen.insert(id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "document_ids must not repeat \"{id}\""
            )));
        }
        clean.push(id);
    }
    Ok(clean)
}

/// Validate framework tags on a metric: plain framework ids,
/// at most the module bound, no repeats, no empties. The ids
/// need not exist yet - a metric may tag a framework that is
/// created later, and deleting a framework never deletes the
/// metrics that tagged it.
pub(crate) fn validate_framework_ids(ids: &[String]) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_FRAMEWORK_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_FRAMEWORK_TAGS} framework tags per metric"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut clean = Vec::new();
    for id in ids {
        let id = require_text(id, "framework_id", MAX_REFERENCE_LEN)?;
        if !seen.insert(id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "framework_ids must not repeat \"{id}\""
            )));
        }
        clean.push(id);
    }
    Ok(clean)
}

/// Validate a framework name plus optional description and
/// version.
pub(crate) fn validate_framework_input(
    name: &str,
    description: &Option<String>,
    version: &Option<String>,
) -> UnifiedResult<(String, Option<String>, Option<String>)> {
    let name = require_text(name, "name", MAX_NAME_LEN)?;
    let description = optional_text(description, "description", MAX_DESCRIPTION_LEN)?;
    let version = optional_text(version, "version", MAX_CODE_LEN)?;
    Ok((name, description, version))
}

/// Validate a metric definition: name, category, unit, data
/// type, reporting frequency, and framework tags.
pub(crate) fn validate_metric_input(
    name: &str,
    description: &Option<String>,
    category: &str,
    unit: &str,
    data_type: &str,
    frequency: &str,
    framework_ids: &[String],
) -> UnifiedResult<(
    String,
    Option<String>,
    Category,
    String,
    MetricDataType,
    String,
    Vec<String>,
)> {
    let name = require_text(name, "name", MAX_NAME_LEN)?;
    let description = optional_text(description, "description", MAX_DESCRIPTION_LEN)?;
    let category = Category::from_str(category.trim()).ok_or_else(|| {
        UnifiedError::bad_request(
            "category must be environmental, social, or governance".to_string(),
        )
    })?;
    let unit = require_text(unit, "unit", MAX_UNIT_LEN)?;
    let data_type = MetricDataType::from_str(data_type.trim()).ok_or_else(|| {
        UnifiedError::bad_request("data_type must be number, text, or boolean".to_string())
    })?;
    let frequency = require_text(frequency, "frequency", MAX_FREQUENCY_LEN)?;
    let framework_ids = validate_framework_ids(framework_ids)?;
    Ok((
        name,
        description,
        category,
        unit,
        data_type,
        frequency,
        framework_ids,
    ))
}

/// Validate a period definition: code plus a valid date
/// range.
pub(crate) fn validate_period_input(
    code: &str,
    name: &Option<String>,
    starts_on: &str,
    ends_on: &str,
) -> UnifiedResult<(String, Option<String>, NaiveDate, NaiveDate)> {
    let code = require_text(code, "code", MAX_CODE_LEN)?;
    let name = optional_text(name, "name", MAX_NAME_LEN)?;
    let (start, end) = super::types::validate_period_range(starts_on, ends_on)
        .map_err(UnifiedError::bad_request)?;
    Ok((code, name, start, end))
}

/// Normalize a raw entry / target value against the metric's
/// data type: numbers become their canonical decimal string,
/// booleans become `true` / `false`, text is stored as
/// written (trimmed). Rejects values that do not match the
/// metric's type.
pub(crate) fn normalize_value(metric: &Metric, raw: &str, field: &str) -> UnifiedResult<String> {
    let text = raw.trim();
    match metric.data_type {
        MetricDataType::Number => {
            if text.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must not be empty"
                )));
            }
            Decimal::parse(text)
                .map(|value| value.to_decimal_string())
                .ok_or_else(|| {
                    UnifiedError::bad_request(format!("{field} must be a number: {raw}"))
                })
        }
        MetricDataType::Boolean => {
            if text.eq_ignore_ascii_case("true") {
                Ok("true".to_string())
            } else if text.eq_ignore_ascii_case("false") {
                Ok("false".to_string())
            } else {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be true or false: {raw}"
                )))
            }
        }
        MetricDataType::Text => {
            if text.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must not be empty"
                )));
            }
            if text.len() > MAX_NOTE_LEN {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {MAX_NOTE_LEN} characters"
                )));
            }
            Ok(text.to_string())
        }
    }
}

/// Validate a target direction string.
pub(crate) fn validate_direction(raw: &str) -> UnifiedResult<TargetDirection> {
    TargetDirection::from_str(raw.trim()).ok_or_else(|| {
        UnifiedError::bad_request("direction must be increase or decrease".to_string())
    })
}

/// Validate a target deadline: a valid `YYYY-MM-DD` date.
pub(crate) fn validate_deadline(raw: &str) -> UnifiedResult<NaiveDate> {
    if !super::types::valid_date(raw) {
        return Err(UnifiedError::bad_request(format!(
            "deadline must be a YYYY-MM-DD date: {raw}"
        )));
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("deadline must be a YYYY-MM-DD date: {raw}"))
    })
}

// Tenant-scoped loaders (cross-tenant reads as 404, never 403)

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

pub(crate) fn load_framework(
    state: &EsgState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Framework> {
    let id = parse_uuid(raw_id, "framework")?;
    let framework = state
        .frameworks
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Framework not found: {id}")))?;
    tenant_match(&framework.tenant_id, tenant_id, id, "Framework")?;
    Ok(framework)
}

pub(crate) fn load_metric(
    state: &EsgState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Metric> {
    let id = parse_uuid(raw_id, "metric")?;
    let metric = state
        .metrics
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Metric not found: {id}")))?;
    tenant_match(&metric.tenant_id, tenant_id, id, "Metric")?;
    Ok(metric)
}

pub(crate) fn load_period(
    state: &EsgState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Period> {
    let id = parse_uuid(raw_id, "period")?;
    let period = state
        .periods
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Period not found: {id}")))?;
    tenant_match(&period.tenant_id, tenant_id, id, "Period")?;
    Ok(period)
}

pub(crate) fn load_entry(state: &EsgState, tenant_id: &str, raw_id: &str) -> UnifiedResult<Entry> {
    let id = parse_uuid(raw_id, "entry")?;
    let entry = state
        .entries
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Entry not found: {id}")))?;
    tenant_match(&entry.tenant_id, tenant_id, id, "Entry")?;
    Ok(entry)
}

pub(crate) fn load_target(
    state: &EsgState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Target> {
    let id = parse_uuid(raw_id, "target")?;
    let target = state
        .targets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Target not found: {id}")))?;
    tenant_match(&target.tenant_id, tenant_id, id, "Target")?;
    Ok(target)
}

/// A period must be open for entries and targets to be
/// written. Closing is the tenant's explicit freeze.
pub(crate) fn require_period_open(period: &Period) -> UnifiedResult<()> {
    if period.status != PeriodStatus::Open {
        return Err(UnifiedError::bad_request(format!(
            "Period {} is closed; entries and targets cannot be changed",
            period.code
        )));
    }
    Ok(())
}

// Audit

/// Append one audit row for a change. `before` / `after` are
/// the serialized rows (or `None` for creates / deletes).
pub(crate) fn record_audit(
    state: &EsgState,
    tenant_id: &str,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    actor: &str,
    before: Option<Value>,
    after: Option<Value>,
) {
    let row = AuditRecord {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        entity_type: entity_type.to_string(),
        entity_id: entity_id.to_string(),
        action: action.to_string(),
        actor: actor.to_string(),
        before,
        after,
        created_at: Utc::now(),
    };
    state.audit.insert(row.id, row);
}

// CSV (pure functions, unit-tested below)

/// Quote one CSV field: quote when it contains a comma, a
/// quote, or a newline; double any quotes inside.
pub fn csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Render rows of fields as CSV text (CRLF line endings).
pub fn render_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let fields: Vec<String> = row.iter().map(|field| csv_field(field)).collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pagination_params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn paginate_bounds() {
        let items: Vec<i64> = (1..=5).collect();
        let first = paginate(items.clone(), &pagination_params(Some(1), Some(2)));
        assert_eq!(first.items, vec![1, 2]);
        assert_eq!(first.pagination.total, 5);
        let past_end = paginate(items.clone(), &pagination_params(Some(4), Some(2)));
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.pagination.total, 5);
    }

    #[test]
    fn value_normalization_by_type() {
        let now = Utc::now();
        let mut metric = Metric {
            id: Uuid::new_v4(),
            tenant_id: "tenant-a".to_string(),
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
        assert_eq!(normalize_value(&metric, "12.50", "value").unwrap(), "12.5");
        assert!(normalize_value(&metric, "abc", "value").is_err());
        assert!(normalize_value(&metric, "", "value").is_err());
        metric.data_type = MetricDataType::Boolean;
        assert_eq!(normalize_value(&metric, "TRUE", "value").unwrap(), "true");
        assert_eq!(normalize_value(&metric, "false", "value").unwrap(), "false");
        assert!(normalize_value(&metric, "yes", "value").is_err());
        metric.data_type = MetricDataType::Text;
        assert_eq!(
            normalize_value(&metric, "  on track ", "value").unwrap(),
            "on track"
        );
        assert!(normalize_value(&metric, "   ", "value").is_err());
    }

    #[test]
    fn scope_and_reference_validation() {
        assert!(validate_scope("company", "company-1").is_ok());
        assert!(validate_scope("", "company-1").is_err());
        assert!(validate_scope("site", "").is_err());
        assert!(validate_document_ids(&["doc-1".to_string(), "doc-2".to_string()]).is_ok());
        assert!(validate_document_ids(&["doc-1".to_string(), "doc-1".to_string()]).is_err());
        assert!(validate_document_ids(&["".to_string()]).is_err());
        assert!(validate_framework_ids(&["fw-1".to_string()]).is_ok());
        assert!(validate_framework_ids(&["fw-1".to_string(), "fw-1".to_string()]).is_err());
        assert!(validate_direction("increase").is_ok());
        assert!(validate_direction("decrease").is_ok());
        assert!(validate_direction("maintain").is_err());
        assert!(validate_deadline("2026-12-31").is_ok());
        assert!(validate_deadline("2026-02-30").is_err());
        assert!(validate_deadline("31-12-2026").is_err());
    }

    #[test]
    fn csv_renders_with_quoting() {
        let rows = vec![
            vec!["a".to_string(), "x,y".to_string()],
            vec!["say \"hi\"".to_string(), "plain".to_string()],
        ];
        let text = render_csv(&rows);
        assert_eq!(text, "a,\"x,y\"\r\n\"say \"\"hi\"\"\",plain\r\n");
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: uuid::Uuid::now_v7().to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
