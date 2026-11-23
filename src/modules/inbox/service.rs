// Inbox - Thread store helpers and unsnooze scheduling.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::tenancy::TenantContext;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::auth::tenant_visible;
use super::{InboxAssignment, InboxCannedReply, InboxState, InboxThread, MAX_PARTICIPANTS,
    MAX_SUBJECT_LEN, SCHEDULER_KIND_UNSNOOZE, parse_uuid};

/// Load a thread only when it belongs to the caller's tenant
/// scope. A thread of another tenant (or an unscoped one) is
/// reported as not found, so its existence never leaks across
/// tenants.
pub(crate) fn load_thread(
    state: &InboxState,
    raw_id: &str,
    scope: &TenantContext,
) -> UnifiedResult<InboxThread> {
    let id = parse_uuid(raw_id, "thread")?;
    let thread = state
        .threads
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Thread not found: {id}")))?;
    if !tenant_visible(thread.tenant_id, thread.company_id.as_deref(), scope) {
        return Err(UnifiedError::not_found(format!("Thread not found: {id}")));
    }
    Ok(thread)
}

pub(crate) fn load_canned_reply(
    state: &InboxState,
    raw_id: &str,
    scope: &TenantContext,
) -> UnifiedResult<InboxCannedReply> {
    let id = parse_uuid(raw_id, "canned reply")?;
    let reply = state
        .canned_replies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Canned reply not found: {id}")))?;
    if !tenant_visible(reply.tenant_id, reply.company_id.as_deref(), scope) {
        return Err(UnifiedError::not_found(format!(
            "Canned reply not found: {id}"
        )));
    }
    Ok(reply)
}

/// Find the live (not merged) thread for one source reference
/// within one tenant. Threads are partitioned by tenant: the same
/// source reference in another tenant is a different thread and
/// is never returned here.
pub(crate) fn find_thread_for_source(
    state: &InboxState,
    channel: &str,
    source_id: &str,
    tenant_id: Uuid,
) -> Option<InboxThread> {
    state
        .threads
        .iter()
        .map(|entry| entry.value().clone())
        .find(|thread| {
            thread.tenant_id == tenant_id
                && thread.source_channel == channel
                && thread.source_id == source_id
                && thread.merged_into.is_none()
        })
}

/// The mutex serializing ingestion for one source reference.
/// Held for the whole find-then-insert in `upsert_thread` (and
/// the duplicate check in `register_thread`), so concurrent
/// events for the same source cannot both miss and insert.
/// The caller holds the returned `Arc` (and its lock guard) for
/// the duration of the critical section.
pub(crate) fn acquire_source_lock(
    state: &InboxState,
    channel: &str,
    source_id: &str,
) -> Arc<Mutex<()>> {
    state
        .upsert_locks
        .entry((channel.to_string(), source_id.to_string()))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .value()
        .clone()
}

/// Serialize a thread for the API, with per-caller read state and
/// SLA / snooze flags computed at read time.
pub(crate) fn thread_view(state: &InboxState, thread: &InboxThread, caller_id: &str) -> Value {
    let now = Utc::now();
    let mut value = serde_json::to_value(thread).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "unread".to_string(),
            Value::Bool(state.unread_for(thread, caller_id)),
        );
        map.insert(
            "overdue".to_string(),
            Value::Bool(thread.is_overdue(now)),
        );
        map.insert(
            "snoozed".to_string(),
            Value::Bool(thread.is_snoozed(now)),
        );
        let merged_children: Vec<String> = state
            .threads
            .iter()
            .filter(|entry| {
                entry.value().merged_into == Some(thread.id)
                    && entry.value().tenant_id == thread.tenant_id
            })
            .map(|entry| entry.key().to_string())
            .collect();
        map.insert(
            "merged_thread_ids".to_string(),
            serde_json::to_value(merged_children).unwrap_or(Value::Null),
        );
    }
    value
}

pub(crate) fn assignments_for(
    state: &InboxState,
    thread_id: Uuid,
    scope: &TenantContext,
) -> Vec<InboxAssignment> {
    let mut items: Vec<InboxAssignment> = state
        .assignments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|assignment| {
            assignment.thread_id == thread_id
                && tenant_visible(
                    assignment.tenant_id,
                    assignment.company_id.as_deref(),
                    scope,
                )
        })
        .collect();
    items.sort_by_key(|assignment| std::cmp::Reverse(assignment.assigned_at));
    items
}

// ---------------------------------------------------------------------------
// Ingestion — maintain the thread index from other modules' events
// ---------------------------------------------------------------------------

