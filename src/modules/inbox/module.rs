// Inbox - Module struct, route table and registration.

// Smart Inbox module - one queue unifying email, chat, tickets, and
// social mentions, with assignment, snooze, canned replies, SLA
// surfacing, per-agent read tracking, and explicit thread merging.
//
// Routes live under `/api/v1/inbox`:
//
//   Queue:
//     GET    /threads - unified queue (filter by channel, status,
//       assignee, priority, department_id, unread, overdue;
//       overdue threads first, then most recent activity).
//     POST   /threads - register a thread reference manually
//       (email / chat / ticket / social; the owning module keeps
//       the messages - this only stores the reference).
//     GET    /threads/{id}.
//     PATCH  /threads/{id} - status / priority / department.
//     GET    /stats - queue counts by status and channel, overdue
//       count, unassigned count, snoozed count.
//   Assignment / snooze / read:
//     POST   /threads/{id}/assign - assign to an agent.
//     POST   /threads/{id}/snooze - hide until a time, then reappear
//       (scheduler job `inbox.unsnooze`).
//     POST   /threads/{id}/unsnooze - bring a snoozed thread back.
//     POST   /threads/{id}/read - mark read for the caller.
//     POST   /threads/{id}/unread - mark unread for the caller.
//   Merge:
//     POST   /threads/{id}/merge - merge into another thread
//       (explicit only; same customer, multiple channels).
//     POST   /threads/{id}/unmerge - reverse a merge.
//   Reply:
//     POST   /threads/{id}/reply - request a reply from the owning
//       module (the inbox never sends mail or chat itself); an
//       optional canned reply id expands into the draft body.
//   Canned replies:
//     GET/POST /canned-replies - list (filter by department_id) /
//       create (per department).
//     GET/PATCH/DELETE /canned-replies/{id}.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory thread index, assignment history, canned replies, and
// per-agent read receipts; ingestion listeners that maintain the
// index from `ticket.created` / `ticket.updated` /
// `ticket.commented` / `ticket.assigned`, `chat.message` /
// `chat.session.created` / `chat.session.assigned`, and `mail.sent`
// payloads; SLA display copied from the tickets module's own SLA
// fields (`sla_resolution_due_at`, `sla_resolution_breached`) on
// the thread - the tickets module owns SLA computation, this module
// only surfaces it and sorts overdue threads first; assignment with
// a `notification.requested` to the assignee; snooze with a
// `scheduler.enqueue` attempt; reply as a fire-and-forget
// `inbox.reply.requested` event for the owning module. What is NOT
// built here: the message stores themselves (mail, chat, and
// tickets own their messages; a thread holds only a reference:
// source channel, source id, subject, participants, last activity,
// unread flag); actual sending of any reply (the owning module
// consumes `inbox.reply.requested`); inbound email ingestion from
// the mail module (no `mail.received` event exists in this tree -
// `mail.sent` is ingested defensively and skipped when its payload
// carries no message id, so email threads mostly arrive through
// the manual registration endpoint until the mail module emits an
// inbound event); social mention ingestion (no social module exists
// in this tree yet - social threads are registered manually with
// channel `social`); and automatic merging (merging is explicit
// only, by agent choice).
//
// Public-access note: there are no public routes in this module.
// Every route requires the caller headers and a capability check
// (`inbox.view` for reads, `inbox.manage` for writes); the API
// Manager default-deny still applies on top.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Tenancy: every handler resolves the tenant FIRST, before caller
// identity or any capability check (`require_tenant` in
// `auth.rs`, following the library module's pattern - see
// `unified_core::tenancy`). A request with no resolved tenant
// context is rejected (401); a resolved tenant id that is not a
// UUID is rejected (403); a verified caller company that does not
// match the resolved tenant's company is rejected (403). There is
// no fallback to the `public` schema and no default tenant.
// Threads, assignments and canned replies carry `tenant_id` +
// `company_id` from the resolved context at creation, and every
// read filters on them, so one tenant's queue is never visible to
// another; a cross-tenant id lookup answers 404, never leaking
// existence. Residual (integrator work, not closed here):
// event-ingested threads whose payload carries no `tenant_id`
// are stored unscoped (nil tenant) and are never served via the
// API until a tenant-scoped event refreshes them; the Diesel
// models / migration `2026-11-22-000054_inbox` (outside this
// module) still carry no `tenant_id` column, so durable
// per-tenant isolation lands with that migration change.
//
// Events emitted on the hook bus:
//   - `inbox.thread.assigned`, `inbox.thread.snoozed`,
//     `inbox.thread.read`, `inbox.thread.merged`,
//     `inbox.thread.unmerged`, `inbox.thread.created`,
//     `inbox.thread.updated`, `inbox.thread.unsnoozed`
//   - `inbox.reply.requested` (consumed by the owning module:
//     `{"thread_id", "source_channel", "source_id", "body",
//     "requested_by", "department_id", "assignee_id"}`)
//   - `notification.requested` (assignment notice, in the
//     notifications module's payload shape)
//   - `scheduler.enqueue` with `{"kind": "inbox.unsnooze",
//     "payload": {"thread_id", "tenant_id", "unsnooze_at"}}`
//     when a thread is snoozed.
// Events this module listens to:
//   - `ticket.created`, `ticket.updated`, `ticket.commented`,
//     `ticket.assigned` - upsert the ticket thread (full ticket
//     JSON payload: id, title, user_id, department_id, priority,
//     status, SLA due / breached flags).
//   - `chat.message` - upsert the chat thread for the channel
//     (payload: channel_id, message_id, author_id, body).
//   - `chat.session.created`, `chat.session.assigned` - upsert the
//     chat thread for the session (payload: session_id,
//     visitor_id, department_id when present).
//   - `mail.sent` - upsert an email thread when the payload
//     carries a message id (`id` / `message_id`); skipped
//     otherwise (see the HONESTY note above).
//   - `inbox.unsnooze_requested` - the event the scheduler emits
//     when an `inbox.unsnooze` job fires, following its naming
//     rule (`backup.run` -> `backup.run_requested`); clears the
//     snooze on the named thread. When the payload carries a
//     `tenant_id` it must match the thread's tenant, otherwise
//     the event is ignored.
//
// Scheduler note: `inbox.unsnooze` is in the scheduler's
// `KNOWN_KINDS` list and its dispatch arm emits
// `inbox.unsnooze_requested` with the job payload forwarded
// unchanged, so the queued job now reaches this module's
// listener (`on_unsnooze_requested`), which verifies the
// payload's `tenant_id` against the thread before clearing the
// snooze. While the scheduler did not know the kind, the thread
// recorded `snooze_scheduler_status: "declined"` instead of
// pretending it was scheduled; the snooze was still stored and
// the thread stayed hidden until `snoozed_until` passed (the
// queue list hides snoozed threads by default and re-shows them
// once the time passes, and `POST /threads/{id}/unsnooze`
// brings one back manually).

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::ai_capabilities::AI_CAPABILITIES;
use super::{CHAT_MESSAGE, CHAT_SESSION_ASSIGNED, CHAT_SESSION_CREATED, InboxState, MAIL_SENT,
    TICKET_ASSIGNED, TICKET_COMMENTED, TICKET_CREATED, TICKET_UPDATED, UNSNOOZE_REQUESTED,
    assign_thread, create_canned_reply, delete_canned_reply, get_canned_reply, get_thread,
    list_canned_replies, list_threads, mark_read, mark_unread, merge_thread, on_chat_message,
    on_chat_session, on_mail_sent, on_ticket_event, on_unsnooze_requested, queue_stats,
    register_thread, request_reply, snooze_thread, unmerge_thread, unsnooze_thread,
    update_canned_reply, update_thread};

