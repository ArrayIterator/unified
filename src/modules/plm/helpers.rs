//! Caller, permission, loading, and validation helpers.

use super::state::PlmState;
use super::types::{
    ApprovalDecision, EcoStatus, PlmApproval, PlmBomVersion, PlmEco, PlmStage, PlmVersion,
    MAX_COMMENT_LEN, MAX_DESCRIPTION_LEN, MAX_IDS, MAX_NAME_LEN, MAX_SLUG_LEN, MAX_STEPS,
    PERM_PLM_MANAGE,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the rental and forums modules.
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

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, task 319).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// The records in this module carry a required `tenant_id` (not a
/// `company_id`): partitioning is by tenant, through the pre-existing
/// `load_*` helpers (cross-tenant row reads as 404) and the list
/// filters. `caller_company` below is therefore the mismatch-only
/// company check (pilot 4 group D idiom), not a strict
/// `require_company` gate: a tenant/caller company mismatch is 403,
/// but a write is never refused only because no company resolves —
/// the write is still fully tenant-partitioned by `tenant_id`.
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
/// middleware-resolved tenant context are sources (header trust was
/// removed by task 317; this helper must never reintroduce it).
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
pub(crate) fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn require_slug(value: &str, field: &str) -> UnifiedResult<String> {
    let slug = value.trim().to_lowercase();
    if !valid_slug(&slug) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)"
        )));
    }
    Ok(slug)
}

pub(crate) fn validate_name(name: &str) -> UnifiedResult<String> {
    require_text(name, "name", MAX_NAME_LEN)
}

pub(crate) fn validate_description(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "description", MAX_DESCRIPTION_LEN)
}

pub(crate) fn validate_comment(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "comment", MAX_COMMENT_LEN)
}

/// Validate a list of reference ids: at most 100 entries, each
/// non-empty and at most 200 characters.
pub(crate) fn validate_id_list(ids: &[String], field: &str) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_IDS {
        return Err(UnifiedError::bad_request(format!(
            "{field} must have at most {MAX_IDS} entries"
        )));
    }
    let mut out = Vec::with_capacity(ids.len());
    for raw in ids {
        out.push(require_text(raw, field, 200)?);
    }
    Ok(out)
}

pub(crate) fn validate_uuid_list(ids: &[Uuid]) -> UnifiedResult<Vec<Uuid>> {
    if ids.len() > MAX_IDS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_IDS} entries"
        )));
    }
    Ok(ids.to_vec())
}

/// Validate an ECO impact level against the documented data
/// values. `None` (no assessment yet) is allowed.
pub(crate) fn validate_impact_level(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        None => Ok(None),
        Some(level) => {
            let level = level.trim().to_lowercase();
            if crate::modules::plm::types::IMPACT_LEVELS.contains(&level.as_str()) {
                Ok(Some(level))
            } else {
                Err(UnifiedError::bad_request(format!(
                    "impact_level must be one of low, medium, high, critical, got \"{level}\""
                )))
            }
        }
    }
}

/// Validate approval steps supplied as data: at most 20 steps,
/// each with a non-empty name and approver role slug.
pub(crate) fn validate_approval_steps(
    steps: &[crate::modules::plm::types::ApprovalStep],
) -> UnifiedResult<Vec<crate::modules::plm::types::ApprovalStep>> {
    if steps.len() > MAX_STEPS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_STEPS} approval steps"
        )));
    }
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let name = require_text(&step.name, "approval step name", MAX_NAME_LEN)?;
        let approver_role = require_slug(&step.approver_role, "approver_role")?;
        out.push(crate::modules::plm::types::ApprovalStep {
            name,
            approver_role,
            required: step.required,
        });
    }
    Ok(out)
}

