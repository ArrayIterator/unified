//! Caller, permission, loading, audit, and view helpers.

use super::state::KycState;
use super::types::{
    valid_code, valid_content_hash, KycAuditEntry, KycCase, KycDocument, KycDocumentType,
    KycInstruction, KycRiskLevel, KycSubjectType, KycVerificationContext, AUDIT_GENESIS_HASH,
    MAX_CODE_LEN, MAX_DESCRIPTION_LEN, MAX_INSTRUCTION_LEN, MAX_LABEL_LEN, MAX_REASON_LEN,
    MAX_SUBJECT_ID_LEN, PERM_KYC_MANAGE,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the forums module.
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
    check_capability(ctx, role, PERM_KYC_MANAGE)
}

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
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

pub(crate) fn require_code(value: &str, field: &str) -> UnifiedResult<String> {
    let code = value.trim().to_lowercase();
    if !valid_code(&code) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max {MAX_CODE_LEN} characters)"
        )));
    }
    Ok(code)
}

pub(crate) fn require_label(value: &str) -> UnifiedResult<String> {
    require_text(value, "label", MAX_LABEL_LEN)
}

pub(crate) fn require_description(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "description", MAX_DESCRIPTION_LEN)
}

pub(crate) fn require_reason(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "reason", MAX_REASON_LEN)
}

pub(crate) fn require_subject_id(value: &str) -> UnifiedResult<String> {
    require_text(value, "subject_id", MAX_SUBJECT_ID_LEN)
}

pub(crate) fn require_instruction_text(value: &str) -> UnifiedResult<String> {
    require_text(value, "text", MAX_INSTRUCTION_LEN)
}

pub(crate) fn require_content_hash(value: &str) -> UnifiedResult<String> {
    let hash = value.trim().to_lowercase();
    if !valid_content_hash(&hash) {
        return Err(UnifiedError::bad_request(
            "content_hash must be a 64-character lowercase hex SHA-256 digest",
        ));
    }
    Ok(hash)
}

/// Validate a list of string values supplied as data: every
/// entry must be non-empty text, at most 128 chars, with no
/// duplicates.
pub(crate) fn require_string_list(values: &[String], field: &str) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for raw in values {
        let value = require_text(raw, field, MAX_CODE_LEN)?;
        if out.contains(&value) {
            return Err(UnifiedError::bad_request(format!(
                "{field} must not contain duplicates"
            )));
        }
        out.push(value);
    }
    Ok(out)
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

// Tenant-scoped loaders
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

pub(crate) fn load_case(state: &KycState, tenant_id: &str, raw_id: &str) -> UnifiedResult<KycCase> {
    let id = parse_uuid(raw_id, "case")?;
    let case = state
        .cases
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Case not found: {id}")))?;
    tenant_match(&case.tenant_id, tenant_id, id, "Case")?;
    Ok(case)
}

/// A case is readable by its subject, by the user who submitted
/// it, by anyone holding the manage capability, and by no one
/// else (other tenants and other subjects read 404, never 403).
pub(crate) fn load_visible_case(
    state: &KycState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    case_id: &str,
) -> UnifiedResult<KycCase> {
    let case = load_case(state, tenant_id, case_id)?;
    if case.subject_id != caller.actor
        && case.submitted_by != caller.actor
        && !can_manage(ctx, &caller.role)
    {
        return Err(UnifiedError::not_found(format!(
            "Case not found: {}",
            case.id
        )));
    }
    Ok(case)
}

pub(crate) fn load_document(
    state: &KycState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<KycDocument> {
    let id = parse_uuid(raw_id, "document")?;
    let document = state
        .documents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))?;
    tenant_match(&document.tenant_id, tenant_id, id, "Document")?;
    Ok(document)
}

pub(crate) fn load_document_type_by_code(
    state: &KycState,
    tenant_id: &str,
    code: &str,
) -> UnifiedResult<KycDocumentType> {
    state
        .document_types
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| row.tenant_id == tenant_id && row.code == code)
        .ok_or_else(|| UnifiedError::not_found(format!("Document type not found: {code}")))
}

pub(crate) fn load_subject_type_by_code(
    state: &KycState,
    tenant_id: &str,
    code: &str,
) -> UnifiedResult<KycSubjectType> {
    state
        .subject_types
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| row.tenant_id == tenant_id && row.code == code)
        .ok_or_else(|| UnifiedError::not_found(format!("Subject type not found: {code}")))
}