// Module

pub struct InboxModule {
    state: InboxState,
}

impl InboxModule {
    pub fn new() -> Self {
        Self {
            state: InboxState::default(),
        }
    }
}

impl Default for InboxModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for InboxModule {
    fn id(&self) -> &'static str {
        "inbox"
    }

    fn name(&self) -> &'static str {
        "Smart Inbox"
    }

    fn description(&self) -> &'static str {
        "One unified queue over email, chat, tickets, and social mentions: a lightweight thread index (references only - the owning modules keep the messages), assignment, snooze with scheduler re-appearance, per-department canned replies, reply requests emitted to the owning module, SLA surfacing from tickets with overdue threads first, per-agent read tracking, and explicit thread merging."
    }

    fn requires(&self) -> &[&'static str] {
        &["tickets", "chat", "mail", "notifications", "scheduler", "roles", "users"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["tickets", "chat", "mail", "notifications", "scheduler", "roles", "users"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("inbox", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in [TICKET_CREATED, TICKET_UPDATED, TICKET_COMMENTED, TICKET_ASSIGNED] {
            let state = self.state.clone();
            bus.register(
                event,
                "inbox",
                Arc::new(move |ctx: &mut HookContext| on_ticket_event(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            CHAT_MESSAGE,
            "inbox",
            Arc::new(move |ctx: &mut HookContext| on_chat_message(state.clone(), ctx)),
        );
        for event in [CHAT_SESSION_CREATED, CHAT_SESSION_ASSIGNED] {
            let state = self.state.clone();
            bus.register(
                event,
                "inbox",
                Arc::new(move |ctx: &mut HookContext| on_chat_session(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            MAIL_SENT,
            "inbox",
            Arc::new(move |ctx: &mut HookContext| on_mail_sent(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            UNSNOOZE_REQUESTED,
            "inbox",
            Arc::new(move |ctx: &mut HookContext| on_unsnooze_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/inbox")
                .app_data(state)
                .route("/threads", web::get().to(list_threads))
                .route("/threads", web::post().to(register_thread))
                .route("/stats", web::get().to(queue_stats))
                .route("/threads/{id}", web::get().to(get_thread))
                .route("/threads/{id}", web::patch().to(update_thread))
                .route("/threads/{id}/assign", web::post().to(assign_thread))
                .route("/threads/{id}/snooze", web::post().to(snooze_thread))
                .route("/threads/{id}/unsnooze", web::post().to(unsnooze_thread))
                .route("/threads/{id}/read", web::post().to(mark_read))
                .route("/threads/{id}/unread", web::post().to(mark_unread))
                .route("/threads/{id}/merge", web::post().to(merge_thread))
                .route("/threads/{id}/unmerge", web::post().to(unmerge_thread))
                .route("/threads/{id}/reply", web::post().to(request_reply))
                .route("/canned-replies", web::get().to(list_canned_replies))
                .route("/canned-replies", web::post().to(create_canned_reply))
                .route("/canned-replies/{id}", web::get().to(get_canned_reply))
                .route("/canned-replies/{id}", web::patch().to(update_canned_reply))
                .route("/canned-replies/{id}", web::delete().to(delete_canned_reply)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/inbox/threads", owner: "inbox", description: "List the unified queue (paginated; filter by channel, status, assignee, priority, department_id, unread, overdue; overdue first, then most recent activity; snoozed threads hidden unless include_snoozed=true).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads", owner: "inbox", description: "Register a thread reference manually (email / chat / ticket / social; the owning module keeps the messages).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/inbox/stats", owner: "inbox", description: "Queue counts by status and channel, overdue count, unassigned count, snoozed count.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/inbox/threads/{id}", owner: "inbox", description: "Get one thread with per-caller read state, overdue / snoozed flags, merged thread ids, and assignment history.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/inbox/threads/{id}", owner: "inbox", description: "Update a thread's status, priority, department, or subject.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/assign", owner: "inbox", description: "Assign a thread to an agent (emits inbox.thread.assigned and notifies the assignee).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/snooze", owner: "inbox", description: "Snooze a thread until a time (emits inbox.thread.snoozed; queues an inbox.unsnooze scheduler job).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/unsnooze", owner: "inbox", description: "Bring a snoozed thread back into the queue now.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/read", owner: "inbox", description: "Mark a thread read for the caller (emits inbox.thread.read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/unread", owner: "inbox", description: "Mark a thread unread for the caller.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/merge", owner: "inbox", description: "Merge a thread into another thread (explicit only; the source thread is hidden from the queue).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/unmerge", owner: "inbox", description: "Reverse a merge, returning the thread to the queue.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/reply", owner: "inbox", description: "Request a reply from the owning module (emits inbox.reply.requested; the inbox never sends mail or chat itself). An optional canned reply id expands into the draft body.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/inbox/canned-replies", owner: "inbox", description: "List canned replies (paginated, filter by department_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/inbox/canned-replies", owner: "inbox", description: "Create a canned reply (per department; null department means every department).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/inbox/canned-replies/{id}", owner: "inbox", description: "Get one canned reply.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/inbox/canned-replies/{id}", owner: "inbox", description: "Update a canned reply.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/inbox/canned-replies/{id}", owner: "inbox", description: "Delete a canned reply.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(InboxModule::new()));
}
