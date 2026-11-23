// Inbox - Hook handlers for tickets, chat and mail.

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{InboxState, PRIORITIES, opt_datetime, opt_string, ticket_status_to_inbox, upsert_thread};

/// Tenant ownership carried by an ingested event payload, when
/// the emitting module includes it. Source modules do not all
/// emit `tenant_id` yet: when it is absent (or not a UUID) the
/// thread is created unscoped (nil tenant id) and is never served
/// via the API until a tenant-scoped event refreshes it — see the
/// module header. The company id is taken from the payload when
/// present, otherwise `None`.
fn payload_tenant(data: &Value) -> (Uuid, Option<String>) {
    let tenant_id = data
        .get("tenant_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .unwrap_or(Uuid::nil());
    let company_id = opt_string(data, "company_id");
    (tenant_id, company_id)
}

/// Ingest a ticket event payload (full ticket JSON from the tickets
/// module: id, title, user_id, department_id, priority, status,
/// sla_resolution_due_at, sla_resolution_breached, updated_at).
pub(crate) fn on_ticket_event(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(source_id) = opt_string(&data, "id") else {
        return;
    };
    let subject = opt_string(&data, "title").unwrap_or_else(|| format!("Ticket {source_id}"));
    let status = opt_string(&data, "status")
        .map(|value| ticket_status_to_inbox(&value).to_string())
        .unwrap_or_else(|| "open".to_string());
    let priority = opt_string(&data, "priority")
        .filter(|value| PRIORITIES.contains(&value.as_str()));
    let customer_id = opt_string(&data, "user_id");
    let participants = customer_id.clone().into_iter().collect();
    let activity_at = opt_datetime(&data, "updated_at").unwrap_or_else(Utc::now);
    let (tenant_id, company_id) = payload_tenant(&data);
    upsert_thread(
        &state,
        "ticket",
        &source_id,
        &subject,
        customer_id,
        None,
        participants,
        opt_string(&data, "department_id"),
        priority,
        &status,
        opt_datetime(&data, "sla_resolution_due_at"),
        data.get("sla_resolution_breached")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        activity_at,
        tenant_id,
        company_id,
    );
}

/// Ingest a chat message event (payload: channel_id, message_id,
/// author_id, body). The subject is the first line of the latest
/// message when the thread is new; later messages only refresh
/// activity and participants.
pub(crate) fn on_chat_message(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(channel_id) = opt_string(&data, "channel_id") else {
        return;
    };
    let body = opt_string(&data, "body").unwrap_or_default();
    let first_line = body.lines().next().unwrap_or("").trim();
    let subject = if first_line.is_empty() {
        format!("Chat channel {channel_id}")
    } else {
        first_line.to_string()
    };
    let author = opt_string(&data, "author_id");
    let participants = author.into_iter().collect();
    let (tenant_id, company_id) = payload_tenant(&data);
    upsert_thread(
        &state,
        "chat",
        &channel_id,
        &subject,
        None,
        None,
        participants,
        None,
        None,
        "open",
        None,
        false,
        Utc::now(),
        tenant_id,
        company_id,
    );
}

/// Ingest a chat session event (payload: session_id, visitor_id,
/// department_id when present).
pub(crate) fn on_chat_session(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(session_id) = opt_string(&data, "session_id") else {
        return;
    };
    let visitor = opt_string(&data, "visitor_id");
    let subject = match &visitor {
        Some(visitor) => format!("Chat session with {visitor}"),
        None => format!("Chat session {session_id}"),
    };
    let participants = visitor.into_iter().collect();
    let (tenant_id, company_id) = payload_tenant(&data);
    upsert_thread(
        &state,
        "chat",
        &session_id,
        &subject,
        None,
        None,
        participants,
        opt_string(&data, "department_id"),
        None,
        "open",
        None,
        false,
        Utc::now(),
        tenant_id,
        company_id,
    );
}

/// Ingest an outbound mail event. The mail module emits no inbound
/// event in this tree, so this only indexes mail the application
/// sent; the payload shape is read defensively (`id` /
/// `message_id`, `to`, `subject`) and the event is skipped when no
/// message id is present.
pub(crate) fn on_mail_sent(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(source_id) = opt_string(&data, "id").or_else(|| opt_string(&data, "message_id"))
    else {
        return;
    };
    let subject = opt_string(&data, "subject")
        .unwrap_or_else(|| format!("Email {source_id}"));
    let recipient = opt_string(&data, "to");
    let participants = recipient.clone().into_iter().collect();
    let (tenant_id, company_id) = payload_tenant(&data);
    upsert_thread(
        &state,
        "email",
        &source_id,
        &subject,
        None,
        recipient,
        participants,
        None,
        None,
        "open",
        None,
        false,
        Utc::now(),
        tenant_id,
        company_id,
    );
}

/// Clear the snooze on the named thread when the scheduler fires
/// the `inbox.unsnooze` job. Idempotent: a thread that is not
/// snoozed, or is snoozed until later, is left alone. When the
/// event payload carries a `tenant_id` (the scheduler echoes the
/// one `schedule_unsnooze` queued), it must match the thread's
/// tenant — a mismatched or unparsable tenant id is ignored, so a
/// job for one tenant can never unsnooze another tenant's thread.
/// A payload without `tenant_id` (scheduler arms that predate the
/// tenant field) is still honoured: the scheduler is a trusted
/// internal emitter and the job was created by this module.
pub(crate) fn on_unsnooze_requested(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(raw_id) = opt_string(&data, "thread_id") else {
        return;
    };
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return;
    };
    let Some(mut thread) = state.threads.get_mut(&id) else {
        return;
    };
    if let Some(raw_tenant) = opt_string(&data, "tenant_id") {
        match Uuid::parse_str(&raw_tenant) {
            Ok(payload_tenant) if payload_tenant == thread.tenant_id => {}
            _ => return,
        }
    }
    if thread.snoozed_until.is_none() {
        return;
    }
    thread.snoozed_until = None;
    thread.snooze_scheduler_status = "not_scheduled".to_string();
    thread.updated_at = Utc::now();
    let thread_id = thread.id;
    drop(thread);
    state.emit(
        "inbox.thread.unsnoozed",
        json!({ "thread_id": thread_id.to_string(), "reason": "scheduler" }),
    );
}

