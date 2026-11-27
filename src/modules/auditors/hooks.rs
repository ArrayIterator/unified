// Hook listeners: scope checks and expiry.

use unified_core::hooks::HookContext;
use unified_core::sharing::{assert_auditor_action_allowed, sharing_events};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// Hook listeners

/// Answer `auditors.scope.check` from the stored scope. Payload:
/// `{"user_id", "module", "resource_kind", "resource_id",
/// "company_id", "action", "allowed": false}`. `action` is `"read"`
/// or `"export"`; anything else is denied. An unknown user id, an
/// expired account, or a revoked account is denied. The answer is
/// written back into the payload's `"allowed"` key.
pub(crate) fn on_scope_check(state: AuditorsState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let module = ctx
        .get("module")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let resource_kind = ctx
        .get("resource_kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let resource_id = ctx
        .get("resource_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let company_id = ctx
        .get("company_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let action = ctx
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let allowed = match state.auditor_for_user(&user_id) {
        Some(account) if account.is_usable(Utc::now()) => {
            let scope = state.scope_of(account.id);
            assert_auditor_action_allowed(
                &scope,
                &module,
                &resource_kind,
                resource_id,
                company_id,
                &action,
            )
            .is_ok()
        }
        _ => false,
    };
    ctx.set("allowed", Value::Bool(allowed));
}

/// Handle `auditors.expire_requested` from the scheduler: expire
/// the named account when due, or sweep all due accounts when no
/// id is given. The payload id is never trusted blindly: the
/// account is expired only when its stored expiry has actually
/// passed.
pub(crate) fn on_expire_requested(state: AuditorsState, ctx: &mut HookContext) {
    let now = Utc::now();
    match ctx
        .get("auditor_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
    {
        Some(id) => {
            let due = state
                .auditors
                .get(&id)
                .is_some_and(|account| account.status == AuditorStatus::Active && now > account.expires_at);
            if due {
                let expired = expire_due_auditors(&state, now);
                ctx.add_result(json!({ "expired": expired.iter().map(|id| id.to_string()).collect::<Vec<_>>() }));
            } else {
                ctx.add_result(json!({ "expired": Vec::<String>::new(), "reason": "auditor not due for expiry" }));
            }
        }
        None => {
            let expired = expire_due_auditors(&state, now);
            ctx.add_result(json!({ "expired": expired.iter().map(|id| id.to_string()).collect::<Vec<_>>() }));
        }
    }
}

/// Handle `auth.login`: when the logging-in user id belongs to an
/// active auditor account, record a session row so the banner
/// endpoint can show it. The payload shape is read defensively;
/// an unrecognised payload is ignored, never an error.
pub(crate) fn on_auth_login(state: AuditorsState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .or_else(|| ctx.get("actor"))
        .or_else(|| ctx.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if user_id.is_empty() {
        return;
    }
    let Some(account) = state.auditor_for_user(&user_id) else {
        return;
    };
    if !account.is_usable(Utc::now()) {
        return;
    }
    let now = Utc::now();
    let session = AuditorSession {
        id: Uuid::now_v7(),
        auditor_id: account.id,
        user_id: account.user_id.clone(),
        started_at: now,
        last_seen_at: now,
        revoked_at: None,
        revoked_by: None,
    };
    state.sessions.insert(session.id, session.clone());
    state.emit(
        sharing_events::AUDITOR_SESSION_STARTED,
        json!({
            "auditor_id": account.id.to_string(),
            "session_id": session.id.to_string(),
            "user_id": account.user_id,
        }),
    );
}