/// Validate an effective date range: `effective_to`, when set,
/// must be after `effective_from`.
pub(crate) fn validate_effective_range(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let (Some(from), Some(to)) = (from, to) {
        if to <= from {
            return Err(UnifiedError::bad_request(
                "effective_to must be after effective_from",
            ));
        }
    }
    Ok(())
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

pub(crate) fn load_stage(
    state: &PlmState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PlmStage> {
    let id = parse_uuid(raw_id, "stage")?;
    let stage = state
        .stages
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Stage not found: {id}")))?;
    tenant_match(&stage.tenant_id, tenant_id, id, "Stage")?;
    Ok(stage)
}

pub(crate) fn load_stage_by_slug(
    state: &PlmState,
    tenant_id: &str,
    slug: &str,
) -> UnifiedResult<PlmStage> {
    state
        .stages
        .iter()
        .map(|entry| entry.value().clone())
        .find(|stage| stage.tenant_id == tenant_id && stage.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Stage not found: {slug}")))
}

pub(crate) fn load_version(
    state: &PlmState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PlmVersion> {
    let id = parse_uuid(raw_id, "version")?;
    let version = state
        .versions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Version not found: {id}")))?;
    tenant_match(&version.tenant_id, tenant_id, id, "Version")?;
    Ok(version)
}

pub(crate) fn load_eco(state: &PlmState, tenant_id: &str, raw_id: &str) -> UnifiedResult<PlmEco> {
    let id = parse_uuid(raw_id, "eco")?;
    let eco = state
        .ecos
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("ECO not found: {id}")))?;
    tenant_match(&eco.tenant_id, tenant_id, id, "ECO")?;
    Ok(eco)
}

pub(crate) fn load_approval(
    state: &PlmState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PlmApproval> {
    let id = parse_uuid(raw_id, "approval")?;
    let approval = state
        .approvals
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Approval not found: {id}")))?;
    tenant_match(&approval.tenant_id, tenant_id, id, "Approval")?;
    Ok(approval)
}

pub(crate) fn load_bom_version(
    state: &PlmState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PlmBomVersion> {
    let id = parse_uuid(raw_id, "bom_version")?;
    let bom_version = state
        .bom_versions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BOM version not found: {id}")))?;
    tenant_match(&bom_version.tenant_id, tenant_id, id, "BOM version")?;
    Ok(bom_version)
}

/// Decisions on one entity, as `(step_index, decision)` pairs,
/// for the pure approval-completeness check.
pub(crate) fn decisions_for(
    state: &PlmState,
    entity_type: &str,
    entity_id: Uuid,
) -> Vec<(i64, ApprovalDecision)> {
    state
        .approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|approval| {
            approval.entity_type == entity_type
                && approval.entity_id == entity_id
                && approval.decision != ApprovalDecision::Pending
        })
        .map(|approval| (approval.step_index, approval.decision))
        .collect()
}

/// Approved version approvals for one version: how many
/// approvals a stage-entry gate can count.
pub(crate) fn approved_version_approvals(
    state: &PlmState,
    tenant_id: &str,
    version_id: Uuid,
) -> i64 {
    state
        .approvals
        .iter()
        .filter(|entry| {
            let approval = entry.value();
            approval.tenant_id == tenant_id
                && approval.entity_type == "version"
                && approval.entity_id == version_id
                && approval.decision == ApprovalDecision::Approved
        })
        .count() as i64
}

/// ECO view with its approvals attached, so a single read shows
/// the whole change story.
pub(crate) fn eco_view(state: &PlmState, eco: &PlmEco) -> Value {
    let mut approvals: Vec<PlmApproval> = state
        .approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|approval| approval.entity_type == "eco" && approval.entity_id == eco.id)
        .collect();
    approvals.sort_by_key(|approval| (approval.step_index, approval.created_at));
    let mut value = serde_json::to_value(eco).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "approvals".to_string(),
            serde_json::to_value(approvals).unwrap_or(Value::Null),
        );
    }
    value
}

