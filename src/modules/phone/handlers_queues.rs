//! HTTP handlers for call queues (as data), queue members
//! and waiters, routing rules (as data), and the admin queue
//! dashboard.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Timelike, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    call_event, call_view, caller, can_manage, created, err, load_member, load_queue,
    load_queue_by_code, load_rule, ok, paginate, queue_event, require_code, require_description,
    require_label, require_permission, require_text, require_time_of_day, require_user_id,
    resolve_tenant,
};
use super::state::PhoneState;
use super::types::{
    evaluate_routing_rules, next_ring_targets, AddQueueMemberRequest, CallKind, CallParticipant,
    CallStatus, CreateQueueRequest, CreateRoutingRuleRequest, EvaluateRoutingRequest,
    ParticipantStatus, PhoneCall, PhoneQueue, QueueMember, QueueWaiter, RingStrategy, RoutingRule,
    UpdateQueueRequest, UpdateRoutingRuleRequest, EVENT_CALL_INITIATED, EVENT_QUEUE_ANSWERED,
    EVENT_QUEUE_JOINED, MAX_LABEL_LEN, MAX_USER_ID_LEN, PERM_PHONE_MANAGE,
};

/// Path carrying a queue id.
#[derive(Debug, Deserialize)]
struct QueuePath {
    queue_id: String,
}

/// Path carrying a queue id and a member id.
#[derive(Debug, Deserialize)]
struct MemberPath {
    queue_id: String,
    member_id: String,
}

/// Path carrying a routing rule id.
#[derive(Debug, Deserialize)]
struct RulePath {
    rule_id: String,
}

fn require_ring_strategy(raw: &Option<String>) -> UnifiedResult<RingStrategy> {
    match raw.as_deref() {
        None => Ok(RingStrategy::default()),
        Some(value) => RingStrategy::from_str(value).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown ring strategy: {value} (expected sequential, simultaneous, or round_robin)"
            ))
        }),
    }
}

fn require_target_type(raw: &str) -> UnifiedResult<String> {
    match raw {
        "queue" | "user" | "ivr_menu" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown routing target type: {other} (expected queue, user, or ivr_menu)"
        ))),
    }
}

fn validate_target(
    state: &PhoneState,
    tenant_id: &str,
    target_type: &str,
    target_id: &str,
) -> UnifiedResult<()> {
    match target_type {
        "queue" => {
            load_queue_by_code(state, tenant_id, target_id)?;
        }
        "ivr_menu" => {
            super::helpers::load_menu_by_code(state, tenant_id, target_id)?;
        }
        "user" => {
            require_user_id(target_id)?;
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown routing target type: {target_type}"
            )));
        }
    }
    Ok(())
}

fn queue_view(state: &PhoneState, queue: &PhoneQueue) -> Value {
    let mut value = serde_json::to_value(queue).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let members = state.queue_members(queue.id);
        map.insert(
            "members".to_string(),
            serde_json::to_value(members).unwrap_or(Value::Null),
        );
        map.insert(
            "waiting_count".to_string(),
            Value::from(state.queue_waiting(queue.id).len() as u64),
        );
        map.insert(
            "ring_order".to_string(),
            serde_json::to_value(next_ring_targets(
                &state.queue_members(queue.id),
                queue.ring_strategy,
            ))
            .unwrap_or(Value::Null),
        );
    }
    value
}

// Queue handlers (manager for writes, authenticated for reads)

