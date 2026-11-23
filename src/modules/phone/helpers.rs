//! Caller, permission, loading, and view helpers.

use super::state::PhoneState;
use super::types::{
    valid_code, valid_ivr_key, AutoResponseScript, CallParticipant, CallRecording, IvrMenu,
    IvrNode, PhoneCall, PhoneQueue, QueueMember, QueueWaiter, RoutingRule, MAX_CODE_LEN,
    MAX_DESCRIPTION_LEN, MAX_GREETING_LEN, MAX_LABEL_LEN, MAX_TEMPLATE_LEN, MAX_USER_ID_LEN,
    PERM_PHONE_MANAGE,
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
/// exactly like the forums and kyc modules.
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
    check_capability(ctx, role, PERM_PHONE_MANAGE)
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

pub(crate) fn require_user_id(value: &str) -> UnifiedResult<String> {
    require_text(value, "user_id", MAX_USER_ID_LEN)
}

pub(crate) fn require_greeting(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "greeting", MAX_GREETING_LEN)
}

pub(crate) fn require_template(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "response_template", MAX_TEMPLATE_LEN)
}

pub(crate) fn require_ivr_key(value: &str) -> UnifiedResult<String> {
    let key = value.trim().to_string();
    if !valid_ivr_key(&key) {
        return Err(UnifiedError::bad_request(
            "IVR key must be a single digit, letter, * or #",
        ));
    }
    Ok(key)
}

pub(crate) fn require_time_of_day(raw: &str, field: &str) -> UnifiedResult<String> {
    let value = raw.trim().to_string();
    if super::types::parse_time_of_day(&value).is_none() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a valid 24-hour time (HH:MM)"
        )));
    }
    Ok(value)
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

