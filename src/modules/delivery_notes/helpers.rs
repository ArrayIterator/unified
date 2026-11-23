//! Caller, permission, loading, and view helpers.

use super::state::DeliveryNoteState;
use super::types::{
    DeliveryNote, DeliveryNoteItem, MAX_ADDRESS_LEN, MAX_LABEL_LEN, MAX_NOTES_LEN,
    MAX_PAPER_SIZE_LEN, MAX_REASON_LEN, MAX_REFERENCE_LEN, MAX_TEMPLATE_ID_LEN, MAX_UNIT_LEN,
    PERM_DELIVERY_NOTES_MANAGE,
};
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::{json, Value};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the phone and sign modules.
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

/// True when the caller holds the manage capability.
pub(crate) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_DELIVERY_NOTES_MANAGE)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group A).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
/// Handlers derive the tenant id from the returned context, so each
/// request resolves its tenant exactly once.
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
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// The caller company scope as a UUID, for records keyed by
/// `company_id: Option<Uuid>`. A scope string that is not a UUID
/// parses to nothing: it can never equal a stored company id.
fn scope_uuid(scope: &Option<String>) -> Option<Uuid> {
    scope
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying exactly that company match; unscoped (legacy) records
/// are hidden from scoped callers. A scope that is not a UUID
/// matches nothing at all — not even global rows — because a
/// caller carrying a company scope is not an unresolved caller.
pub(crate) fn in_company_scope_uuid(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(_) => match scope_uuid(caller_company) {
            Some(id) => record_company_id == &Some(id),
            None => false,
        },
    }
}

/// Fail closed (404) when a stored delivery note is outside the
/// caller's company scope. Single-note reads and mutations call
/// this right after `load_note`; list handlers use
/// `in_company_scope_uuid` as a filter instead. The failure is a
/// 404, not a 403, matching the module's tenant loaders: a note
/// from another company reads as missing, so the check never
/// leaks whether such a note exists.
pub(crate) fn require_note_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope_uuid(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Delivery note not found"))
    }
}

/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row. A caller scope that is not a UUID cannot be
/// stored on a note, so omitting the company is refused for such
/// callers as well.
pub(crate) fn require_company_match_uuid(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_uuid_for(&caller, body_company_id)
}

/// `require_company_match_uuid` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_uuid_for(
    caller: &Option<String>,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match (body_company_id, caller) {
        (Some(body), Some(scope)) => match Uuid::parse_str(scope) {
            Ok(scope_id) if *body == scope_id => Ok(Some(*body)),
            _ => Err(UnifiedError::forbidden(
                "company_id does not match the caller's company scope",
            )),
        },
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(scope)) => match Uuid::parse_str(scope) {
            Ok(scope_id) => Ok(Some(scope_id)),
            Err(_) => Err(UnifiedError::forbidden(
                "The caller company scope is not a UUID, so it cannot be stored on a delivery note",
            )),
        },
        (None, None) => Ok(None),
    }
}

/// Verify a newly named company on an update body
/// (`Option<Uuid>` inside the outer `Option`): `Some(id)` must
/// equal the caller scope (403 otherwise; an unresolved caller
/// may not name a company); `None` is an explicit clear and stays
/// a clear, never re-stamped with the caller scope. The outer
/// `None` (field absent) is handled by the caller leaving the
/// stored value untouched.
pub(crate) fn require_company_change_uuid(
    scope: &Option<String>,
    new: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match (new, scope) {
        (Some(id), Some(scope)) => match Uuid::parse_str(scope) {
            Ok(scope_id) if *id == scope_id => Ok(Some(*id)),
            _ => Err(UnifiedError::forbidden(
                "company_id does not match the caller's company scope",
            )),
        },
        (Some(_), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, _) => Ok(None),
    }
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

pub(crate) fn require_label(value: &str) -> UnifiedResult<String> {
    require_text(value, "label", MAX_LABEL_LEN)
}

pub(crate) fn require_address(value: &str) -> UnifiedResult<String> {
    require_text(value, "address", MAX_ADDRESS_LEN)
}

pub(crate) fn require_notes(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "notes", MAX_NOTES_LEN)
}

