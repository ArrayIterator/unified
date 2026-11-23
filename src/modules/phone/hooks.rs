//! Hook handlers for user deletion and the AI grounding
//! hooks.
//!
//! The AI grounding hooks are the points where the stored
//! configuration reaches the AI module's gated runner: when
//! `run_ai_capability` runs the `answer_caller_question`
//! capability it emits
//! `phone.ai.answer_caller_question_requested`, and this
//! handler answers with the tenant's active IVR menus and
//! auto-response scripts, so the behaviour the AI follows is
//! the stored configuration, not text hard-coded in this
//! module. The summary capability is grounded with the call's
//! stored facts.

use super::state::PhoneState;
use super::types::{DELETED_USER_ID, EVENT_AI_ANSWER_REQUESTED, EVENT_AI_SUMMARIZE_REQUESTED};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// User deletion (users module owns the event)
// ---------------------------------------------------------------------------

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the forums module reads). The user's calls are
/// anonymized to `deleted-user` so the call log stays
/// auditable without the identity; participants, signalling
/// senders and recipients, queue members and waiters, and
/// recording starters are anonymized the same way. Presence
/// rows are deleted outright (presence is live state, not
/// history). Calls are never deleted: a call log must stay on
/// record.
pub(crate) fn on_user_deleted(state: PhoneState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(json!({ "phone": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_calls = 0i64;
    for mut entry in state.calls.iter_mut() {
        let mut changed = false;
        if entry.value().caller_id == user_id {
            entry.value_mut().caller_id = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().recording_consent_by.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().recording_consent_by = Some(DELETED_USER_ID.to_string());
            changed = true;
        }
        if changed {
            entry.value_mut().updated_at = now;
            anonymized_calls += 1;
        }
    }
    let mut anonymized_participants = 0i64;
    for mut entry in state.participants.iter_mut() {
        if entry.value().user_id == user_id {
            entry.value_mut().user_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_participants += 1;
        }
    }
    let mut anonymized_signals = 0i64;
    for mut entry in state.signals.iter_mut() {
        let mut changed = false;
        if entry.value().sender_id == user_id {
            entry.value_mut().sender_id = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().recipient_id.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().recipient_id = Some(DELETED_USER_ID.to_string());
            changed = true;
        }
        if changed {
            anonymized_signals += 1;
        }
    }
    let mut anonymized_members = 0i64;
    for mut entry in state.queue_members.iter_mut() {
        if entry.value().user_id == user_id {
            entry.value_mut().user_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_members += 1;
        }
    }
    let mut anonymized_waiters = 0i64;
    for mut entry in state.queue_waiters.iter_mut() {
        if entry.value().caller_id == user_id {
            entry.value_mut().caller_id = DELETED_USER_ID.to_string();
            anonymized_waiters += 1;
        }
    }
    let mut anonymized_recordings = 0i64;
    for mut entry in state.recordings.iter_mut() {
        if entry.value().started_by == user_id {
            entry.value_mut().started_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_recordings += 1;
        }
        if entry.value().consent_by.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().consent_by = Some(DELETED_USER_ID.to_string());
            entry.value_mut().updated_at = now;
        }
    }
    // Presence is live state, not history: the row goes away.
    let removed_presence = state.remove_presence_for_user(&user_id);
    ctx.add_result(json!({
        "phone": "user_deleted",
        "user_id": user_id,
        "anonymized_calls": anonymized_calls,
        "anonymized_participants": anonymized_participants,
        "anonymized_signals": anonymized_signals,
        "anonymized_queue_members": anonymized_members,
        "anonymized_queue_waiters": anonymized_waiters,
        "anonymized_recordings": anonymized_recordings,
        "removed_presence_rows": removed_presence,
    }));
}

// ---------------------------------------------------------------------------
// AI grounding (ai module owns the events)
// ---------------------------------------------------------------------------

/// Answer `phone.ai.answer_caller_question_requested` with
/// the grounding the AI runner merges into the system prompt:
/// the tenant's active IVR menus (codes, greetings, key
/// trees) and active auto-response scripts (triggers and
/// templates), so the model answers from the stored
/// configuration. The payload carries `tenant_id` under the
/// input the auto-response handler passed to
/// `run_ai_capability`. A payload naming no tenant is
/// skipped, not failed: the runner then proceeds with the
/// caller's input alone.
pub(crate) fn on_ai_answer_requested(state: PhoneState, ctx: &mut HookContext) {
    let input = ctx.get("input").cloned().unwrap_or(json!({}));
    let Some(tenant_id) = input
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(
            json!({ "phone": "skipped", "reason": "answer grounding carries no tenant_id" }),
        );
        return;
    };
    let menus: Vec<serde_json::Value> = state
        .ivr_menus
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|menu| menu.tenant_id == tenant_id && menu.active)
        .map(|menu| {
            let nodes: Vec<serde_json::Value> = state
                .menu_nodes(menu.id)
                .iter()
                .map(|node| {
                    json!({
                        "key": node.key,
                        "action": node.action.as_str(),
                        "target_id": node.target_id,
                        "message": node.message,
                    })
                })
                .collect();
            json!({
                "code": menu.code,
                "label": menu.label,
                "greeting": menu.greeting,
                "nodes": nodes,
            })
        })
        .collect();
    let scripts: Vec<serde_json::Value> = state
        .active_scripts(&tenant_id)
        .iter()
        .map(|script| {
            json!({
                "code": script.code,
                "label": script.label,
                "trigger": script.trigger,
                "response_template": script.response_template,
            })
        })
        .collect();
    ctx.add_result(json!({
        "tenant_id": tenant_id,
        "ivr_menus": menus,
        "auto_response_scripts": scripts,
        "rules": [
            "Answer only routine caller questions from the menus and scripts above.",
            "You may suggest a route (a queue code, a menu code, or escalation to a human agent) but you must never execute a transfer, booking, or charge yourself.",
            "When no menu or script answers the question, say so and suggest escalation to a human agent.",
        ],
    }));
}