/// Version view with its approvals attached.
pub(crate) fn version_view(state: &PlmState, version: &PlmVersion) -> Value {
    let mut approvals: Vec<PlmApproval> = state
        .approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|approval| approval.entity_type == "version" && approval.entity_id == version.id)
        .collect();
    approvals.sort_by_key(|approval| approval.created_at);
    let mut value = serde_json::to_value(version).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("revision".to_string(), Value::String(version.revision()));
        map.insert(
            "approvals".to_string(),
            serde_json::to_value(approvals).unwrap_or(Value::Null),
        );
    }
    value
}

/// Next ECO code for a tenant: `ECO-0001`, `ECO-0002`, and so
/// on, skipping any code already in use.
pub(crate) fn next_eco_code(state: &PlmState, tenant_id: &str) -> String {
    let mut sequence = state
        .ecos
        .iter()
        .filter(|entry| entry.value().tenant_id == tenant_id)
        .count() as i64
        + 1;
    loop {
        let code = format!("ECO-{sequence:04}");
        if !state
            .ecos
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == code)
        {
            return code;
        }
        sequence += 1;
    }
}

/// Highest BOM version number for one BOM in one tenant, or
/// `None` when the BOM has no PLM versions yet.
pub(crate) fn max_bom_version(state: &PlmState, tenant_id: &str, bom_id: &str) -> Option<i64> {
    state
        .bom_versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| version.tenant_id == tenant_id && version.bom_id == bom_id)
        .map(|version| version.version_number)
        .max()
}

pub(crate) fn eco_status_from_str(raw: &str) -> UnifiedResult<EcoStatus> {
    EcoStatus::from_str(raw).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "to_status must be one of draft, review, approved, implemented, closed, rejected, got \"{raw}\""
        ))
    })
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
    fn valid_slug_rules() {
        assert!(valid_slug("phase-out"));
        assert!(valid_slug("a"));
        assert!(!valid_slug("Phase-Out"));
        assert!(!valid_slug("a--b"));
        assert!(!valid_slug("-a"));
        assert!(!valid_slug("a-"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a_b"));
    }

    #[test]
    fn require_slug_lowercases_and_validates() {
        assert_eq!(require_slug("Phase-Out", "slug").unwrap(), "phase-out");
        assert!(require_slug("Phase Out", "slug").is_err());
    }

    #[test]
    fn impact_level_validation() {
        assert_eq!(validate_impact_level(&None).unwrap(), None);
        assert_eq!(
            validate_impact_level(&Some("HIGH".to_string())).unwrap(),
            Some("high".to_string())
        );
        assert!(validate_impact_level(&Some("severe".to_string())).is_err());
    }

    #[test]
    fn approval_steps_validation() {
        use crate::modules::plm::types::ApprovalStep;
        let good = vec![ApprovalStep {
            name: "Engineering".to_string(),
            approver_role: "engineering".to_string(),
            required: true,
        }];
        assert!(validate_approval_steps(&good).is_ok());
        let bad_role = vec![ApprovalStep {
            name: "Engineering".to_string(),
            approver_role: "Eng Lead".to_string(),
            required: true,
        }];
        assert!(validate_approval_steps(&bad_role).is_err());
        let too_many: Vec<ApprovalStep> = (0..21)
            .map(|index| ApprovalStep {
                name: format!("Step {index}"),
                approver_role: "engineering".to_string(),
                required: true,
            })
            .collect();
        assert!(validate_approval_steps(&too_many).is_err());
    }

    #[test]
    fn effective_range_validation() {
        use chrono::TimeZone;
        let from = Utc.with_ymd_and_hms(2026, 11, 22, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 12, 22, 0, 0, 0).unwrap();
        assert!(validate_effective_range(Some(from), Some(to)).is_ok());
        assert!(validate_effective_range(Some(to), Some(from)).is_err());
        assert!(validate_effective_range(Some(from), Some(from)).is_err());
        assert!(validate_effective_range(None, Some(to)).is_ok());
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
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn tenant_request(company_id: &str, caller_company_id: Option<&str>) -> HttpRequest {
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
        if let Some(company) = caller_company_id {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
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

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
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
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
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
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let req = TestRequest::default()
            .insert_header(("x-company-id", "other"))
            .to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }
}