pub(crate) fn require_reason(value: &str) -> UnifiedResult<String> {
    require_text(value, "reason", MAX_REASON_LEN)
}

pub(crate) fn require_unit(value: &str) -> UnifiedResult<String> {
    require_text(value, "unit", MAX_UNIT_LEN)
}

pub(crate) fn require_reference(value: &str, field: &str) -> UnifiedResult<String> {
    require_text(value, field, MAX_REFERENCE_LEN)
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

// ---------------------------------------------------------------------------
// Tenant-scoped loaders
// ---------------------------------------------------------------------------
//
// Every loader checks the row's `tenant_id` against the caller's
// tenant. A row from another tenant reads as 404, never 403, so
// its existence does not leak across tenants.

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

pub(crate) fn load_note(
    state: &DeliveryNoteState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<DeliveryNote> {
    let id = parse_uuid(raw_id, "delivery note")?;
    let note = state
        .notes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery note not found: {id}")))?;
    tenant_match(&note.tenant_id, tenant_id, id, "Delivery note")?;
    Ok(note)
}

pub(crate) fn load_item(
    state: &DeliveryNoteState,
    tenant_id: &str,
    note_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<DeliveryNoteItem> {
    let id = parse_uuid(raw_id, "delivery note item")?;
    let item = state
        .items
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery note item not found: {id}")))?;
    if item.delivery_note_id != note_id {
        return Err(UnifiedError::not_found(format!(
            "Delivery note item not found: {id}"
        )));
    }
    tenant_match(&item.tenant_id, tenant_id, id, "Delivery note item")?;
    Ok(item)
}

/// Parse an optional date filter (`YYYY-MM-DD` or RFC 3339) into
/// a day window start. Returns `None` when the filter is absent;
/// an invalid value is a 400.
pub(crate) fn parse_date_filter(
    raw: &str,
    field: &str,
) -> UnifiedResult<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    let trimmed = raw.trim();
    if trimmed.len() == 10 {
        let date = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map_err(|_| {
            UnifiedError::bad_request(format!(
                "{field} must be a date (YYYY-MM-DD) or RFC 3339 timestamp"
            ))
        })?;
        return Ok(utc_from_naive(date));
    }
    chrono::DateTime::parse_from_rfc3339(trimmed)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!(
                "{field} must be a date (YYYY-MM-DD) or RFC 3339 timestamp"
            ))
        })
}

fn utc_from_naive(date: chrono::NaiveDate) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap_or_default())
        .single()
        .unwrap_or_else(chrono::Utc::now)
}

// ---------------------------------------------------------------------------
// Views and events
// ---------------------------------------------------------------------------

/// DO view with its items and quantity totals attached, so a
/// single read shows the whole document.
pub(crate) fn note_view(state: &DeliveryNoteState, note: &DeliveryNote) -> Value {
    let items = state.note_items(note.id);
    let mut value = serde_json::to_value(note).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "items".to_string(),
            serde_json::to_value(&items).unwrap_or(Value::Null),
        );
        map.insert(
            "total_ordered".to_string(),
            Value::from(super::types::total_ordered(&items)),
        );
        map.insert(
            "total_shipped".to_string(),
            Value::from(super::types::total_shipped(&items)),
        );
        map.insert(
            "total_returned".to_string(),
            Value::from(super::types::total_returned(&items)),
        );
    }
    value
}

/// Emit a DO event on the hook bus with the standard payload.
pub(crate) fn note_event(
    state: &DeliveryNoteState,
    event: &str,
    note: &DeliveryNote,
    actor_id: &str,
    detail: Option<String>,
) {
    state.emit(
        event,
        json!({
            "tenant_id": note.tenant_id,
            "delivery_note_id": note.id.to_string(),
            "number": note.number,
            "shipment_id": note.shipment_id.map(|id| id.to_string()),
            "tracking_number": note.tracking_number,
            "rental_order_id": note.rental_order_id,
            "order_id": note.order_id,
            "company_id": note.company_id.map(|id| id.to_string()),
            "status": note.status.as_str(),
            "receiver_name": note.receiver.name,
            "actor_id": actor_id,
            "detail": detail,
        }),
    );
}

