//! HTTP handlers for calls: initiate, list, read, update,
//! accept, reject, end, hold, resume, invite, participants,
//! and the call log (ended and missed calls).

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    call_event, call_view, caller, can_manage, created, err, load_actionable_call,
    load_queue_by_code, load_visible_call, ok, optional_text, paginate, require_user_id,
    resolve_tenant,
};
use super::state::PhoneState;
use super::types::{
    can_transition, compute_duration_secs, CallKind, CallOutcome, CallParticipant, CallStatus,
    EndCallRequest, InitiateCallRequest, InviteParticipantRequest, ParticipantStatus,
    UpdateCallRequest, EVENT_CALL_ANSWERED, EVENT_CALL_ENDED, EVENT_CALL_HELD,
    EVENT_CALL_INITIATED, EVENT_CALL_MISSED, EVENT_CALL_RESUMED, MAX_LABEL_LEN,
};

/// Path carrying a call id.
#[derive(Debug, Deserialize)]
struct CallPath {
    call_id: String,
}

/// Resolve the callee list of an initiate request: either one
/// `callee_id` or a `callee_ids` list. Duplicates and the
/// caller themselves are rejected.
fn resolve_callees(input: &InitiateCallRequest, caller_id: &str) -> UnifiedResult<Vec<String>> {
    let mut callees: Vec<String> = Vec::new();
    if let Some(raw) = &input.callee_id {
        callees.push(require_user_id(raw)?);
    }
    for raw in &input.callee_ids {
        callees.push(require_user_id(raw)?);
    }
    if callees.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one callee is required (callee_id or callee_ids)",
        ));
    }
    let mut unique: Vec<String> = Vec::with_capacity(callees.len());
    for callee in callees {
        if callee == caller_id {
            return Err(UnifiedError::bad_request(
                "A call cannot include its own caller as a callee",
            ));
        }
        if unique.contains(&callee) {
            return Err(UnifiedError::bad_request(
                "callee_ids must not contain duplicates",
            ));
        }
        unique.push(callee);
    }
    Ok(unique)
}