pub(crate) async fn list_queues(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").map(|raw| raw == "true");
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<PhoneQueue> = state
            .queues
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|queue| {
                if queue.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active_filter {
                    if queue.active != active {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !queue.code.to_lowercase().contains(needle.as_str())
                        && !queue.label.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        let views: Vec<Value> = items
            .iter()
            .map(|queue| queue_view(&state, queue))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_queue(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateQueueRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let description = require_description(&input.description)?;
        let ring_strategy = require_ring_strategy(&input.ring_strategy)?;
        let settings = state.current_settings(&tenant_id);
        let ring_timeout_secs = input
            .ring_timeout_secs
            .unwrap_or(settings.default_ring_timeout_secs);
        if ring_timeout_secs < 1 || ring_timeout_secs > 600 {
            return Err(UnifiedError::bad_request(
                "ring_timeout_secs must be between 1 and 600",
            ));
        }
        let max_wait_secs = input.max_wait_secs.unwrap_or(300);
        if max_wait_secs < 1 || max_wait_secs > 3600 {
            return Err(UnifiedError::bad_request(
                "max_wait_secs must be between 1 and 3600",
            ));
        }
        if state.queue_by_code(&tenant_id, &code).is_some() {
            return Err(UnifiedError::conflict(format!(
                "Queue code \"{code}\" already exists for this tenant"
            )));
        }
        if let Some(overflow_id) = input.overflow_queue_id {
            let overflow = state
                .queues
                .get(&overflow_id)
                .map(|entry| entry.value().clone());
            match overflow {
                Some(queue) if queue.tenant_id == tenant_id => {}
                _ => {
                    return Err(UnifiedError::not_found(format!(
                        "Overflow queue not found: {overflow_id}"
                    )));
                }
            }
        }
        let overflow_action = match &input.overflow_action {
            Some(raw) => Some(require_text(raw, "overflow_action", MAX_LABEL_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let queue = PhoneQueue {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            description,
            ring_strategy,
            ring_timeout_secs,
            max_wait_secs,
            overflow_queue_id: input.overflow_queue_id,
            overflow_action,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.queues.insert(queue.id, queue.clone());
        Ok(queue_view(&state, &queue))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_queue(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        Ok(queue_view(&state, &queue))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_queue(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
    body: web::Json<UpdateQueueRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let mut queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        if let Some(label) = &input.label {
            queue.label = require_label(label)?;
        }
        if let Some(description) = &input.description {
            queue.description = require_description(description)?;
        }
        if let Some(strategy) = &input.ring_strategy {
            queue.ring_strategy = require_ring_strategy(&Some(strategy.clone()))?;
        }
        if let Some(timeout) = input.ring_timeout_secs {
            if !(1..=600).contains(&timeout) {
                return Err(UnifiedError::bad_request(
                    "ring_timeout_secs must be between 1 and 600",
                ));
            }
            queue.ring_timeout_secs = timeout;
        }
        if let Some(max_wait) = input.max_wait_secs {
            if !(1..=3600).contains(&max_wait) {
                return Err(UnifiedError::bad_request(
                    "max_wait_secs must be between 1 and 3600",
                ));
            }
            queue.max_wait_secs = max_wait;
        }
        if let Some(overflow) = &input.overflow_queue_id {
            match overflow {
                Some(id) => {
                    let target = state.queues.get(id).map(|entry| entry.value().clone());
                    match target {
                        Some(target) if target.tenant_id == tenant_id => {
                            if *id == queue.id {
                                return Err(UnifiedError::bad_request(
                                    "A queue cannot overflow into itself",
                                ));
                            }
                            queue.overflow_queue_id = Some(*id);
                        }
                        _ => {
                            return Err(UnifiedError::not_found(format!(
                                "Overflow queue not found: {id}"
                            )));
                        }
                    }
                }
                None => queue.overflow_queue_id = None,
            }
        }
        if let Some(action) = &input.overflow_action {
            queue.overflow_action = match action {
                Some(raw) => Some(require_text(raw, "overflow_action", MAX_LABEL_LEN)?),
                None => None,
            };
        }
        if let Some(active) = input.active {
            queue.active = active;
        }
        queue.updated_at = Utc::now();
        state.queues.insert(queue.id, queue.clone());
        Ok(queue_view(&state, &queue))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_queue(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        if !state.queue_waiting(queue.id).is_empty() {
            return Err(UnifiedError::conflict(
                "A queue with waiting callers cannot be deleted",
            ));
        }
        if state.calls.iter().any(|entry| {
            entry.value().queue_id == Some(queue.id) && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "A queue with live calls cannot be deleted",
            ));
        }
        state.queues.remove(&queue.id);
        let member_ids: Vec<Uuid> = state
            .queue_members(queue.id)
            .iter()
            .map(|member| member.id)
            .collect();
        for id in member_ids {
            state.queue_members.remove(&id);
        }
        Ok(json!({ "deleted": queue.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Queue members

pub(crate) async fn list_queue_members(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        let active_filter = params.filters.get("active").map(|raw| raw == "true");
        let mut items = state.queue_members(queue.id);
        if let Some(active) = active_filter {
            items.retain(|member| member.active == active);
        }
        let views: Vec<Value> = items
            .iter()
            .map(|member| serde_json::to_value(member).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_queue_member(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
    body: web::Json<AddQueueMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        let user_id = require_user_id(&input.user_id)?;
        if state
            .queue_members(queue.id)
            .iter()
            .any(|member| member.user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already a member of this queue"
            )));
        }
        let position = match input.position {
            Some(position) if position >= 0 => position,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "position must be zero or greater",
                ));
            }
            None => {
                state
                    .queue_members(queue.id)
                    .iter()
                    .map(|member| member.position)
                    .max()
                    .unwrap_or(-1)
                    + 1
            }
        };
        let now = Utc::now();
        let member = QueueMember {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            queue_id: queue.id,
            user_id,
            position,
            active: input.active,
            last_answered_at: None,
            created_at: now,
            updated_at: now,
        };
        state.queue_members.insert(member.id, member.clone());
        Ok(json!(member))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn remove_queue_member(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MemberPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        let member = load_member(&state, &tenant_id, &path.member_id)?;
        if member.queue_id != queue.id {
            return Err(UnifiedError::not_found(format!(
                "Queue member not found: {}",
                member.id
            )));
        }
        state.queue_members.remove(&member.id);
        Ok(json!({ "deleted": member.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Queue waiting room and admin answering

/// A caller joins a queue as a waiting caller. The caller waits
/// until an admin answers the queue (which creates a call
/// between the admin and the caller) or the wait expires
/// (`max_wait_secs`, enforced by the caller / a future
/// scheduler pass - this version records the join time and the
/// overflow target and leaves expiry to the dashboard).
pub(crate) async fn join_queue(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        if !queue.active {
            return Err(UnifiedError::bad_request(format!(
                "Queue \"{}\" is not active",
                queue.code
            )));
        }
        if state
            .queue_waiting(queue.id)
            .iter()
            .any(|waiter| waiter.caller_id == caller.actor)
        {
            return Err(UnifiedError::conflict(
                "The caller is already waiting in this queue",
            ));
        }
        let now = Utc::now();
        let waiter = QueueWaiter {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            queue_id: queue.id,
            caller_id: caller.actor.clone(),
            status: "waiting".to_string(),
            call_id: None,
            joined_at: now,
            answered_at: None,
        };
        state.queue_waiters.insert(waiter.id, waiter.clone());
        queue_event(
            &state,
            EVENT_QUEUE_JOINED,
            &queue,
            &caller.actor,
            Some(format!("waiter {}", waiter.id)),
        );
        Ok(json!(waiter))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_queue_waiting(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        if !can_manage(&ctx, &caller.role) {
            // A waiting caller sees only their own place in line.
            let mine: Vec<Value> = state
                .queue_waiting(queue.id)
                .iter()
                .filter(|waiter| waiter.caller_id == caller.actor)
                .map(|waiter| serde_json::to_value(waiter).unwrap_or(Value::Null))
                .collect();
            return Ok(paginate(mine, &params).to_json());
        }
        let views: Vec<Value> = state
            .queue_waiting(queue.id)
            .iter()
            .map(|waiter| serde_json::to_value(waiter).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// An admin answers the next waiting caller: the oldest waiter
/// becomes a ringing call between the admin and the caller, the
/// waiter is marked answered, and the answering admin's queue
/// member row records the answer time (driving round-robin).
pub(crate) async fn answer_queue(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<QueuePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let queue = load_queue(&state, &tenant_id, &path.queue_id)?;
        let waiting = state.queue_waiting(queue.id);
        let Some(waiter) = waiting.first().cloned() else {
            return Err(UnifiedError::not_found(
                "No callers are waiting in this queue",
            ));
        };
        let now = Utc::now();
        let call = PhoneCall {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            caller_id: caller.actor.clone(),
            kind: CallKind::Voice,
            status: CallStatus::Ringing,
            outcome: None,
            queue_id: Some(queue.id),
            ivr_menu_id: None,
            subject: Some(format!("Queue call from {}", queue.code)),
            recording_consent: false,
            recording_consent_by: None,
            started_at: None,
            ended_at: None,
            duration_secs: None,
            summary: None,
            summary_model: None,
            created_at: now,
            updated_at: now,
        };
        state.calls.insert(call.id, call.clone());
        let admin_participant = CallParticipant {
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
            .insert(admin_participant.id, admin_participant);
        let caller_participant = CallParticipant {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            call_id: call.id,
            user_id: waiter.caller_id.clone(),
            direction: "inbound".to_string(),
            status: ParticipantStatus::Ringing,
            joined_at: None,
            left_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .participants
            .insert(caller_participant.id, caller_participant);
        let mut updated_waiter = waiter.clone();
        updated_waiter.status = "answered".to_string();
        updated_waiter.call_id = Some(call.id);
        updated_waiter.answered_at = Some(now);
        state
            .queue_waiters
            .insert(updated_waiter.id, updated_waiter);
        for mut entry in state.queue_members.iter_mut() {
            if entry.value().queue_id == queue.id && entry.value().user_id == caller.actor {
                entry.value_mut().last_answered_at = Some(now);
                entry.value_mut().updated_at = now;
            }
        }
        queue_event(
            &state,
            EVENT_QUEUE_ANSWERED,
            &queue,
            &caller.actor,
            Some(format!("call {}", call.id)),
        );
        call_event(&state, EVENT_CALL_INITIATED, &call, &caller.actor, None);
        // The admin created the call and is already joined; the
        // waiting caller answers it through the normal accept
        // endpoint.
        Ok(call_view(&state, &call))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Admin queue dashboard: every queue with its waiting
/// callers, member counts, and ring order (manager only).
pub(crate) async fn admin_queue_dashboard(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let params = query.into_inner();
        let mut items: Vec<PhoneQueue> = state
            .queues
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|queue| queue.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        let views: Vec<Value> = items
            .iter()
            .map(|queue| {
                let mut view = queue_view(&state, queue);
                if let Value::Object(map) = &mut view {
                    let waiting = state.queue_waiting(queue.id);
                    map.insert(
                        "waiting".to_string(),
                        serde_json::to_value(waiting).unwrap_or(Value::Null),
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

// Routing rules (manager)

pub(crate) async fn list_routing_rules(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").map(|raw| raw == "true");
        let target_filter = params.filters.get("target_type").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<RoutingRule> = state
            .routing_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                if rule.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active_filter {
                    if rule.active != active {
                        return false;
                    }
                }
                if let Some(target) = &target_filter {
                    if rule.target_type != *target {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !rule.code.to_lowercase().contains(needle.as_str())
                        && !rule.label.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|rule| rule.priority);
        let views: Vec<Value> = items
            .iter()
            .map(|rule| serde_json::to_value(rule).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_routing_rule(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateRoutingRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let description = require_description(&input.description)?;
        let target_type = require_target_type(&input.target_type)?;
        let target_id = require_text(&input.target_id, "target_id", MAX_USER_ID_LEN)?;
        validate_target(&state, &tenant_id, &target_type, &target_id)?;
        let match_time_start = match &input.match_time_start {
            Some(raw) => Some(require_time_of_day(raw, "match_time_start")?),
            None => None,
        };
        let match_time_end = match &input.match_time_end {
            Some(raw) => Some(require_time_of_day(raw, "match_time_end")?),
            None => None,
        };
        let match_caller_id = match &input.match_caller_id {
            Some(raw) if !raw.trim().is_empty() => Some(raw.trim().to_string()),
            _ => None,
        };
        if state
            .routing_rules
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Routing rule code \"{code}\" already exists for this tenant"
            )));
        }
        let now = Utc::now();
        let rule = RoutingRule {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            description,
            priority: input.priority,
            match_caller_id,
            match_time_start,
            match_time_end,
            target_type,
            target_id,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.routing_rules.insert(rule.id, rule.clone());
        Ok(json!(rule))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_routing_rule(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RulePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let rule = load_rule(&state, &tenant_id, &path.rule_id)?;
        Ok(json!(rule))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_routing_rule(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RulePath>,
    body: web::Json<UpdateRoutingRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let mut rule = load_rule(&state, &tenant_id, &path.rule_id)?;
        if let Some(label) = &input.label {
            rule.label = require_label(label)?;
        }
        if let Some(description) = &input.description {
            rule.description = require_description(description)?;
        }
        if let Some(priority) = input.priority {
            rule.priority = priority;
        }
        if let Some(pattern) = &input.match_caller_id {
            rule.match_caller_id = match pattern {
                Some(raw) if !raw.trim().is_empty() => Some(raw.trim().to_string()),
                _ => None,
            };
        }
        if let Some(start) = &input.match_time_start {
            rule.match_time_start = match start {
                Some(raw) => Some(require_time_of_day(raw, "match_time_start")?),
                None => None,
            };
        }
        if let Some(end) = &input.match_time_end {
            rule.match_time_end = match end {
                Some(raw) => Some(require_time_of_day(raw, "match_time_end")?),
                None => None,
            };
        }
        if let Some(target_type) = &input.target_type {
            rule.target_type = require_target_type(target_type)?;
        }
        if let Some(target_id) = &input.target_id {
            rule.target_id = require_text(target_id, "target_id", MAX_USER_ID_LEN)?;
        }
        if let Some(active) = input.active {
            rule.active = active;
        }
        validate_target(&state, &tenant_id, &rule.target_type, &rule.target_id)?;
        rule.updated_at = Utc::now();
        state.routing_rules.insert(rule.id, rule.clone());
        Ok(json!(rule))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_routing_rule(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RulePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let rule = load_rule(&state, &tenant_id, &path.rule_id)?;
        state.routing_rules.remove(&rule.id);
        Ok(json!({ "deleted": rule.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Evaluate the tenant's routing rules for one caller id at a
/// given time of day (defaults to now, UTC). Returns the first
/// matching rule, or `null` when nothing matches.
pub(crate) async fn evaluate_routing(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<EvaluateRoutingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let input = body.into_inner();
        let caller_id = require_user_id(&input.caller_id)?;
        let now_minutes = match input.now_minutes {
            Some(minutes) if (0..=1439).contains(&minutes) => minutes,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "now_minutes must be between 0 and 1439",
                ));
            }
            None => {
                let now = Utc::now();
                (now.hour() as i64) * 60 + (now.minute() as i64)
            }
        };
        let rules = state.active_routing_rules(&tenant_id);
        let matched = evaluate_routing_rules(&rules, &caller_id, now_minutes);
        Ok(json!({
            "caller_id": caller_id,
            "now_minutes": now_minutes,
            "matched_rule": matched,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