pub(crate) fn load_context(
    state: &KycState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<KycVerificationContext> {
    let id = parse_uuid(raw_id, "verification context")?;
    let context = state
        .contexts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Verification context not found: {id}")))?;
    tenant_match(&context.tenant_id, tenant_id, id, "Verification context")?;
    Ok(context)
}

pub(crate) fn load_instruction(
    state: &KycState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<KycInstruction> {
    let id = parse_uuid(raw_id, "instruction")?;
    let instruction = state
        .instructions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Instruction not found: {id}")))?;
    tenant_match(&instruction.tenant_id, tenant_id, id, "Instruction")?;
    Ok(instruction)
}

pub(crate) fn load_risk_level(
    state: &KycState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<KycRiskLevel> {
    let id = parse_uuid(raw_id, "risk level")?;
    let level = state
        .risk_levels
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Risk level not found: {id}")))?;
    tenant_match(&level.tenant_id, tenant_id, id, "Risk level")?;
    Ok(level)
}

/// Validate a risk-level score range: both ends in 0-100 and
/// `min_score <= max_score`.
pub(crate) fn validate_score_range(min_score: i64, max_score: i64) -> UnifiedResult<()> {
    if !(0..=100).contains(&min_score) || !(0..=100).contains(&max_score) {
        return Err(UnifiedError::bad_request(
            "min_score and max_score must both be between 0 and 100",
        ));
    }
    if min_score > max_score {
        return Err(UnifiedError::bad_request(
            "min_score must not be greater than max_score",
        ));
    }
    Ok(())
}

/// Reject a risk level whose active range would overlap an
/// existing active level of the same tenant (optionally
/// excluding the level being updated, so it does not overlap
/// itself).
pub(crate) fn check_risk_range_overlap(
    state: &KycState,
    tenant_id: &str,
    min_score: i64,
    max_score: i64,
    exclude_id: Option<Uuid>,
) -> UnifiedResult<()> {
    for level in state.active_risk_levels(tenant_id) {
        if Some(level.id) == exclude_id {
            continue;
        }
        if min_score <= level.max_score && level.min_score <= max_score {
            return Err(UnifiedError::conflict(format!(
                "Risk range {}-{} overlaps active risk level \"{}\" ({}-{})",
                min_score, max_score, level.code, level.min_score, level.max_score
            )));
        }
    }
    Ok(())
}

// Audit trail

/// SHA-256 hex of the audit entry payload
/// `prev_hash|id|case_id|action|actor|created_at|detail`.
pub(crate) fn audit_hash(
    prev_hash: &str,
    id: Uuid,
    case_id: Uuid,
    action: &str,
    actor: &str,
    created_at: chrono::DateTime<Utc>,
    detail: Option<&str>,
) -> String {
    let payload = format!(
        "{prev_hash}|{id}|{case_id}|{action}|{actor}|{}|{}",
        created_at.to_rfc3339(),
        detail.unwrap_or("")
    );
    let digest = Sha256::digest(payload.as_bytes());
    format!("{digest:x}")
}

/// Append one hash-chained audit entry for a case. The previous
/// hash is the latest entry of the same case (by creation
/// time), or the genesis hash when the case has no entries yet.
pub(crate) fn append_audit(
    state: &KycState,
    tenant_id: &str,
    case_id: Uuid,
    action: &str,
    actor_id: &str,
    detail: Option<String>,
) -> KycAuditEntry {
    let mut entries: Vec<KycAuditEntry> = state
        .audit_entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| entry.case_id == case_id)
        .collect();
    entries.sort_by_key(|entry| entry.created_at);
    let prev_hash = entries
        .last()
        .map(|entry| entry.hash.clone())
        .unwrap_or_else(|| AUDIT_GENESIS_HASH.to_string());
    let now = Utc::now();
    let id = Uuid::now_v7();
    let hash = audit_hash(
        &prev_hash,
        id,
        case_id,
        action,
        actor_id,
        now,
        detail.as_deref(),
    );
    let entry = KycAuditEntry {
        id,
        tenant_id: tenant_id.to_string(),
        case_id,
        action: action.to_string(),
        actor_id: actor_id.to_string(),
        detail,
        prev_hash,
        hash,
        created_at: now,
    };
    state.audit_entries.insert(entry.id, entry.clone());
    entry
}

/// Recompute the audit chain of a case from the genesis hash.
/// Returns `true` when every entry's hash matches its payload
/// and every `prev_hash` links to the previous entry.
pub(crate) fn verify_audit_chain(entries: &[KycAuditEntry]) -> bool {
    let mut ordered: Vec<&KycAuditEntry> = entries.iter().collect();
    ordered.sort_by_key(|entry| entry.created_at);
    let mut prev = AUDIT_GENESIS_HASH.to_string();
    for entry in ordered {
        if entry.prev_hash != prev {
            return false;
        }
        let recomputed = audit_hash(
            &entry.prev_hash,
            entry.id,
            entry.case_id,
            &entry.action,
            &entry.actor_id,
            entry.created_at,
            entry.detail.as_deref(),
        );
        if recomputed != entry.hash {
            return false;
        }
        prev = entry.hash.clone();
    }
    true
}

/// Case view with its linked documents and audit trail
/// attached, so a single read shows the whole verification
/// story.
pub(crate) fn case_view(state: &KycState, case: &KycCase) -> Value {
    let mut value = serde_json::to_value(case).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let documents = state.case_documents(case.id);
        map.insert(
            "documents".to_string(),
            serde_json::to_value(documents).unwrap_or(Value::Null),
        );
        let mut audit: Vec<KycAuditEntry> = state
            .audit_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.case_id == case.id)
            .collect();
        audit.sort_by_key(|entry| entry.created_at);
        let chain_valid = verify_audit_chain(&audit);
        map.insert(
            "audit".to_string(),
            serde_json::to_value(audit).unwrap_or(Value::Null),
        );
        map.insert("audit_chain_valid".to_string(), Value::Bool(chain_valid));
    }
    value
}