pub(crate) fn load_call(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PhoneCall> {
    let id = parse_uuid(raw_id, "call")?;
    let call = state
        .calls
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Call not found: {id}")))?;
    tenant_match(&call.tenant_id, tenant_id, id, "Call")?;
    Ok(call)
}

/// A call is readable by its caller, by any of its
/// participants, by anyone holding the manage capability, and
/// by no one else (other tenants and other users read 404,
/// never 403).
pub(crate) fn load_visible_call(
    state: &PhoneState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    call_id: &str,
) -> UnifiedResult<PhoneCall> {
    let call = load_call(state, tenant_id, call_id)?;
    let is_participant = state
        .call_participants(call.id)
        .iter()
        .any(|participant| participant.user_id == caller.actor);
    if call.caller_id != caller.actor && !is_participant && !can_manage(ctx, &caller.role) {
        return Err(UnifiedError::not_found(format!(
            "Call not found: {}",
            call.id
        )));
    }
    Ok(call)
}

/// A call is actionable by its caller, by a participant, or by
/// a manager — used by the accept / reject / end / hold /
/// resume actions. The caller of a queued call (the waiter)
/// acts through the participant row created at answer time.
pub(crate) fn load_actionable_call(
    state: &PhoneState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    call_id: &str,
) -> UnifiedResult<PhoneCall> {
    load_visible_call(state, ctx, tenant_id, caller, call_id)
}

pub(crate) fn load_queue(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PhoneQueue> {
    let id = parse_uuid(raw_id, "queue")?;
    let queue = state
        .queues
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Queue not found: {id}")))?;
    tenant_match(&queue.tenant_id, tenant_id, id, "Queue")?;
    Ok(queue)
}

pub(crate) fn load_queue_by_code(
    state: &PhoneState,
    tenant_id: &str,
    code: &str,
) -> UnifiedResult<PhoneQueue> {
    state
        .queue_by_code(tenant_id, code)
        .ok_or_else(|| UnifiedError::not_found(format!("Queue not found: {code}")))
}

pub(crate) fn load_member(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<QueueMember> {
    let id = parse_uuid(raw_id, "queue member")?;
    let member = state
        .queue_members
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Queue member not found: {id}")))?;
    tenant_match(&member.tenant_id, tenant_id, id, "Queue member")?;
    Ok(member)
}

pub(crate) fn load_waiter(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<QueueWaiter> {
    let id = parse_uuid(raw_id, "queue waiter")?;
    let waiter = state
        .queue_waiters
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Queue waiter not found: {id}")))?;
    tenant_match(&waiter.tenant_id, tenant_id, id, "Queue waiter")?;
    Ok(waiter)
}

pub(crate) fn load_rule(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<RoutingRule> {
    let id = parse_uuid(raw_id, "routing rule")?;
    let rule = state
        .routing_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Routing rule not found: {id}")))?;
    tenant_match(&rule.tenant_id, tenant_id, id, "Routing rule")?;
    Ok(rule)
}

pub(crate) fn load_menu(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<IvrMenu> {
    let id = parse_uuid(raw_id, "IVR menu")?;
    let menu = state
        .ivr_menus
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("IVR menu not found: {id}")))?;
    tenant_match(&menu.tenant_id, tenant_id, id, "IVR menu")?;
    Ok(menu)
}

pub(crate) fn load_menu_by_code(
    state: &PhoneState,
    tenant_id: &str,
    code: &str,
) -> UnifiedResult<IvrMenu> {
    state
        .menu_by_code(tenant_id, code)
        .ok_or_else(|| UnifiedError::not_found(format!("IVR menu not found: {code}")))
}

pub(crate) fn load_node(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<IvrNode> {
    let id = parse_uuid(raw_id, "IVR node")?;
    let node = state
        .ivr_nodes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("IVR node not found: {id}")))?;
    tenant_match(&node.tenant_id, tenant_id, id, "IVR node")?;
    Ok(node)
}

pub(crate) fn load_script(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<AutoResponseScript> {
    let id = parse_uuid(raw_id, "auto-response script")?;
    let script = state
        .scripts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Auto-response script not found: {id}")))?;
    tenant_match(&script.tenant_id, tenant_id, id, "Auto-response script")?;
    Ok(script)
}

pub(crate) fn load_recording(
    state: &PhoneState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<CallRecording> {
    let id = parse_uuid(raw_id, "recording")?;
    let recording = state
        .recordings
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Recording not found: {id}")))?;
    tenant_match(&recording.tenant_id, tenant_id, id, "Recording")?;
    Ok(recording)
}

/// Find the participant row for one user in one call.
pub(crate) fn load_participant(
    state: &PhoneState,
    call_id: Uuid,
    user_id: &str,
) -> UnifiedResult<CallParticipant> {
    state
        .call_participants(call_id)
        .into_iter()
        .find(|participant| participant.user_id == user_id)
        .ok_or_else(|| {
            UnifiedError::not_found(format!("User {user_id} is not a participant of this call"))
        })
}

// ---------------------------------------------------------------------------
// Views and events
// ---------------------------------------------------------------------------

/// Call view with its participants, signalling inbox size, and
/// recordings attached, so a single read shows the whole call.
pub(crate) fn call_view(state: &PhoneState, call: &PhoneCall) -> Value {
    let mut value = serde_json::to_value(call).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let participants = state.call_participants(call.id);
        map.insert(
            "participants".to_string(),
            serde_json::to_value(participants).unwrap_or(Value::Null),
        );
        let recordings = state.call_recordings(call.id);
        map.insert(
            "recordings".to_string(),
            serde_json::to_value(recordings).unwrap_or(Value::Null),
        );
        map.insert(
            "undelivered_signals".to_string(),
            Value::from(
                state
                    .call_signals(call.id)
                    .iter()
                    .filter(|signal| !signal.delivered)
                    .count() as u64,
            ),
        );
    }
    value
}

/// Emit a call event on the hook bus with the standard call
/// payload, and notify every participant who is not the actor.
pub(crate) fn call_event(
    state: &PhoneState,
    event: &str,
    call: &PhoneCall,
    actor_id: &str,
    detail: Option<String>,
) {
    let participant_ids: Vec<String> = state
        .call_participants(call.id)
        .iter()
        .map(|participant| participant.user_id.clone())
        .collect();
    state.emit(
        event,
        json!({
            "tenant_id": call.tenant_id,
            "call_id": call.id.to_string(),
            "caller_id": call.caller_id,
            "kind": call.kind.as_str(),
            "status": call.status.as_str(),
            "outcome": call.outcome.map(|outcome| outcome.as_str()),
            "queue_id": call.queue_id.map(|id| id.to_string()),
            "ivr_menu_id": call.ivr_menu_id.map(|id| id.to_string()),
            "participant_ids": participant_ids,
            "duration_secs": call.duration_secs,
            "actor_id": actor_id,
            "detail": detail,
        }),
    );
    for user_id in &participant_ids {
        if user_id == actor_id {
            continue;
        }
        state.notify(
            "phone_call",
            user_id,
            "Phone call update",
            &format!("Call {} is now {}", call.id, call.status.as_str()),
            None,
        );
    }
}

/// Emit a queue event on the hook bus.
pub(crate) fn queue_event(
    state: &PhoneState,
    event: &str,
    queue: &PhoneQueue,
    actor_id: &str,
    detail: Option<String>,
) {
    state.emit(
        event,
        json!({
            "tenant_id": queue.tenant_id,
            "queue_id": queue.id.to_string(),
            "queue_code": queue.code,
            "ring_strategy": queue.ring_strategy.as_str(),
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
    fn require_code_rules() {
        assert_eq!(require_code("My-Code", "code").unwrap(), "my-code");
        assert!(require_code("a--b", "code").is_err());
        assert!(require_code("a_b", "code").is_err());
        assert!(require_code("", "code").is_err());
    }

    #[test]
    fn require_ivr_key_rules() {
        assert_eq!(require_ivr_key("1").unwrap(), "1");
        assert_eq!(require_ivr_key("*").unwrap(), "*");
        assert_eq!(require_ivr_key("#").unwrap(), "#");
        assert!(require_ivr_key("12").is_err());
        assert!(require_ivr_key("").is_err());
    }

    #[test]
    fn require_time_of_day_rules() {
        assert_eq!(require_time_of_day("09:30", "start").unwrap(), "09:30");
        assert!(require_time_of_day("9:30", "start").is_err());
        assert!(require_time_of_day("24:00", "start").is_err());
        assert!(require_time_of_day("09:60", "start").is_err());
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
