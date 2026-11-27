// Contracts - Helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// Tenant edge guard (C3 pilot 4, group B).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only: no per-record company check is applied.
/// Real partitioning needs a `tenant_id` on the records, a C11
/// follow-up.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
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

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Money: finite, non-negative, rounded to 4 decimal places.
pub(crate) fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
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

/// SHA-256 hex digest. Used for signing-token storage, content
/// sealing, and IP / user-agent hashing in the audit trail - the
/// raw value is never stored or logged.
pub(crate) fn hash_value(value: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(value.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// One unguessable signing token (48 random alphanumeric
/// characters). Only its SHA-256 hash is ever stored.
pub(crate) fn generate_token() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(SIGNING_TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// Escape a substituted value for safe inclusion in rendered
/// template output.
pub(crate) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A placeholder name is `{{name}}` with `name` made of letters,
/// digits, and underscores only.
pub(crate) fn valid_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PLACEHOLDER_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Extract every `{{name}}` marker from `body`, in order of first
/// appearance. Malformed markers (unclosed `{{`, empty name,
/// illegal characters) are rejected.
pub(crate) fn extract_placeholders(body: &str) -> UnifiedResult<Vec<String>> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(UnifiedError::bad_request(
                "Template body contains an unclosed \"{{\" placeholder",
            ));
        };
        let name = after_open[..close].trim();
        if !valid_placeholder_name(name) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid placeholder name \"{name}\": use letters, digits, and underscores only"
            )));
        }
        if !found.iter().any(|entry| entry == name) {
            found.push(name.to_string());
        }
        rest = &after_open[close + 2..];
    }
    Ok(found)
}

/// Validate a template body against its declared placeholder list:
/// every marker in the body must be declared, and the declared
/// list must be within bounds. Unknown placeholders are rejected
/// at save time, never silently dropped.
pub(crate) fn validate_template_body(body: &str, placeholders: &[String]) -> UnifiedResult<()> {
    if body.len() > MAX_BODY_LEN {
        return Err(UnifiedError::bad_request(format!(
            "body must be at most {MAX_BODY_LEN} characters"
        )));
    }
    if placeholders.len() > MAX_PLACEHOLDERS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_PLACEHOLDERS} placeholders are allowed"
        )));
    }
    for name in placeholders {
        if !valid_placeholder_name(name) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid placeholder name \"{name}\": use letters, digits, and underscores only"
            )));
        }
    }
    let used = extract_placeholders(body)?;
    for name in used {
        if !placeholders.iter().any(|declared| declared == &name) {
            return Err(UnifiedError::bad_request(format!(
                "Placeholder \"{{{{{name}}}}}\" is used in the body but not declared in \"placeholders\""
            )));
        }
    }
    Ok(())
}