/// Validate an output kind against the printing module's
/// vocabulary. Unknown kinds are rejected instead of queued
/// into a job the printer cannot render.
pub(crate) fn validate_output_kind(raw: &str) -> UnifiedResult<String> {
    let kind = raw.trim().to_lowercase();
    match kind.as_str() {
        "paper" | "pdf" | "thermal" | "email" => Ok(kind),
        _ => Err(UnifiedError::bad_request(
            "output_kind must be paper, pdf, thermal, or email",
        )),
    }
}

/// Validate a paper size code: a short slug, never empty.
pub(crate) fn validate_paper_size(raw: &str) -> UnifiedResult<String> {
    require_text(raw, "paper_size_code", MAX_PAPER_SIZE_LEN)
}

/// Validate a template id: a short reference, never empty.
pub(crate) fn validate_template_id(raw: &str) -> UnifiedResult<String> {
    require_text(raw, "template_id", MAX_TEMPLATE_ID_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_kind_validation() {
        assert_eq!(validate_output_kind("paper").unwrap(), "paper");
        assert_eq!(validate_output_kind("PDF").unwrap(), "pdf");
        assert_eq!(validate_output_kind("thermal").unwrap(), "thermal");
        assert_eq!(validate_output_kind("email").unwrap(), "email");
        assert!(validate_output_kind("fax").is_err());
        assert!(validate_output_kind("").is_err());
    }

    #[test]
    fn date_filter_parsing() {
        let day = parse_date_filter("2026-11-22", "from").unwrap();
        assert_eq!(day.format("%Y-%m-%d").to_string(), "2026-11-22");
        let stamp = parse_date_filter("2026-11-22T10:00:00Z", "from").unwrap();
        assert_eq!(
            stamp.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-11-22T10:00:00"
        );
        assert!(parse_date_filter("22/11/2026", "from").is_err());
        assert!(parse_date_filter("", "from").is_err());
    }

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
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
        let mut caller = unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_uuid_matches_only_the_same_company_uuid() {
        let company = Uuid::now_v7();
        let scope = Some(company.to_string());
        assert!(in_company_scope_uuid(&Some(company), &scope));
        assert!(!in_company_scope_uuid(&Some(Uuid::now_v7()), &scope));
        assert!(!in_company_scope_uuid(&None, &scope));
        assert!(in_company_scope_uuid(&None, &None));
        assert!(!in_company_scope_uuid(&Some(company), &None));
        // A non-UUID scope matches nothing, not even global rows.
        assert!(!in_company_scope_uuid(&None, &Some("acme".to_string())));
    }

    #[test]
    fn require_note_in_company_scope_is_a_404_not_a_403() {
        let company = Uuid::now_v7();
        let scope = Some(company.to_string());
        assert!(require_note_in_company_scope(&Some(Uuid::now_v7()), &scope).is_err());
        let error = require_note_in_company_scope(&Some(Uuid::now_v7()), &scope).unwrap_err();
        assert_eq!(error.code, 404);
        assert!(require_note_in_company_scope(&Some(company), &scope).is_ok());
    }

    #[test]
    fn require_company_match_uuid_for_refuses_body_company_from_unresolved_callers() {
        let company = Uuid::now_v7();
        assert!(require_company_match_uuid_for(&None, &Some(company)).is_err());
        assert!(
            require_company_match_uuid_for(&None, &None)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            require_company_match_uuid_for(&Some(company.to_string()), &Some(company)).unwrap(),
            Some(company)
        );
        assert!(
            require_company_match_uuid_for(&Some(Uuid::now_v7().to_string()), &Some(company))
                .is_err()
        );
    }

    #[test]
    fn require_company_change_uuid_keeps_an_explicit_clear_a_clear() {
        let company = Uuid::now_v7();
        let scope = Some(company.to_string());
        assert_eq!(require_company_change_uuid(&scope, &None).unwrap(), None);
        assert_eq!(
            require_company_change_uuid(&scope, &Some(company)).unwrap(),
            Some(company)
        );
        assert!(require_company_change_uuid(&scope, &Some(Uuid::now_v7())).is_err());
        assert!(require_company_change_uuid(&None, &Some(company)).is_err());
    }
}
