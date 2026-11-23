// Inbox - Thread handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::auth::{require_tenant, tenant_visible};
use super::service::acquire_source_lock;
use super::{AssignRequest, CHANNELS, InboxAssignment, InboxState, InboxThread, MAX_BODY_LEN,
    MAX_ID_LEN, MAX_PARTICIPANTS, MAX_SUBJECT_LEN, MergeRequest, PERM_INBOX_MANAGE, PERM_INBOX_VIEW,
    RegisterThreadRequest, ReplyRequest, STATUSES, SnoozeRequest, UpdateThreadRequest,
    assignments_for, caller, created, err, find_thread_for_source, load_canned_reply, load_thread,
    ok, optional_text, paginate, priority_rank, require_permission, require_text, schedule_unsnooze,
    thread_view, valid_channel, valid_priority, valid_status};

pub(crate) async fn list_threads(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let params = query.into_inner();
        let channel_filter = params.filters.get("channel").cloned();
        let status_filter = params.filters.get("status").cloned();
        let assignee_filter = params.filters.get("assignee").cloned();
        let priority_filter = params.filters.get("priority").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let unread_filter = params.filters.get("unread").map(|raw| raw == "true");
        let overdue_filter = params.filters.get("overdue").map(|raw| raw == "true");
        let include_snoozed = params.filters.get("include_snoozed").map(|raw| raw == "true").unwrap_or(false);
        let include_merged = params.filters.get("merged").map(|raw| raw == "true").unwrap_or(false);
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                if !tenant_visible(thread.tenant_id, thread.company_id.as_deref(), &scope) {
                    return false;
                }
                if !include_merged && thread.merged_into.is_some() {
                    return false;
                }
                if !include_snoozed && thread.is_snoozed(now) {
                    return false;
                }
                if let Some(channel) = &channel_filter {
                    if thread.source_channel != *channel {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if thread.status != *status {
                        return false;
                    }
                }
                if let Some(assignee) = &assignee_filter {
                    if assignee == "unassigned" {
                        if thread.assignee_id.is_some() {
                            return false;
                        }
                    } else if thread.assignee_id.as_deref() != Some(assignee.as_str()) {
                        return false;
                    }
                }
                if let Some(priority) = &priority_filter {
                    if thread.priority != *priority {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if thread.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(want_unread) = unread_filter {
                    if state.unread_for(thread, &caller.actor) != want_unread {
                        return false;
                    }
                }
                if let Some(want_overdue) = overdue_filter {
                    if thread.is_overdue(now) != want_overdue {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        thread.subject,
                        thread.source_id,
                        thread.customer_id.clone().unwrap_or_default(),
                        thread.customer_label.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|thread| thread_view(&state, &thread, &caller.actor))
            .collect();
        // Default order: overdue threads first, then most recent
        // activity. An explicit ?sort= overrides it.
        let sort = params.sort.clone().unwrap_or_else(|| "overdue".to_string());
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
                "sla_due_at" => a
                    .get("sla_due_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("sla_due_at").and_then(Value::as_str)),
                "priority" => priority_rank(
                    a.get("priority").and_then(Value::as_str).unwrap_or("normal"),
                )
                .cmp(&priority_rank(
                    b.get("priority").and_then(Value::as_str).unwrap_or("normal"),
                )),
                "overdue" => {
                    let overdue_a = a.get("overdue").and_then(Value::as_bool).unwrap_or(false);
                    let overdue_b = b.get("overdue").and_then(Value::as_bool).unwrap_or(false);
                    overdue_b
                        .cmp(&overdue_a)
                        .then(
                            b.get("last_activity_at")
                                .and_then(Value::as_str)
                                .cmp(&a.get("last_activity_at").and_then(Value::as_str)),
                        )
                }
                _ => b
                    .get("last_activity_at")
                    .and_then(Value::as_str)
                    .cmp(&a.get("last_activity_at").and_then(Value::as_str)),
            };
            if sort == "overdue" || sort == "last_activity" {
                ordering
            } else if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn register_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    body: web::Json<RegisterThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let channel = valid_channel(&input.source_channel)?;
        let source_id = require_text(&input.source_id, "source_id", MAX_ID_LEN)?;
        let subject = require_text(&input.subject, "subject", MAX_SUBJECT_LEN)?;
        let mut participants: Vec<String> = Vec::new();
        for raw in &input.participants {
            if participants.len() >= MAX_PARTICIPANTS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_PARTICIPANTS} participants are allowed"
                )));
            }
            participants.push(require_text(raw, "participant", MAX_ID_LEN)?);
        }
        let status = match &input.status {
            Some(raw) => valid_status(raw)?,
            None => "open".to_string(),
        };
        let priority = match &input.priority {
            Some(raw) => valid_priority(raw)?,
            None => "normal".to_string(),
        };
        let customer_id = optional_text(&input.customer_id, "customer_id", MAX_ID_LEN)?;
        let customer_label = optional_text(&input.customer_label, "customer_label", MAX_ID_LEN)?;
        let department_id = optional_text(&input.department_id, "department_id", MAX_ID_LEN)?;
        let assignee_id = optional_text(&input.assignee_id, "assignee_id", MAX_ID_LEN)?;
        // The duplicate check and the insert run under the same
        // per-source lock as ingestion, so two concurrent
        // registrations for one source cannot both insert.
        let lock = acquire_source_lock(&state, &channel, &source_id);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if find_thread_for_source(&state, &channel, &source_id, scope.tenant_id).is_some() {
            return Err(UnifiedError::conflict(format!(
                "A live thread already exists for {channel} source \"{source_id}\""
            )));
        }
        let now = Utc::now();
        let thread = InboxThread {
            id: Uuid::now_v7(),
            tenant_id: scope.tenant_id,
            company_id: scope.company_id.clone(),
            source_channel: channel.clone(),
            source_id: source_id.clone(),
            subject,
            customer_id,
            customer_label,
            participants,
            department_id,
            assignee_id,
            status,
            priority,
            unread: true,
            snoozed_until: None,
            snooze_scheduler_status: "not_scheduled".to_string(),
            sla_due_at: input.sla_due_at,
            sla_breached: false,
            merged_into: None,
            last_activity_at: now,
            created_at: now,
            updated_at: now,
        };
        let id = thread.id;
        state.threads.insert(id, thread.clone());
        // Release the source lock before emitting: a listener that
        // re-enters ingestion for this source must not block on
        // this non-reentrant mutex.
        drop(_guard);
        state.emit(
            "inbox.thread.created",
            json!({
                "thread_id": id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "source_channel": channel,
                "source_id": source_id,
                "subject": thread.subject,
                "created_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn queue_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let now = Utc::now();
        let live: Vec<InboxThread> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                thread.merged_into.is_none()
                    && tenant_visible(thread.tenant_id, thread.company_id.as_deref(), &scope)
            })
            .collect();
        let mut by_status = serde_json::Map::new();
        for status in STATUSES {
            let count = live.iter().filter(|thread| thread.status == status).count();
            by_status.insert(status.to_string(), Value::from(count as u64));
        }
        let mut by_channel = serde_json::Map::new();
        for channel in CHANNELS {
            let count = live.iter().filter(|thread| thread.source_channel == channel).count();
            by_channel.insert(channel.to_string(), Value::from(count as u64));
        }
        Ok(json!({
            "total": live.len() as u64,
            "by_status": by_status,
            "by_channel": by_channel,
            "overdue": live.iter().filter(|thread| thread.is_overdue(now)).count() as u64,
            "unassigned": live.iter().filter(|thread| thread.assignee_id.is_none() && (thread.status == "open" || thread.status == "pending")).count() as u64,
            "snoozed": live.iter().filter(|thread| thread.is_snoozed(now)).count() as u64,
            "unread_for_caller": live.iter().filter(|thread| state.unread_for(thread, &caller.actor)).count() as u64,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let thread = load_thread(&state, &path, &scope)?;
        let mut view = thread_view(&state, &thread, &caller.actor);
        if let Value::Object(map) = &mut view {
            map.insert(
                "assignments".to_string(),
                serde_json::to_value(assignments_for(&state, thread.id, &scope))
                    .unwrap_or(Value::Null),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let mut thread = load_thread(&state, &path, &scope)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be updated; unmerge it first",
            ));
        }
        if let Some(raw) = &input.status {
            thread.status = valid_status(raw)?;
        }
        if let Some(raw) = &input.priority {
            thread.priority = valid_priority(raw)?;
        }
        if let Some(department) = &input.department_id {
            thread.department_id = match department {
                Some(raw) => Some(require_text(raw, "department_id", MAX_ID_LEN)?),
                None => None,
            };
        }
        if let Some(subject) = &input.subject {
            thread.subject = require_text(subject, "subject", MAX_SUBJECT_LEN)?;
        }
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.updated",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "status": thread.status,
                "priority": thread.priority,
                "updated_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn assign_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let assignee = require_text(&body.assignee_id, "assignee_id", MAX_ID_LEN)?;
        let mut thread = load_thread(&state, &path, &scope)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be assigned; unmerge it first",
            ));
        }
        let now = Utc::now();
        for mut entry in state.assignments.iter_mut() {
            if entry.value().thread_id == thread.id && entry.value().active {
                entry.value_mut().active = false;
                entry.value_mut().updated_at = now;
            }
        }
        let assignment = InboxAssignment {
            id: Uuid::now_v7(),
            tenant_id: scope.tenant_id,
            company_id: scope.company_id.clone(),
            thread_id: thread.id,
            assignee_id: assignee.clone(),
            assigned_by: caller.actor.clone(),
            active: true,
            assigned_at: now,
            created_at: now,
            updated_at: now,
        };
        state.assignments.insert(assignment.id, assignment);
        thread.assignee_id = Some(assignee.clone());
        thread.updated_at = now;
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.assigned",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "source_channel": thread.source_channel,
                "source_id": thread.source_id,
                "assignee_id": assignee,
                "assigned_by": caller.actor,
            }),
        );
        state.notify(
            "inbox.thread.assigned",
            &assignee,
            &format!("Thread assigned: {}", thread.subject),
            &format!(
                "You were assigned the {} thread \"{}\" ({}).",
                thread.source_channel, thread.subject, thread.source_id
            ),
            Some(format!("/api/v1/inbox/threads/{}", thread.id)),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn snooze_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SnoozeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut thread = load_thread(&state, &path, &scope)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be snoozed; unmerge it first",
            ));
        }
        if body.snoozed_until <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "snoozed_until must be in the future",
            ));
        }
        thread.snoozed_until = Some(body.snoozed_until);
        thread.snooze_scheduler_status = schedule_unsnooze(&state, &mut thread);
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.snoozed",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "snoozed_until": body.snoozed_until.to_rfc3339(),
                "snoozed_by": caller.actor,
                "scheduler_status": thread.snooze_scheduler_status,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unsnooze_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut thread = load_thread(&state, &path, &scope)?;
        thread.snoozed_until = None;
        thread.snooze_scheduler_status = "not_scheduled".to_string();
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.unsnoozed",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "reason": "manual",
                "unsnoozed_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_read(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let mut thread = load_thread(&state, &path, &scope)?;
        state.reads.insert(
            InboxState::read_key(thread.id, &caller.actor),
            Utc::now(),
        );
        thread.unread = false;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.read",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "user_id": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_unread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let mut thread = load_thread(&state, &path, &scope)?;
        state
            .reads
            .remove(&InboxState::read_key(thread.id, &caller.actor));
        thread.unread = true;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn merge_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MergeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut source = load_thread(&state, &path, &scope)?;
        let target = load_thread(&state, &body.target_thread_id.to_string(), &scope)?;
        if source.id == target.id {
            return Err(UnifiedError::bad_request("A thread cannot be merged into itself"));
        }
        if source.merged_into.is_some() {
            return Err(UnifiedError::conflict("This thread is already merged into another thread"));
        }
        if target.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "The target thread is itself merged; merge into the live target instead",
            ));
        }
        source.merged_into = Some(target.id);
        source.status = "closed".to_string();
        source.updated_at = Utc::now();
        state.threads.insert(source.id, source.clone());
        // The target inherits the source's participants and takes the
        // latest activity, so the merged view stays complete.
        let mut target = target;
        for participant in &source.participants {
            if !target.participants.contains(participant) {
                target.participants.push(participant.clone());
            }
        }
        target.participants.truncate(MAX_PARTICIPANTS);
        if source.last_activity_at > target.last_activity_at {
            target.last_activity_at = source.last_activity_at;
        }
        target.unread = true;
        target.updated_at = Utc::now();
        state.threads.insert(target.id, target.clone());
        state.emit(
            "inbox.thread.merged",
            json!({
                "thread_id": source.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "target_thread_id": target.id.to_string(),
                "merged_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &target, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unmerge_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut thread = load_thread(&state, &path, &scope)?;
        let Some(target_id) = thread.merged_into else {
            return Err(UnifiedError::conflict("This thread is not merged"));
        };
        thread.merged_into = None;
        thread.status = "open".to_string();
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.unmerged",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "target_thread_id": target_id.to_string(),
                "unmerged_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn request_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReplyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let thread = load_thread(&state, &path, &scope)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be replied to; reply on the live target thread instead",
            ));
        }
        let mut reply_body: Option<String> = match &body.body {
            Some(raw) => Some(require_text(raw, "body", MAX_BODY_LEN)?),
            None => None,
        };
        if reply_body.is_none() {
            let Some(canned_id) = body.canned_reply_id else {
                return Err(UnifiedError::bad_request(
                    "Provide either body or canned_reply_id",
                ));
            };
            let canned = load_canned_reply(&state, &canned_id.to_string(), &scope)?;
            let expanded = canned
                .body
                .replace("{customer}", thread.customer_label.as_deref().unwrap_or(""))
                .replace("{subject}", &thread.subject)
                .replace("{thread_id}", &thread.id.to_string());
            reply_body = Some(require_text(&expanded, "body", MAX_BODY_LEN)?);
        }
        let reply_body = reply_body.expect("reply body resolved above");
        // The inbox never sends mail or chat itself: the owning
        // module consumes this event and sends through its own
        // channel. The reply is a request, not a delivery.
        state.emit(
            "inbox.reply.requested",
            json!({
                "thread_id": thread.id.to_string(),
                "tenant_id": scope.tenant_id.to_string(),
                "source_channel": thread.source_channel,
                "source_id": thread.source_id,
                "body": reply_body,
                "requested_by": caller.actor,
                "department_id": thread.department_id,
                "assignee_id": thread.assignee_id,
            }),
        );
        Ok(json!({
            "requested": true,
            "thread_id": thread.id.to_string(),
            "source_channel": thread.source_channel,
            "source_id": thread.source_id,
            "requested_by": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