/// The grounding event name for the answer capability, for
/// registration in `module.rs`.
pub(crate) fn ai_answer_event() -> &'static str {
    EVENT_AI_ANSWER_REQUESTED
}

/// Answer `phone.ai.summarize_call_requested` with the
/// grounding for the summary run: the call's stored facts
/// (kind, status, outcome, duration, subject, participant
/// ids, started / ended times) plus the caller-supplied
/// transcript when the summarize handler passed one. A
/// payload naming no call, or a call this module cannot find,
/// is skipped, not failed.
pub(crate) fn on_ai_summarize_requested(state: PhoneState, ctx: &mut HookContext) {
    let input = ctx.get("input").cloned().unwrap_or(json!({}));
    let call_id = input
        .get("call_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(call_id) = call_id else {
        ctx.add_result(
            json!({ "phone": "skipped", "reason": "summary grounding carries no call_id" }),
        );
        return;
    };
    let Some(call) = state.calls.get(&call_id).map(|entry| entry.value().clone()) else {
        ctx.add_result(json!({ "phone": "skipped", "call_id": call_id.to_string(), "reason": "no phone call with that id" }));
        return;
    };
    ctx.add_result(json!({
        "call_id": call.id.to_string(),
        "tenant_id": call.tenant_id,
        "caller_id": call.caller_id,
        "kind": call.kind.as_str(),
        "status": call.status.as_str(),
        "outcome": call.outcome.map(|outcome| outcome.as_str()),
        "duration_secs": call.duration_secs,
        "subject": call.subject,
        "participant_ids": state.call_participants(call.id).iter().map(|participant| participant.user_id.clone()).collect::<Vec<String>>(),
        "started_at": call.started_at.map(|time| time.to_rfc3339()),
        "ended_at": call.ended_at.map(|time| time.to_rfc3339()),
        "transcript": input.get("transcript"),
    }));
}

/// The grounding event name for the summary capability, for
/// registration in `module.rs`.
pub(crate) fn ai_summarize_event() -> &'static str {
    EVENT_AI_SUMMARIZE_REQUESTED
}