/// Emit a case event on the hook bus with the standard case
/// payload, and append the matching audit entry.
pub(crate) fn case_event(
    state: &KycState,
    event: &str,
    case: &KycCase,
    actor_id: &str,
    action: &str,
    detail: Option<String>,
) {
    append_audit(
        state,
        &case.tenant_id,
        case.id,
        action,
        actor_id,
        detail.clone(),
    );
    state.emit(
        event,
        json!({
            "tenant_id": case.tenant_id,
            "case_id": case.id.to_string(),
            "subject_id": case.subject_id,
            "subject_type": case.subject_type,
            "status": case.status.as_str(),
            "risk_score": case.risk_score,
            "risk_level_code": case.risk_level_code,
            "actor_id": actor_id,
            "detail": detail,
        }),
    );
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
    fn audit_chain_verifies_and_detects_tampering() {
        let state = KycState::default();
        let case_id = Uuid::new_v4();
        append_audit(&state, "tenant-1", case_id, "created", "user-1", None);
        append_audit(
            &state,
            "tenant-1",
            case_id,
            "submitted",
            "user-1",
            Some("submitted for review".to_string()),
        );
        append_audit(&state, "tenant-1", case_id, "verified", "admin-1", None);
        let entries: Vec<KycAuditEntry> = state
            .audit_entries
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        assert_eq!(entries.len(), 3);
        assert!(verify_audit_chain(&entries));
        // Tamper with one entry's detail: the chain must break.
        let mut tampered = entries.clone();
        tampered[1].detail = Some("forged".to_string());
        assert!(!verify_audit_chain(&tampered));
        // Drop the middle entry: the prev_hash link must break.
        let mut dropped = entries.clone();
        dropped.remove(1);
        assert!(!verify_audit_chain(&dropped));
    }

    #[test]
    fn audit_chain_starts_from_genesis() {
        let state = KycState::default();
        let case_id = Uuid::new_v4();
        let entry = append_audit(&state, "tenant-1", case_id, "created", "user-1", None);
        assert_eq!(entry.prev_hash, AUDIT_GENESIS_HASH);
        assert_eq!(entry.hash.len(), 64);
    }

    #[test]
    fn require_code_rules() {
        assert_eq!(require_code("My-Code", "code").unwrap(), "my-code");
        assert!(require_code("a--b", "code").is_err());
        assert!(require_code("a_b", "code").is_err());
        assert!(require_code("", "code").is_err());
    }

    #[test]
    fn content_hash_must_be_sha256_hex() {
        assert!(require_content_hash(&"b".repeat(64)).is_ok());
        assert!(require_content_hash("short").is_err());
        assert!(require_content_hash(&"B".repeat(64)).is_ok());
    }

    #[test]
    fn string_list_rejects_duplicates_and_blanks() {
        assert!(require_string_list(&["a".to_string(), "b".to_string()], "checks").is_ok());
        assert!(require_string_list(&["a".to_string(), "a".to_string()], "checks").is_err());
        assert!(require_string_list(&[" ".to_string()], "checks").is_err());
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
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
        req
    }

    #[test]
    fn resolve_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(resolve_tenant(&bare).is_err());
    }

    #[test]
    fn resolve_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant_id = resolve_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant_id.is_empty());
    }
}