/// Create or refresh the thread for one source reference. Never
/// duplicates message stores: only the reference row is kept. New
/// activity marks the thread unread and refreshes `last_activity_at`.
/// `sla_due_at` / `sla_breached` are only meaningful for ticket
/// threads; other channels pass `None` / `false` and leave a stored
/// SLA value untouched when the caller passes `None`.
///
/// Atomic per source: the whole find-then-insert runs under the
/// source lock (`acquire_source_lock`), so concurrent events for
/// the same source reference yield exactly one live thread.
/// `tenant_id` / `company_id` stamp the new thread's ownership;
/// event ingestion passes the payload's tenant when present, or
/// the nil UUID (unscoped, never served via the API) when the
/// payload carries none — see the module header.
#[allow(clippy::too_many_arguments)]
pub(crate) fn upsert_thread(
    state: &InboxState,
    channel: &str,
    source_id: &str,
    subject: &str,
    customer_id: Option<String>,
    customer_label: Option<String>,
    participants: Vec<String>,
    department_id: Option<String>,
    priority: Option<String>,
    status: &str,
    sla_due_at: Option<DateTime<Utc>>,
    sla_breached: bool,
    activity_at: DateTime<Utc>,
    tenant_id: Uuid,
    company_id: Option<String>,
) -> Uuid {
    // The lock guards only the find-then-insert: it is released
    // before the `inbox.thread.created` emit below, so a listener
    // that re-enters ingestion for the same source cannot deadlock
    // on this non-reentrant mutex.
    let (id, created) = {
        let lock = acquire_source_lock(state, channel, source_id);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        upsert_thread_locked(
            state,
            channel,
            source_id,
            subject,
            customer_id,
            customer_label,
            participants,
            department_id,
            priority,
            status,
            sla_due_at,
            sla_breached,
            activity_at,
            tenant_id,
            company_id,
        )
    };
    if let Some(thread) = created {
        state.emit(
            "inbox.thread.created",
            json!({
                "thread_id": id.to_string(),
                "tenant_id": tenant_id.to_string(),
                "source_channel": channel,
                "source_id": source_id,
                "subject": thread.subject,
            }),
        );
    }
    id
}

/// The locked find-then-insert half of `upsert_thread`: callers
/// must hold the source lock. Returns the thread id and, when a
/// new thread was inserted, a clone of it (for the post-lock
/// `inbox.thread.created` emit).
#[allow(clippy::too_many_arguments)]
fn upsert_thread_locked(
    state: &InboxState,
    channel: &str,
    source_id: &str,
    subject: &str,
    customer_id: Option<String>,
    customer_label: Option<String>,
    participants: Vec<String>,
    department_id: Option<String>,
    priority: Option<String>,
    status: &str,
    sla_due_at: Option<DateTime<Utc>>,
    sla_breached: bool,
    activity_at: DateTime<Utc>,
    tenant_id: Uuid,
    company_id: Option<String>,
) -> (Uuid, Option<InboxThread>) {
    let now = Utc::now();
    let subject = subject.trim();
    let subject = if subject.is_empty() {
        format!("{channel} thread {source_id}")
    } else if subject.len() > MAX_SUBJECT_LEN {
        subject.chars().take(MAX_SUBJECT_LEN).collect()
    } else {
        subject.to_string()
    };
    let mut participants: Vec<String> = participants
        .into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    participants.truncate(MAX_PARTICIPANTS);
    if let Some(existing) = find_thread_for_source(state, channel, source_id, tenant_id) {
        let mut updated = existing.clone();
        updated.subject = subject;
        if let Some(customer) = customer_id {
            updated.customer_id = Some(customer);
        }
        if let Some(label) = customer_label {
            updated.customer_label = Some(label);
        }
        for participant in participants {
            if !updated.participants.contains(&participant) {
                updated.participants.push(participant);
            }
        }
        updated.participants.truncate(MAX_PARTICIPANTS);
        if let Some(department) = department_id {
            updated.department_id = Some(department);
        }
        if let Some(priority) = priority {
            updated.priority = priority;
        }
        updated.status = status.to_string();
        if sla_due_at.is_some() {
            updated.sla_due_at = sla_due_at;
        }
        updated.sla_breached = sla_breached;
        updated.unread = true;
        updated.last_activity_at = activity_at.max(updated.last_activity_at);
        updated.updated_at = now;
        let id = updated.id;
        state.threads.insert(id, updated);
        (id, None)
    } else {
        let thread = InboxThread {
            id: Uuid::now_v7(),
            tenant_id,
            company_id,
            source_channel: channel.to_string(),
            source_id: source_id.to_string(),
            subject,
            customer_id,
            customer_label,
            participants,
            department_id,
            assignee_id: None,
            status: status.to_string(),
            priority: priority.unwrap_or_else(|| "normal".to_string()),
            unread: true,
            snoozed_until: None,
            snooze_scheduler_status: "not_scheduled".to_string(),
            sla_due_at,
            sla_breached,
            merged_into: None,
            last_activity_at: activity_at,
            created_at: now,
            updated_at: now,
        };
        let id = thread.id;
        state.threads.insert(id, thread.clone());
        (id, Some(thread))
    }
}

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn opt_datetime(data: &Value, key: &str) -> Option<DateTime<Utc>> {
    data.get(key)
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc))
}