fn list_visible_calls(
    state: &PhoneState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &super::helpers::Caller,
    params: &PaginationParams,
) -> Vec<Value> {
    let is_manager = can_manage(ctx, &caller.role);
    let status_filter = params
        .filters
        .get("status")
        .and_then(|raw| CallStatus::from_str(raw));
    let kind_filter = params
        .filters
        .get("kind")
        .and_then(|raw| CallKind::from_str(raw));
    let user_filter = params.filters.get("user").cloned();
    let queue_filter = params.filters.get("queue_id").cloned();
    let from_filter = params.filters.get("from").cloned();
    let to_filter = params.filters.get("to").cloned();
    let search = params.search.clone().map(|value| value.to_lowercase());
    let mut items: Vec<super::types::PhoneCall> = state
        .calls
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|call| {
            if call.tenant_id != tenant_id {
                return false;
            }
            if !is_manager {
                let is_participant = state
                    .call_participants(call.id)
                    .iter()
                    .any(|participant| participant.user_id == caller.actor);
                if call.caller_id != caller.actor && !is_participant {
                    return false;
                }
            }
            if let Some(status) = status_filter {
                if call.status != status {
                    return false;
                }
            }
            if let Some(kind) = kind_filter {
                if call.kind != kind {
                    return false;
                }
            }
            if let Some(user) = &user_filter {
                let is_participant = state
                    .call_participants(call.id)
                    .iter()
                    .any(|participant| participant.user_id == *user);
                if call.caller_id != *user && !is_participant {
                    return false;
                }
            }
            if let Some(queue_id) = &queue_filter {
                if call.queue_id.map(|id| id.to_string()) != Some(queue_id.clone()) {
                    return false;
                }
            }
            if let Some(from) = &from_filter {
                if call.created_at.to_rfc3339() < *from {
                    return false;
                }
            }
            if let Some(to) = &to_filter {
                if call.created_at.to_rfc3339() > *to {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !call.caller_id.to_lowercase().contains(needle.as_str())
                    && !call
                        .subject
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(needle.as_str())
                {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    items.iter().map(|call| call_view(state, call)).collect()
}

/// Mark every still-ringing participant of a call as missed
/// and close the call as missed (used when the call ends
/// without ever being answered).
fn mark_missed(state: &PhoneState, call: &mut super::types::PhoneCall, now: chrono::DateTime<Utc>) {
    call.status = CallStatus::Missed;
    call.outcome = Some(CallOutcome::Missed);
    call.ended_at = Some(now);
    for mut entry in state.participants.iter_mut() {
        if entry.value().call_id == call.id
            && matches!(
                entry.value().status,
                ParticipantStatus::Invited | ParticipantStatus::Ringing
            )
        {
            entry.value_mut().status = ParticipantStatus::Missed;
            entry.value_mut().updated_at = now;
        }
    }
}

// Call handlers (authenticated)

pub(crate) async fn list_calls(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let views = list_visible_calls(&state, &ctx, &tenant_id, &caller, &params);
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn initiate_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<InitiateCallRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let callees = resolve_callees(&input, &caller.actor)?;
        let settings = state.current_settings(&tenant_id);
        if callees.len() > 1 && !settings.allow_group_calls {
            return Err(UnifiedError::bad_request(
                "Group calls are disabled for this tenant",
            ));
        }
        if (callees.len() as i64) + 1 > settings.max_group_participants {
            return Err(UnifiedError::bad_request(format!(
                "Group calls hold at most {} participants",
                settings.max_group_participants
            )));
        }
        let kind = match input.kind.as_deref() {
            None | Some("voice") => CallKind::Voice,
            Some("video") => CallKind::Video,
            Some(other) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown call kind: {other}"
                )))
            }
        };
        let subject = optional_text(&input.subject, "subject", MAX_LABEL_LEN)?;
        let queue = match &input.queue_code {
            Some(code) => Some(load_queue_by_code(&state, &tenant_id, code)?),
            None => None,
        };
        if let Some(queue) = &queue {
            if !queue.active {
                return Err(UnifiedError::bad_request(format!(
                    "Queue \"{}\" is not active",
                    queue.code
                )));
            }
        }
        let ivr_menu = match &input.ivr_menu_code {
            Some(code) => Some(super::helpers::load_menu_by_code(&state, &tenant_id, code)?),
            None => None,
        };
        if let Some(menu) = &ivr_menu {
            if !menu.active {
                return Err(UnifiedError::bad_request(format!(
                    "IVR menu \"{}\" is not active",
                    menu.code
                )));
            }
        }
        let now = Utc::now();
        let call = super::types::PhoneCall {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            caller_id: caller.actor.clone(),
            kind,
            status: CallStatus::Ringing,
            outcome: None,
            queue_id: queue.as_ref().map(|queue| queue.id),
            ivr_menu_id: ivr_menu.as_ref().map(|menu| menu.id),
            subject,
            recording_consent: input.recording_consent,
            recording_consent_by: if input.recording_consent {
                Some(caller.actor.clone())
            } else {
                None
            },
            started_at: None,
            ended_at: None,
            duration_secs: None,
            summary: None,
            summary_model: None,
            created_at: now,
            updated_at: now,
        };
        state.calls.insert(call.id, call.clone());
        let caller_participant = CallParticipant {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            call_id: call.id,
            user_id: caller.actor.clone(),
            direction: "outbound".to_string(),
            status: ParticipantStatus::Joined,
            joined_at: Some(now),
            left_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .participants
            .insert(caller_participant.id, caller_participant);
        for callee in &callees {
            let participant = CallParticipant {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.clone(),
                call_id: call.id,
                user_id: callee.clone(),
                direction: "inbound".to_string(),
                status: ParticipantStatus::Ringing,
                joined_at: None,
                left_at: None,
                created_at: now,
                updated_at: now,
            };
            state.participants.insert(participant.id, participant);
            state.notify(
                "phone_call",
                callee,
                "Incoming call",
                &format!("{} is calling you ({})", caller.actor, kind.as_str()),
                None,
            );
        }
        call_event(
            &state,
            EVENT_CALL_INITIATED,
            &call,
            &caller.actor,
            Some(format!("{} callee(s)", callees.len())),
        );
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    body: web::Json<UpdateCallRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let mut call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if call.caller_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Call not found: {}",
                call.id
            )));
        }
        if call.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "An ended or missed call cannot be updated",
            ));
        }
        if let Some(subject) = &input.subject {
            call.subject = optional_text(subject, "subject", MAX_LABEL_LEN)?;
        }
        if let Some(consent) = input.recording_consent {
            call.recording_consent = consent;
            call.recording_consent_by = if consent {
                Some(caller.actor.clone())
            } else {
                None
            };
        }
        call.updated_at = Utc::now();
        state.calls.insert(call.id, call.clone());
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if call.caller_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Call not found: {}",
                call.id
            )));
        }
        if !call.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "Only an ended or missed call can be deleted; end the call first",
            ));
        }
        state.calls.remove(&call.id);
        let participant_ids: Vec<Uuid> = state
            .call_participants(call.id)
            .iter()
            .map(|participant| participant.id)
            .collect();
        for id in participant_ids {
            state.participants.remove(&id);
        }
        let signal_ids: Vec<Uuid> = state
            .call_signals(call.id)
            .iter()
            .map(|signal| signal.id)
            .collect();
        for id in signal_ids {
            state.signals.remove(&id);
        }
        let recording_ids: Vec<Uuid> = state
            .call_recordings(call.id)
            .iter()
            .map(|recording| recording.id)
            .collect();
        for id in recording_ids {
            state.recordings.remove(&id);
        }
        Ok(json!({ "deleted": call.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn accept_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut call = load_actionable_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if !can_transition(call.status, CallStatus::Active) {
            return Err(UnifiedError::conflict(format!(
                "A {} call cannot be accepted",
                call.status.as_str()
            )));
        }
        let mut participant = super::helpers::load_participant(&state, call.id, &caller.actor)?;
        if !matches!(
            participant.status,
            ParticipantStatus::Ringing | ParticipantStatus::Invited
        ) {
            return Err(UnifiedError::conflict(format!(
                "Participant status {} cannot accept this call",
                participant.status.as_str()
            )));
        }
        let now = Utc::now();
        participant.status = ParticipantStatus::Joined;
        participant.joined_at = Some(now);
        participant.updated_at = now;
        state.participants.insert(participant.id, participant);
        call.status = CallStatus::Active;
        call.started_at = Some(now);
        call.updated_at = now;
        state.calls.insert(call.id, call.clone());
        call_event(&state, EVENT_CALL_ANSWERED, &call, &caller.actor, None);
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut call = load_actionable_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if call.status != CallStatus::Ringing {
            return Err(UnifiedError::conflict(format!(
                "Only a ringing call can be rejected (now {})",
                call.status.as_str()
            )));
        }
        let mut participant = super::helpers::load_participant(&state, call.id, &caller.actor)?;
        if participant.user_id == call.caller_id {
            return Err(UnifiedError::bad_request(
                "The caller cannot reject their own call; end it instead",
            ));
        }
        let now = Utc::now();
        participant.status = ParticipantStatus::Declined;
        participant.updated_at = now;
        state.participants.insert(participant.id, participant);
        // In a 1:1 call a decline ends the call as declined; in a
        // group call the call keeps ringing for the rest.
        let still_ringing = state.call_participants(call.id).iter().any(|p| {
            matches!(
                p.status,
                ParticipantStatus::Ringing | ParticipantStatus::Invited
            )
        });
        if !still_ringing {
            call.status = CallStatus::Ended;
            call.outcome = Some(CallOutcome::Declined);
            call.ended_at = Some(now);
            call.updated_at = now;
            state.calls.insert(call.id, call.clone());
            call_event(
                &state,
                EVENT_CALL_ENDED,
                &call,
                &caller.actor,
                Some("declined".to_string()),
            );
        }
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn end_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    body: web::Json<EndCallRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let reason = optional_text(&input.reason, "reason", MAX_LABEL_LEN)?;
        let mut call = load_actionable_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if call.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Call is already {}",
                call.status.as_str()
            )));
        }
        let now = Utc::now();
        let was_ringing = call.status == CallStatus::Ringing;
        if was_ringing {
            // Nobody answered: the call is missed, not completed.
            mark_missed(&state, &mut call, now);
            call.updated_at = now;
            state.calls.insert(call.id, call.clone());
            call_event(
                &state,
                EVENT_CALL_MISSED,
                &call,
                &caller.actor,
                reason.clone(),
            );
        } else {
            if !can_transition(call.status, CallStatus::Ended) {
                return Err(UnifiedError::conflict(format!(
                    "A {} call cannot be ended",
                    call.status.as_str()
                )));
            }
            call.status = CallStatus::Ended;
            call.outcome = Some(CallOutcome::Completed);
            call.ended_at = Some(now);
            call.duration_secs = compute_duration_secs(call.started_at, call.ended_at);
            call.updated_at = now;
            for mut entry in state.participants.iter_mut() {
                if entry.value().call_id == call.id
                    && entry.value().status == ParticipantStatus::Joined
                {
                    entry.value_mut().status = ParticipantStatus::Left;
                    entry.value_mut().left_at = Some(now);
                    entry.value_mut().updated_at = now;
                }
            }
            state.calls.insert(call.id, call.clone());
            call_event(&state, EVENT_CALL_ENDED, &call, &caller.actor, reason);
        }
        // A recording left running is stopped with the call;
        // its bytes are attached later through the attach
        // endpoint.
        super::handlers_recordings::stop_open_recordings(&state, call.id);
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn hold_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut call = load_actionable_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if !can_transition(call.status, CallStatus::Held) {
            return Err(UnifiedError::conflict(format!(
                "A {} call cannot be held",
                call.status.as_str()
            )));
        }
        call.status = CallStatus::Held;
        call.updated_at = Utc::now();
        state.calls.insert(call.id, call.clone());
        call_event(&state, EVENT_CALL_HELD, &call, &caller.actor, None);
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn resume_call(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut call = load_actionable_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if !can_transition(call.status, CallStatus::Active) {
            return Err(UnifiedError::conflict(format!(
                "A {} call cannot be resumed",
                call.status.as_str()
            )));
        }
        call.status = CallStatus::Active;
        call.updated_at = Utc::now();
        state.calls.insert(call.id, call.clone());
        call_event(&state, EVENT_CALL_RESUMED, &call, &caller.actor, None);
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn invite_participant(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    body: web::Json<InviteParticipantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let user_id = require_user_id(&input.user_id)?;
        let call = load_actionable_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        if !matches!(
            call.status,
            CallStatus::Ringing | CallStatus::Active | CallStatus::Held
        ) {
            return Err(UnifiedError::conflict(format!(
                "Cannot invite into a {} call",
                call.status.as_str()
            )));
        }
        let settings = state.current_settings(&tenant_id);
        if !settings.allow_group_calls {
            return Err(UnifiedError::bad_request(
                "Group calls are disabled for this tenant",
            ));
        }
        if state.call_participants(call.id).len() as i64 >= settings.max_group_participants {
            return Err(UnifiedError::bad_request(format!(
                "Group calls hold at most {} participants",
                settings.max_group_participants
            )));
        }
        if state
            .call_participants(call.id)
            .iter()
            .any(|participant| participant.user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already a participant of this call"
            )));
        }
        let now = Utc::now();
        let participant = CallParticipant {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            call_id: call.id,
            user_id: user_id.clone(),
            direction: "inbound".to_string(),
            status: ParticipantStatus::Ringing,
            joined_at: None,
            left_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .participants
            .insert(participant.id, participant.clone());
        state.notify(
            "phone_call",
            &user_id,
            "Incoming call",
            &format!("{} invited you to a call", caller.actor),
            None,
        );
        Ok(json!(participant))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_participants(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        let status_filter = params.filters.get("status").cloned();
        let mut items = state.call_participants(call.id);
        if let Some(status) = &status_filter {
            items.retain(|participant| participant.status.as_str() == status);
        }
        let views: Vec<Value> = items
            .iter()
            .map(|participant| serde_json::to_value(participant).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Call log: ended and missed calls only, with the caller /
/// callee, started / ended times, duration, direction (from
/// the caller's point of view), and outcome. Filter by user,
/// status, outcome, kind, and date range.
pub(crate) async fn list_call_logs(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let is_manager = can_manage(&ctx, &caller.role);
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| CallStatus::from_str(raw));
        let outcome_filter = params
            .filters
            .get("outcome")
            .and_then(|raw| CallOutcome::from_str(raw));
        let user_filter = params.filters.get("user").cloned();
        let from_filter = params.filters.get("from").cloned();
        let to_filter = params.filters.get("to").cloned();
        let mut items: Vec<super::types::PhoneCall> = state
            .calls
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|call| {
                if call.tenant_id != tenant_id {
                    return false;
                }
                if !call.status.is_terminal() {
                    return false;
                }
                if !is_manager {
                    let is_participant = state
                        .call_participants(call.id)
                        .iter()
                        .any(|participant| participant.user_id == caller.actor);
                    if call.caller_id != caller.actor && !is_participant {
                        return false;
                    }
                }
                if let Some(status) = status_filter {
                    if call.status != status {
                        return false;
                    }
                }
                if let Some(outcome) = outcome_filter {
                    if call.outcome != Some(outcome) {
                        return false;
                    }
                }
                if let Some(user) = &user_filter {
                    let is_participant = state
                        .call_participants(call.id)
                        .iter()
                        .any(|participant| participant.user_id == *user);
                    if call.caller_id != *user && !is_participant {
                        return false;
                    }
                }
                if let Some(from) = &from_filter {
                    if call.created_at.to_rfc3339() < *from {
                        return false;
                    }
                }
                if let Some(to) = &to_filter {
                    if call.created_at.to_rfc3339() > *to {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = items
            .iter()
            .map(|call| {
                let mut view = call_view(&state, call);
                if let Value::Object(map) = &mut view {
                    let direction = if call.caller_id == caller.actor {
                        "outbound"
                    } else {
                        "inbound"
                    };
                    map.insert(
                        "direction".to_string(),
                        Value::String(direction.to_string()),
                    );
                }
                view
            })
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Read the AI summary stored on a call log entry.
pub(crate) async fn get_call_summary(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        Ok(json!({
            "call_id": call.id.to_string(),
            "status": call.status.as_str(),
            "summary": call.summary,
            "summary_model": call.summary_model,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