#[cfg(test)]
mod tests {
    use super::super::{find_thread_for_source, InboxThread};
    use super::*;
    use unified_core::hooks::HookContext;
    use chrono::Duration;

    fn snoozed_thread(state: &InboxState, tenant_id: Uuid) -> Uuid {
        let now = Utc::now();
        let thread = InboxThread {
            id: Uuid::now_v7(),
            tenant_id,
            company_id: Some(tenant_id.to_string()),
            source_channel: "ticket".to_string(),
            source_id: "T-1".to_string(),
            subject: "Snoozed".to_string(),
            customer_id: None,
            customer_label: None,
            participants: vec![],
            department_id: None,
            assignee_id: None,
            status: "open".to_string(),
            priority: "normal".to_string(),
            unread: true,
            snoozed_until: Some(now + Duration::hours(1)),
            snooze_scheduler_status: "queued".to_string(),
            sla_due_at: None,
            sla_breached: false,
            merged_into: None,
            last_activity_at: now,
            created_at: now,
            updated_at: now,
        };
        let id = thread.id;
        state.threads.insert(id, thread);
        id
    }

    fn unsnooze_ctx(thread_id: Uuid, tenant_id: Option<Uuid>) -> HookContext {
        let mut payload = json!({ "thread_id": thread_id.to_string() });
        if let Some(tenant) = tenant_id {
            payload["tenant_id"] = json!(tenant.to_string());
        }
        HookContext::new("inbox.unsnooze_requested", payload)
    }

    #[test]
    fn unsnooze_clears_snooze_without_tenant_in_payload() {
        let state = InboxState::default();
        let id = snoozed_thread(&state, Uuid::now_v7());
        let mut ctx = unsnooze_ctx(id, None);
        on_unsnooze_requested(state.clone(), &mut ctx);
        let thread = state.threads.get(&id).expect("thread kept");
        assert!(thread.snoozed_until.is_none());
        assert_eq!(thread.snooze_scheduler_status, "not_scheduled");
    }

    #[test]
    fn unsnooze_clears_snooze_with_matching_tenant() {
        let state = InboxState::default();
        let tenant_id = Uuid::now_v7();
        let id = snoozed_thread(&state, tenant_id);
        let mut ctx = unsnooze_ctx(id, Some(tenant_id));
        on_unsnooze_requested(state.clone(), &mut ctx);
        assert!(state
            .threads
            .get(&id)
            .expect("thread kept")
            .snoozed_until
            .is_none());
    }

    #[test]
    fn unsnooze_ignores_mismatched_tenant() {
        let state = InboxState::default();
        let tenant_id = Uuid::now_v7();
        let id = snoozed_thread(&state, tenant_id);
        let mut ctx = unsnooze_ctx(id, Some(Uuid::now_v7()));
        on_unsnooze_requested(state.clone(), &mut ctx);
        assert!(state
            .threads
            .get(&id)
            .expect("thread kept")
            .snoozed_until
            .is_some());
    }

    #[test]
    fn unsnooze_ignores_unknown_thread() {
        let state = InboxState::default();
        let mut ctx = unsnooze_ctx(Uuid::now_v7(), None);
        on_unsnooze_requested(state.clone(), &mut ctx);
    }

    #[test]
    fn ticket_event_without_tenant_creates_unscoped_thread() {
        let state = InboxState::default();
        let mut ctx = HookContext::new(
            "ticket.created",
            json!({ "id": "T-9", "title": "Help", "status": "open" }),
        );
        on_ticket_event(state.clone(), &mut ctx);
        let found = find_thread_for_source(&state, "ticket", "T-9", Uuid::nil());
        assert!(
            found.is_some(),
            "unscoped ingestion still indexes the thread"
        );
        assert_eq!(found.expect("thread").tenant_id, Uuid::nil());
    }

    #[test]
    fn ticket_event_with_tenant_scopes_thread() {
        let state = InboxState::default();
        let tenant_id = Uuid::now_v7();
        let mut ctx = HookContext::new(
            "ticket.created",
            json!({ "id": "T-10", "title": "Help", "status": "open", "tenant_id": tenant_id.to_string() }),
        );
        on_ticket_event(state.clone(), &mut ctx);
        let found = find_thread_for_source(&state, "ticket", "T-10", tenant_id);
        assert!(
            found.is_some(),
            "tenant-scoped ingestion indexes under that tenant"
        );
        assert!(find_thread_for_source(&state, "ticket", "T-10", Uuid::nil()).is_none());
    }
}