/// Render a template body with `values`: every declared
/// placeholder must be supplied, unknown keys are rejected, and
/// every substituted value is HTML-escaped.
pub(crate) fn render_template(body: &str, placeholders: &[String], values: &HashMap<String, String>) -> UnifiedResult<String> {
    for key in values.keys() {
        if !placeholders.iter().any(|declared| declared == key) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown placeholder \"{key}\" for this template"
            )));
        }
    }
    for name in placeholders {
        if !values.contains_key(name) {
            return Err(UnifiedError::bad_request(format!(
                "Missing value for placeholder \"{name}\""
            )));
        }
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let close = after_open.find("}}").ok_or_else(|| {
            UnifiedError::bad_request("Template body contains an unclosed placeholder")
        })?;
        let name = after_open[..close].trim();
        let value = values.get(name).cloned().unwrap_or_default();
        out.push_str(&escape_html(&value));
        rest = &after_open[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Validate an alert-days list: 1..=365, at most 10 entries,
/// strictly descending (largest offset first), no duplicates.
pub(crate) fn validate_alert_days(days: &[i64]) -> UnifiedResult<Vec<i64>> {
    if days.is_empty() || days.len() > MAX_ALERT_DAYS {
        return Err(UnifiedError::bad_request(format!(
            "alert_days must contain 1..={MAX_ALERT_DAYS} entries"
        )));
    }
    let mut sorted = days.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    if sorted.len() != days.len() {
        return Err(UnifiedError::bad_request(
            "alert_days must not contain duplicates",
        ));
    }
    if days != sorted.as_slice() {
        return Err(UnifiedError::bad_request(
            "alert_days must be sorted in descending order (largest offset first)",
        ));
    }
    for day in days {
        if *day < 1 || *day > 365 {
            return Err(UnifiedError::bad_request(
                "alert_days entries must be between 1 and 365",
            ));
        }
    }
    Ok(days.to_vec())
}

/// Best-effort client address for audit hashing: the first entry
/// of `X-Forwarded-For` when present, else the peer address. The
/// caller hashes the result; the raw value is never stored.
pub(crate) fn request_ip(req: &HttpRequest) -> String {
    if let Some(forwarded) = req.headers().get("x-forwarded-for") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(first) = value.split(',').next() {
                let trimmed = first.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    req.connection_info()
        .realip_remote_addr()
        .unwrap_or("")
        .to_string()
}

pub(crate) fn request_user_agent(req: &HttpRequest) -> String {
    req.headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Append one audit-trail entry. `req` may be `None` for
/// scheduler-driven entries (IP / user-agent hashes are then the
/// hash of the empty string, which is still unambiguous in the
/// trail because the actor is `scheduler`).
pub(crate) fn record_audit(
    state: &ContractsState,
    contract_id: Uuid,
    signer_id: Option<Uuid>,
    actor: &str,
    action: &str,
    req: Option<&HttpRequest>,
    details: Option<String>,
) {
    let (ip_hash, user_agent_hash) = match req {
        Some(request) => (
            hash_value(&request_ip(request)),
            hash_value(&request_user_agent(request)),
        ),
        None => (hash_value(""), hash_value("")),
    };
    let entry = ContractAuditEntry {
        id: Uuid::now_v7(),
        contract_id,
        signer_id,
        actor: actor.to_string(),
        action: action.to_string(),
        ip_hash,
        user_agent_hash,
        details,
        created_at: Utc::now(),
    };
    state.audit_trail.insert(entry.id, entry);
}

pub(crate) fn load_template(state: &ContractsState, raw_id: &str) -> UnifiedResult<ContractTemplate> {
    let id = parse_uuid(raw_id, "template")?;
    state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Template not found: {id}")))
}

pub(crate) fn load_contract(state: &ContractsState, raw_id: &str) -> UnifiedResult<Contract> {
    let id = parse_uuid(raw_id, "contract")?;
    state
        .contracts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Contract not found: {id}")))
}

pub(crate) fn load_signer(state: &ContractsState, contract_id: Uuid, raw_id: &str) -> UnifiedResult<ContractSigner> {
    let id = parse_uuid(raw_id, "signer")?;
    state
        .signers
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|signer| signer.contract_id == contract_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Signer not found: {id}")))
}

pub(crate) fn signers_of(state: &ContractsState, contract_id: Uuid) -> Vec<ContractSigner> {
    let mut signers: Vec<ContractSigner> = state
        .signers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|signer| signer.contract_id == contract_id)
        .collect();
    signers.sort_by_key(|signer| (signer.order_index, signer.created_at));
    signers
}

/// Effective status at read time: an `active` contract whose end
/// date passed reads as `expired` (the stored status is
/// unchanged; the scheduler listener or the check-expiry endpoint
/// materialises it).
pub(crate) fn effective_status(contract: &Contract, now: DateTime<Utc>) -> ContractStatus {
    if contract.status == ContractStatus::Active {
        if let Some(ends_at) = contract.ends_at {
            if now >= ends_at {
                return ContractStatus::Expired;
            }
        }
    }
    contract.status
}

/// Canonical content hash recorded when a contract is sealed
/// (last signer signs). Any later content change would produce a
/// different hash, so the stored hash proves the sealed content.
pub(crate) fn seal_content_hash(contract: &Contract) -> String {
    hash_value(&format!(
        "{}|{}|{}|{}|{}|{:.4}|{}|{}|{}",
        contract.id,
        contract.title,
        contract.company_party,
        contract.counterparty_name,
        contract.counterparty_email,
        contract.value,
        contract.currency,
        contract.terms,
        contract
            .starts_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_default(),
    ))
}

/// Validate a status transition. Returns the new status or a 409
/// conflict naming the illegal move.
pub(crate) fn transition(
    contract: &Contract,
    target: ContractStatus,
    action: &str,
) -> UnifiedResult<ContractStatus> {
    let allowed = match contract.status {
        ContractStatus::Draft => [ContractStatus::Sent, ContractStatus::Voided].contains(&target),
        ContractStatus::Sent => [
            ContractStatus::PartiallySigned,
            // Sealing straight from Sent: the only caller that
            // targets Signed is the sealing step in `sign_via_link`,
            // which only runs once every non-declined signer
            // signed - a single signer, or every other signer
            // declined (see `all_non_declined_signed`).
            ContractStatus::Signed,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::PartiallySigned => [
            ContractStatus::Signed,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::Signed => [
            ContractStatus::Active,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::Active => [
            ContractStatus::Expired,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::Expired | ContractStatus::Terminated | ContractStatus::Voided => false,
    };
    if !allowed {
        return Err(UnifiedError::conflict(format!(
            "Cannot {action} a contract in status \"{}\" (target \"{}\")",
            contract.status.as_str(),
            target.as_str(),
        )));
    }
    Ok(target)
}

/// Activation guard: a contract whose end date already passed
/// must not become active - it would read as expired at once.
/// Returns a 422 when `ends_at <= now`.
pub(crate) fn reject_activation_if_ended(
    contract: &Contract,
    now: DateTime<Utc>,
) -> UnifiedResult<()> {
    if let Some(ends_at) = contract.ends_at {
        if ends_at <= now {
            return Err(UnifiedError::unprocessable(
                "Cannot activate a contract whose end date has passed",
            ));
        }
    }
    Ok(())
}

/// Queue one expiry alert per armed offset through
/// `scheduler.enqueue` with kind `contracts.expiry_alert`. Mirrors
/// the events module's `schedule_reminder`: the scheduler answers
/// `{"queued": true, "job_id": ...}` when it knows the kind, and
/// `{"queued": false, "error": "Unknown job kind: ..."}` when it
/// does not. The contract records `expiry_alert_status` honestly
/// (`queued` / `declined` / `not_scheduled`): `declined` only when
/// the scheduler actually declined an enqueue, `not_scheduled`
/// when no alert was attempted at all.
pub(crate) fn schedule_expiry_alerts(state: &ContractsState, contract: &mut Contract) {
    let Some(ends_at) = contract.ends_at else {
        contract.expiry_alert_status = "not_scheduled".to_string();
        return;
    };
    let Some(bus) = state.bus() else {
        // No hook bus, so no enqueue was attempted: this is not a
        // scheduler decline.
        contract.expiry_alert_status = "not_scheduled".to_string();
        return;
    };
    let mut any_queued = false;
    let mut any_declined = false;
    for days_before in &contract.alert_days {
        let send_at = ends_at - Duration::days(*days_before);
        if send_at <= Utc::now() {
            continue;
        }
        let answer = bus.emit(
            "scheduler.enqueue",
            json!({
                "kind": "contracts.expiry_alert",
                "payload": {
                    "contract_id": contract.id.to_string(),
                    "days_before": days_before,
                    "send_at": send_at.to_rfc3339(),
                },
            }),
        );
        match answer.results.first() {
            Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
                any_queued = true;
            }
            _ => {
                any_declined = true;
            }
        }
    }
    contract.expiry_alert_status = if any_queued {
        "queued".to_string()
    } else if any_declined {
        "declined".to_string()
    } else {
        // No alert was attempted: every armed offset is already
        // past, so nothing was queued and nothing was declined.
        "not_scheduled".to_string()
    };
}

/// Materialise expiry for one contract: when the end date passed
/// while `active`, move it to `expired`, emit
/// `contracts.expired`, notify the creator, and audit the move.
/// Returns true when the contract was expired by this call.
pub(crate) fn materialise_expiry(state: &ContractsState, contract: &mut Contract) -> bool {
    let now = Utc::now();
    if contract.status != ContractStatus::Active {
        return false;
    }
    let Some(ends_at) = contract.ends_at else {
        return false;
    };
    if now < ends_at {
        return false;
    }
    contract.status = ContractStatus::Expired;
    contract.updated_at = now;
    state.emit(
        "contracts.expired",
        json!({
            "contract_id": contract.id.to_string(),
            "title": contract.title,
            "ends_at": ends_at.to_rfc3339(),
        }),
    );
    state.notify(
        "contract.expired",
        &contract.created_by,
        &format!("Contract expired: {}", contract.title),
        &format!(
            "Contract \"{}\" with {} reached its end date and is now expired.",
            contract.title, contract.counterparty_name
        ),
        Some(format!("/api/v1/contracts/{}", contract.id)),
    );
    record_audit(
        state,
        contract.id,
        None,
        "scheduler",
        "expired",
        None,
        Some(format!("End date {} passed", ends_at.to_rfc3339())),
    );
    true
}

/// Public view of a contract: the sealed hash is exposed (it
/// proves the sealed content) but no signer token material ever
/// appears here - tokens live only on signer rows as hashes.
pub(crate) fn contract_view(state: &ContractsState, contract: &Contract) -> Value {
    let now = Utc::now();
    let signers = signers_of(state, contract.id);
    let signed_count = signers
        .iter()
        .filter(|signer| signer.status == SignerStatus::Signed)
        .count();
    json!({
        "id": contract.id.to_string(),
        "template_id": contract.template_id.map(|id| id.to_string()),
        "title": contract.title,
        "company_party": contract.company_party,
        "counterparty_name": contract.counterparty_name,
        "counterparty_email": contract.counterparty_email,
        "terms": contract.terms,
        "value": contract.value,
        "currency": contract.currency,
        "signing_order": contract.signing_order.as_str(),
        "status": effective_status(contract, now).as_str(),
        "stored_status": contract.status.as_str(),
        "starts_at": contract.starts_at.map(|value| value.to_rfc3339()),
        "ends_at": contract.ends_at.map(|value| value.to_rfc3339()),
        "signed_at": contract.signed_at.map(|value| value.to_rfc3339()),
        "content_hash": contract.content_hash,
        "void_reason": contract.void_reason,
        "terminate_reason": contract.terminate_reason,
        "renewed_from_id": contract.renewed_from_id.map(|id| id.to_string()),
        "alert_days": contract.alert_days,
        "expiry_alert_status": contract.expiry_alert_status,
        "signers_total": signers.len(),
        "signers_signed": signed_count,
        "created_by": contract.created_by,
        "created_at": contract.created_at.to_rfc3339(),
        "updated_at": contract.updated_at.to_rfc3339(),
    })
}

/// Public view of a signer: the token hash is exposed (it is a
/// hash, not a credential); the raw token is never returned except
/// once, inside the create / resend response, under `token`.
pub(crate) fn signer_view(signer: &ContractSigner) -> Value {
    json!({
        "id": signer.id.to_string(),
        "contract_id": signer.contract_id.to_string(),
        "name": signer.name,
        "email": signer.email,
        "order_index": signer.order_index,
        "status": signer.status.as_str(),
        "token_hash": signer.token_hash,
        "token_expires_at": signer.token_expires_at.to_rfc3339(),
        "signed_at": signer.signed_at.map(|value| value.to_rfc3339()),
        "declined_reason": signer.declined_reason,
        "viewed_at": signer.viewed_at.map(|value| value.to_rfc3339()),
        "created_at": signer.created_at.to_rfc3339(),
        "updated_at": signer.updated_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contract(
        status: ContractStatus,
        ends_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Contract {
        Contract {
            id: uuid::Uuid::new_v4(),
            template_id: None,
            title: "Sample".to_string(),
            company_party: "Acme".to_string(),
            counterparty_name: "Beta".to_string(),
            counterparty_email: "beta@example.com".to_string(),
            terms: "Terms".to_string(),
            value: 100.0,
            currency: "USD".to_string(),
            signing_order: SigningOrder::Sequential,
            status,
            starts_at: None,
            ends_at,
            signed_at: None,
            content_hash: None,
            void_reason: None,
            terminate_reason: None,
            renewed_from_id: None,
            alert_days: vec![],
            expiry_alert_status: "not_scheduled".to_string(),
            created_by: "admin".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn email_valid_accepts_well_formed_address() {
        assert!(valid_email("signer@example.com"));
    }

    #[test]
    fn email_rejects_malformed_addresses() {
        assert!(!valid_email("no-at-sign"));
        assert!(!valid_email("a@b@c.co"));
        assert!(!valid_email("a b@c.co"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@.b.co"));
        assert!(!valid_email(""));
    }

    #[test]
    fn currency_uppercases_iso_code() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
    }

    #[test]
    fn currency_rejects_short_code() {
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
    }

    #[test]
    fn price_rejects_negative_and_non_finite() {
        assert!(valid_price(-1.0).is_err());
        assert!(valid_price(f64::NAN).is_err());
        assert!(valid_price(f64::INFINITY).is_err());
    }

    #[test]
    fn price_rounds_to_four_decimals() {
        assert_eq!(valid_price(1.23456).unwrap(), 1.2346);
        assert_eq!(valid_price(10.0).unwrap(), 10.0);
    }

    #[test]
    fn escape_html_escapes_all_special_characters() {
        assert_eq!(escape_html("<a>&\"'"), "&lt;a&gt;&amp;&quot;&#x27;");
    }

    #[test]
    fn extract_placeholders_returns_first_appearance_order_deduped() {
        let found = extract_placeholders(
            "Hello {{name}}, order {{order_id}} and {{name}} again",
        )
        .unwrap();
        assert_eq!(found, vec!["name".to_string(), "order_id".to_string()]);
    }

    #[test]
    fn extract_placeholders_rejects_unclosed_marker() {
        assert!(extract_placeholders("Hello {{name").is_err());
        assert!(extract_placeholders("Hello {{bad-name}}").is_err());
    }

    #[test]
    fn render_template_substitutes_and_escapes_values() {
        let placeholders = vec!["name".to_string()];
        let mut values = std::collections::HashMap::new();
        values.insert("name".to_string(), "<Bob>".to_string());
        let out = render_template("Dear {{name}}", &placeholders, &values).unwrap();
        assert_eq!(out, "Dear &lt;Bob&gt;");
    }

    #[test]
    fn render_template_rejects_missing_and_unknown_values() {
        let placeholders = vec!["name".to_string()];
        let empty: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        assert!(render_template("Dear {{name}}", &placeholders, &empty).is_err());
        let mut extra = std::collections::HashMap::new();
        extra.insert("name".to_string(), "Bob".to_string());
        extra.insert("other".to_string(), "x".to_string());
        assert!(render_template("Dear {{name}}", &placeholders, &extra).is_err());
    }

    #[test]
    fn alert_days_accepts_descending_list() {
        assert_eq!(
            validate_alert_days(&[30, 14, 7, 1]).unwrap(),
            vec![30, 14, 7, 1]
        );
    }

    #[test]
    fn alert_days_rejects_unsorted_and_duplicate_lists() {
        assert!(validate_alert_days(&[14, 30]).is_err());
        assert!(validate_alert_days(&[30, 30]).is_err());
    }

    #[test]
    fn alert_days_rejects_empty_list() {
        assert!(validate_alert_days(&[]).is_err());
    }

    #[test]
    fn transition_allows_draft_to_sent() {
        let contract = sample_contract(ContractStatus::Draft, None);
        assert_eq!(
            transition(&contract, ContractStatus::Sent, "send").unwrap(),
            ContractStatus::Sent
        );
    }

    #[test]
    fn transition_rejects_draft_to_active() {
        let contract = sample_contract(ContractStatus::Draft, None);
        assert!(transition(&contract, ContractStatus::Active, "activate").is_err());
    }

    #[test]
    fn transition_rejects_moves_from_terminal_states() {
        let contract = sample_contract(ContractStatus::Voided, None);
        assert!(transition(&contract, ContractStatus::Sent, "send").is_err());
        let contract = sample_contract(ContractStatus::Terminated, None);
        assert!(transition(&contract, ContractStatus::Voided, "void").is_err());
    }

    #[test]
    fn effective_status_expires_active_contract_past_end_date() {
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        let contract = sample_contract(ContractStatus::Active, Some(past));
        assert_eq!(
            effective_status(&contract, chrono::Utc::now()),
            ContractStatus::Expired
        );
    }

    #[test]
    fn effective_status_keeps_future_active_contract_active() {
        let future = chrono::Utc::now() + chrono::Duration::days(1);
        let contract = sample_contract(ContractStatus::Active, Some(future));
        assert_eq!(
            effective_status(&contract, chrono::Utc::now()),
            ContractStatus::Active
        );
    }

    #[test]
    fn hash_value_empty_input_is_known_sha256() {
        assert_eq!(
            hash_value(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn render_template_rejects_unclosed_placeholder_without_panicking() {
        let placeholders = vec!["name".to_string()];
        let mut values = std::collections::HashMap::new();
        values.insert("name".to_string(), "Bob".to_string());
        let error = render_template("Dear {{name", &placeholders, &values).unwrap_err();
        assert_eq!(error.code, 400);
        assert_eq!(
            error.message,
            "Template body contains an unclosed placeholder"
        );
    }

    #[test]
    fn reject_activation_if_ended_returns_422_for_past_end_date() {
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        let contract = sample_contract(ContractStatus::Signed, Some(past));
        let error =
            reject_activation_if_ended(&contract, chrono::Utc::now()).unwrap_err();
        assert_eq!(error.code, 422);
        let future = chrono::Utc::now() + chrono::Duration::days(1);
        let contract = sample_contract(ContractStatus::Signed, Some(future));
        assert!(reject_activation_if_ended(&contract, chrono::Utc::now()).is_ok());
        let contract = sample_contract(ContractStatus::Signed, None);
        assert!(reject_activation_if_ended(&contract, chrono::Utc::now()).is_ok());
    }

    #[test]
    fn transition_allows_sent_to_signed_for_sealing() {
        let contract = sample_contract(ContractStatus::Sent, None);
        assert_eq!(
            transition(&contract, ContractStatus::Signed, "seal").unwrap(),
            ContractStatus::Signed
        );
        let contract = sample_contract(ContractStatus::PartiallySigned, None);
        assert_eq!(
            transition(&contract, ContractStatus::Signed, "seal").unwrap(),
            ContractStatus::Signed
        );
        let contract = sample_contract(ContractStatus::Draft, None);
        assert!(transition(&contract, ContractStatus::Signed, "seal").is_err());
    }

    fn state_with_bus() -> ContractsState {
        let state = ContractsState::default();
        *state.hooks.write().unwrap() = Some(Arc::new(HookBus::new()));
        state
    }

    #[test]
    fn schedule_expiry_alerts_reports_not_scheduled_when_all_offsets_past() {
        let state = state_with_bus();
        let mut contract = sample_contract(
            ContractStatus::Active,
            Some(chrono::Utc::now() + chrono::Duration::days(1)),
        );
        contract.alert_days = vec![30, 14, 7];
        schedule_expiry_alerts(&state, &mut contract);
        assert_eq!(contract.expiry_alert_status, "not_scheduled");
    }

    #[test]
    fn schedule_expiry_alerts_reports_declined_when_enqueue_is_refused() {
        // The bus has no scheduler listener, so the enqueue attempt
        // is answered with no queued result: a real decline.
        let state = state_with_bus();
        let mut contract = sample_contract(
            ContractStatus::Active,
            Some(chrono::Utc::now() + chrono::Duration::days(60)),
        );
        contract.alert_days = vec![30];
        schedule_expiry_alerts(&state, &mut contract);
        assert_eq!(contract.expiry_alert_status, "declined");
    }

    #[test]
    fn schedule_expiry_alerts_reports_not_scheduled_without_end_date() {
        let state = state_with_bus();
        let mut contract = sample_contract(ContractStatus::Active, None);
        contract.alert_days = vec![30];
        schedule_expiry_alerts(&state, &mut contract);
        assert_eq!(contract.expiry_alert_status, "not_scheduled");
    }
}