/// Queue the `inbox.unsnooze` scheduler job for a snoozed thread.
/// Mirrors the events module's `schedule_reminder`: the scheduler
/// answers `{"queued": true, "job_id": ...}` when it knows the
/// kind (`inbox.unsnooze` is in `KNOWN_KINDS`, with a dispatch arm
/// emitting `inbox.unsnooze_requested`), and `{"queued": false,
/// "error": ...}` if the kind is ever removed. The snooze is
/// stored either way; the returned status records the outcome
/// honestly (`queued` / `declined`). The payload carries the
/// thread's tenant id so the scheduler can echo it back in
/// `inbox.unsnooze_requested` and the listener can verify the job
/// belongs to that tenant before clearing the snooze.
pub(crate) fn schedule_unsnooze(state: &InboxState, thread: &mut InboxThread) -> String {
    let Some(bus) = state.bus() else {
        return "declined".to_string();
    };
    let Some(unsnooze_at) = thread.snoozed_until else {
        return "not_scheduled".to_string();
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": SCHEDULER_KIND_UNSNOOZE,
            "payload": {
                "thread_id": thread.id.to_string(),
                "tenant_id": thread.tenant_id.to_string(),
                "unsnooze_at": unsnooze_at.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            "queued".to_string()
        }
        _ => "declined".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::tenancy;

    fn scope_for(tenant_id: Uuid) -> TenantContext {
        TenantContext {
            tenant_id,
            schema_name: tenancy::schema_name(tenant_id),
            company_id: Some(tenant_id.to_string()),
            site_id: None,
        }
    }

    fn upsert_for(state: &InboxState, tenant_id: Uuid) -> Uuid {
        upsert_thread(
            state,
            "ticket",
            "SRC-1",
            "Subject",
            None,
            None,
            vec![],
            None,
            None,
            "open",
            None,
            false,
            Utc::now(),
            tenant_id,
            Some(tenant_id.to_string()),
        )
    }

    #[test]
    fn load_thread_rejects_other_tenant() {
        let state = InboxState::default();
        let tenant_a = Uuid::now_v7();
        let tenant_b = Uuid::now_v7();
        let id = upsert_for(&state, tenant_a);
        assert!(load_thread(&state, &id.to_string(), &scope_for(tenant_a)).is_ok());
        assert!(load_thread(&state, &id.to_string(), &scope_for(tenant_b)).is_err());
    }

    #[test]
    fn load_thread_rejects_unscoped_thread() {
        let state = InboxState::default();
        let id = upsert_for(&state, Uuid::nil());
        assert!(load_thread(&state, &id.to_string(), &scope_for(Uuid::now_v7())).is_err());
    }

    #[test]
    fn same_source_in_two_tenants_is_two_threads() {
        let state = InboxState::default();
        let tenant_a = Uuid::now_v7();
        let tenant_b = Uuid::now_v7();
        let id_a = upsert_for(&state, tenant_a);
        let id_b = upsert_for(&state, tenant_b);
        assert_ne!(id_a, id_b);
        assert_eq!(
            find_thread_for_source(&state, "ticket", "SRC-1", tenant_a).map(|thread| thread.id),
            Some(id_a)
        );
        assert_eq!(
            find_thread_for_source(&state, "ticket", "SRC-1", tenant_b).map(|thread| thread.id),
            Some(id_b)
        );
    }

    #[test]
    fn concurrent_upsert_yields_one_live_thread() {
        let state = InboxState::default();
        let tenant_id = Uuid::now_v7();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        upsert_thread(
                            &state,
                            "ticket",
                            "SRC-RACE",
                            "Subject",
                            None,
                            None,
                            vec![],
                            None,
                            None,
                            "open",
                            None,
                            false,
                            Utc::now(),
                            tenant_id,
                            Some(tenant_id.to_string()),
                        );
                    }
                });
            }
        });
        let live: Vec<InboxThread> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                thread.source_channel == "ticket"
                    && thread.source_id == "SRC-RACE"
                    && thread.merged_into.is_none()
            })
            .collect();
        assert_eq!(
            live.len(),
            1,
            "concurrent upserts must not duplicate the thread"
        );
    }

    #[test]
    fn assignments_for_filters_by_tenant() {
        let state = InboxState::default();
        let tenant_a = Uuid::now_v7();
        let tenant_b = Uuid::now_v7();
        let thread_id = upsert_for(&state, tenant_a);
        let now = Utc::now();
        let assignment_id = Uuid::now_v7();
        state.assignments.insert(
            assignment_id,
            InboxAssignment {
                id: assignment_id,
                tenant_id: tenant_b,
                company_id: Some(tenant_b.to_string()),
                thread_id,
                assignee_id: "agent-1".to_string(),
                assigned_by: "admin".to_string(),
                active: true,
                assigned_at: now,
                created_at: now,
                updated_at: now,
            },
        );
        assert!(assignments_for(&state, thread_id, &scope_for(tenant_a)).is_empty());
        assert_eq!(
            assignments_for(&state, thread_id, &scope_for(tenant_b)).len(),
            1
        );
    }
}
